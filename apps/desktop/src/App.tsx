import { useEffect, useRef, useState } from 'react';
import { connection, EMPTY_STATUS, onConnectionStatus, type ConnectionStatus } from './lib/connection';
import { app, inTauri } from './lib/player';
import { RoomView } from './RoomView';
import { RoomEntryDialog } from './components/RoomDialogs';
import { onRoomState, room } from './lib/room';
import { ConnectionSetup } from './screens/ConnectionSetup';
import { Home } from './screens/Home';
import { prefs, Settings, type Section } from './screens/Settings';
import { UpdateNotice } from './components/UpdateNotice';
import { updates } from './lib/updates';

type View = 'home' | 'setup' | 'settings' | 'room';

export function App() {
  const [status, setStatus] = useState<ConnectionStatus>(EMPTY_STATUS);
  const [view, setView] = useState<View>('home');
  const [loaded, setLoaded] = useState(!inTauri());
  const [entry, setEntry] = useState<'create' | 'join' | null>(null);
  // Every room gets a fresh room view (no UI state carries over from the previous room).
  const [roomSession, setRoomSession] = useState(0);
  // Settings can open from a room (error actions) and come back to it.
  const [settingsAt, setSettingsAt] = useState<Section>('Connection');
  const viewRef = useRef<View>('home');
  viewRef.current = view;
  const openSettings = (s: Section) => {
    setSettingsAt(s);
    setView('settings');
  };
  // While the Create/Join dialog is open it decides when to enter the room (it may show a
  // notice first).
  const entryOpen = useRef(false);
  entryOpen.current = entry !== null;

  // Debug builds: optional scripted two-window scenario (scripts/dev/two-windows.ps1 -Scenario).
  useEffect(() => {
    // Test builds (build-release.ps1 -TestBuild) include the scenarios too: release-mode labs.
    if (import.meta.env.DEV || import.meta.env.VITE_SYNCWATCH_TEST_BUILD === '1') void import('./dev/scenario').then((m) => m.maybeRunScenario());
  }, []);

  // Already in a room (window reload, dev auto-room): show it.
  useEffect(() => {
    void room.state().then((s) => {
      setRoomSession(s.sessionId);
      if (s.active) setView('room');
    });
    return onRoomState((s) => {
      if (s.sessionId) setRoomSession(s.sessionId);
      if (s.active && !entryOpen.current && viewRef.current !== 'settings') setView('room');
    });
  }, []);

  useEffect(() => {
    void connection.status().then((s) => {
      if (s) {
        setStatus(s);
        // First run: open the setup screen.
        if (!s.configured) setView('setup');
      }
      setLoaded(true);
    });
    return onConnectionStatus(setStatus);
  }, []);

  // Outside the room there is no video: report an empty center rect so the mpv window hides,
  // and tell the native side the UI has painted (no white flash).
  useEffect(() => {
    if (!loaded) return;
    if (view !== 'room') void app.setLayout({ x: 0, y: 0, w: 0, h: 0 }, [], false);
    requestAnimationFrame(() => requestAnimationFrame(() => void app.ready()));
  }, [loaded, view]);

  // Updates (ADR 0016): one check after the first paint, never blocking. The offer shows on the
  // home screen only, never inside a room.
  const [skipVersion, setSkipVersion] = useState('');
  const checked = useRef(false);
  useEffect(() => {
    if (!loaded || checked.current || !inTauri()) return;
    checked.current = true;
    const t = window.setTimeout(() => {
      void prefs
        .get()
        .then((p) => {
          setSkipVersion(p.skipVersion);
          if (p.checkUpdates) void updates.check().catch(() => {});
        })
        .catch(() => {});
    }, 1500);
    return () => window.clearTimeout(t);
  }, [loaded]);
  const skip = (v: string) => {
    setSkipVersion(v);
    void prefs
      .get()
      .then((p) => prefs.set({ ...p, skipVersion: v }))
      .catch(() => {});
  };

  const refresh = () => void connection.status().then((s) => s && setStatus(s));

  if (!loaded) return <div className="h-full bg-bg" />;
  switch (view) {
    case 'setup':
      return (
        <ConnectionSetup
          status={status}
          onDone={() => {
            refresh();
            setView('home');
          }}
          onBack={() => setView('home')}
        />
      );
    case 'settings':
      return (
        <Settings
          key={settingsAt}
          status={status}
          initial={settingsAt}
          onReplace={() => setView('setup')}
          onBack={() => void room.state().then((s) => setView(s.active ? 'room' : 'home'))}
        />
      );
    case 'room':
      return <RoomView key={roomSession} onLeave={() => setView('home')} onOpenSettings={openSettings} />;
    default:
      return (
        <>
          <Home
            status={status}
            onSetup={() => setView('setup')}
            onSettings={() => openSettings('Connection')}
            onCreate={() => status.canConnect && setEntry('create')}
            onJoin={() => status.canConnect && setEntry('join')}
          />
          <UpdateNotice skipVersion={skipVersion} onSkip={skip} />
          {entry && (
            <RoomEntryDialog
              mode={entry}
              onClose={() => setEntry(null)}
              onConnectionSettings={() => {
                setEntry(null);
                openSettings('Connection');
              }}
              onEntered={() => {
                setEntry(null);
                setView('room');
              }}
            />
          )}
        </>
      );
  }
}
