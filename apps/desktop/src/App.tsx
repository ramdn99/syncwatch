import { useEffect, useState } from 'react';
import { connection, EMPTY_STATUS, onConnectionStatus, type ConnectionStatus } from './lib/connection';
import { app, inTauri } from './lib/player';
import { RoomView } from './RoomView';
import { ConnectionSetup } from './screens/ConnectionSetup';
import { Home } from './screens/Home';
import { SettingsConnection } from './screens/SettingsConnection';

type View = 'home' | 'setup' | 'settings' | 'room';

export function App() {
  const [status, setStatus] = useState<ConnectionStatus>(EMPTY_STATUS);
  const [view, setView] = useState<View>('home');
  const [loaded, setLoaded] = useState(!inTauri());

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
      return <SettingsConnection status={status} onReplace={() => setView('setup')} onBack={() => setView('home')} />;
    case 'room':
      return <RoomView onLeave={() => setView('home')} />;
    default:
      return (
        <Home
          status={status}
          onSetup={() => setView('setup')}
          onSettings={() => setView('settings')}
          onCreate={() => status.canConnect && setView('room')}
        />
      );
  }
}
