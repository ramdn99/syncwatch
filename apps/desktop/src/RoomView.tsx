import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import logoUrl from './assets/logo-lockup.svg';
import { ChatOverlay } from './components/ChatOverlay';
import { ChangeSourceDialog } from './components/ChangeSourceDialog';
import { ChatPanel } from './components/ChatPanel';
import { HostControls } from './components/HostControls';
import { MembersPanel } from './components/MembersPanel';
import { VideoArea } from './components/VideoArea';
import { computeColumns, dragWidth, loadPrefs, savePrefs, toPhysical, type LayoutPrefs } from './lib/layout';
import { app, inTauri, onPlayerEvent, player, type PlaybackState, type Rect, type Track } from './lib/player';
import { mapKey } from './lib/shortcuts';
import { mockChat, mockMembers, type MockChatMessage } from './mock/data';

const EMPTY_STATE: PlaybackState = {
  paused: true,
  timePos: null,
  duration: null,
  speed: 1,
  pausedForCache: false,
  eofReached: false,
  volume: 100,
  mute: false,
  fileLoaded: false,
  cacheAhead: null,
};

const VIDEO_EXT = ['mkv', 'mp4', 'webm', 'avi', 'mov', 'm4v', 'ts', 'm2ts', 'wmv', 'flv', 'ogv', 'mpg', 'mpeg'];
const SUB_EXT = ['ass', 'ssa', 'srt', 'vtt', 'sub', 'sup', 'idx'];
const AUDIO_EXT = ['mka', 'aac', 'ac3', 'eac3', 'dts', 'flac', 'mp3', 'opus', 'ogg', 'wav', 'm4a', 'thd'];

export function RoomView({ onLeave }: { onLeave: () => void }) {
  const [prefs, setPrefs] = useState<LayoutPrefs>(loadPrefs);
  const [fullscreen, setFullscreen] = useState(false);
  const [winWidth, setWinWidth] = useState(() => window.innerWidth);
  const [viewAsHost, setViewAsHost] = useState(true);
  const [state, setState] = useState<PlaybackState>(EMPTY_STATE);
  const [tracks, setTracks] = useState<Track[]>([]);
  const [mediaKey, setMediaKey] = useState('none');
  const [chat, setChat] = useState<MockChatMessage[]>(mockChat);
  const [overlayInput, setOverlayInput] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [changeSource, setChangeSource] = useState<{ path?: string } | null>(null);

  const centerRef = useRef<HTMLDivElement>(null);
  const leftRef = useRef<HTMLDivElement>(null);
  const rightRef = useRef<HTMLDivElement>(null);
  const chatInputRef = useRef<HTMLTextAreaElement>(null);
  const stateRef = useRef(state);
  stateRef.current = state;

  const cols = useMemo(() => computeColumns(winWidth, prefs, fullscreen), [winWidth, prefs, fullscreen]);
  useEffect(() => savePrefs(prefs), [prefs]);

  // --- report the CENTER rect to the native side (mpv window follows it) ----------------
  const report = useCallback(() => {
    const el = centerRef.current;
    if (!el) return;
    const dpr = window.devicePixelRatio || 1;
    const rect = (e: HTMLElement | null): Rect | null => (e && e.offsetWidth > 0 ? toPhysical(e.getBoundingClientRect(), dpr) : null);
    const center = rect(el);
    if (!center) return;
    const columns = [rect(leftRef.current), rect(rightRef.current)].filter((r): r is Rect => r !== null);
    void app.setLayout(center, columns, fullscreen);
  }, [fullscreen]);

  useLayoutEffect(() => {
    report();
  }, [report, cols]);

  useEffect(() => {
    const el = centerRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => report());
    ro.observe(el);
    const onResize = () => {
      setWinWidth(window.innerWidth);
      report();
    };
    window.addEventListener('resize', onResize);
    // DPI change (monitor move): devicePixelRatio changes without a resize of the element.
    let mq = window.matchMedia(`(resolution: ${window.devicePixelRatio}dppx)`);
    const onDpr = () => {
      report();
      mq.removeEventListener('change', onDpr);
      mq = window.matchMedia(`(resolution: ${window.devicePixelRatio}dppx)`);
      mq.addEventListener('change', onDpr);
    };
    mq.addEventListener('change', onDpr);
    return () => {
      ro.disconnect();
      window.removeEventListener('resize', onResize);
      mq.removeEventListener('change', onDpr);
    };
  }, [report]);

  // --- first paint -> show window (no white flash) --------------------------------------
  useEffect(() => {
    requestAnimationFrame(() => requestAnimationFrame(() => void app.ready()));
    void app.snapshot().then((s) => {
      if (!s) return;
      setState(s.state);
      setTracks(s.tracks);
      if (!s.dev) setViewAsHost(true);
    });
  }, []);

  // --- player events --------------------------------------------------------------------
  useEffect(
    () =>
      onPlayerEvent((e) => {
        switch (e.type) {
          case 'fileLoaded':
            setTracks(e.tracks);
            setMediaKey(`m${Date.now()}`);
            break;
          case 'tracksChanged':
            setTracks(e.tracks);
            break;
          case 'state': {
            const { type: _t, ...s } = e;
            setState(s);
            break;
          }
          case 'endFile':
            if (e.reason === 'error') setError('This file could not be played. It may be damaged or in an unsupported format.');
            break;
          default:
            break;
        }
      }),
    [],
  );

  const run = (p: Promise<unknown>) => p.catch((err: unknown) => setError(String(err)));

  const toggleFullscreen = useCallback(
    (on?: boolean) => {
      const next = on ?? !fullscreen;
      setFullscreen(next);
      setOverlayInput(false);
      void app.setFullscreen(next);
    },
    [fullscreen],
  );

  const openFile = async () => {
    if (!inTauri()) return;
    const { open } = await import('@tauri-apps/plugin-dialog');
    const path = await open({ multiple: false, directory: false, filters: [{ name: 'Video', extensions: VIDEO_EXT }, { name: 'All files', extensions: ['*'] }] });
    if (typeof path === 'string') void run(player.open(path));
  };

  const addTrack = async (kind: 'audio' | 'sub') => {
    if (!inTauri()) return;
    const { open } = await import('@tauri-apps/plugin-dialog');
    const path = await open({
      multiple: false,
      filters: [{ name: kind === 'sub' ? 'Subtitles' : 'Audio', extensions: kind === 'sub' ? SUB_EXT : AUDIO_EXT }],
    });
    if (typeof path === 'string') void run(player.addExternal(kind, path));
  };

  // Drag-and-drop a video onto the window.
  useEffect(() => {
    if (!inTauri()) return;
    let un: (() => void) | undefined;
    void import('@tauri-apps/api/webview').then(({ getCurrentWebview }) =>
      getCurrentWebview()
        .onDragDropEvent((ev) => {
          if (ev.payload.type !== 'drop') return;
          const p = ev.payload.paths[0];
          // Dropping a file changes the room's source: confirm in the dialog first.
          if (p) setChangeSource((cur) => cur ?? { path: p });
        })
        .then((u) => (un = u)),
    );
    return () => un?.();
  }, []);

  // Leaving the room hides the video surface (an empty center rect hides the mpv window).
  useEffect(() => () => void app.setLayout({ x: 0, y: 0, w: 0, h: 0 }, [], false), []);

  const sendChat = (text: string) => {
    const t = stateRef.current;
    setChat((c) => [
      ...c,
      {
        id: `local-${Date.now()}`,
        senderId: 'm1',
        senderName: mockMembers[0]?.name ?? 'You',
        isHost: viewAsHost,
        colorIndex: 0,
        text,
        wallTime: new Date().toTimeString().slice(0, 5),
        mediaTimeMs: t.fileLoaded && t.timePos != null ? Math.round(t.timePos * 1000) : null,
      },
    ]);
  };

  const cycleTrack = (kind: 'audio' | 'sub') => {
    const list = tracks.filter((t) => t.kind === kind);
    if (list.length === 0) return;
    const i = list.findIndex((t) => t.selected);
    if (kind === 'sub' && i === list.length - 1) return void run(player.selectTrack('sub', { mode: 'off' }));
    const next = list[(i + 1) % list.length];
    if (next) void run(player.selectTrack(kind, { mode: 'id', id: next.id }));
  };

  // --- shortcuts ------------------------------------------------------------------------
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const action = mapKey(e, viewAsHost);
      if (!action) return;
      e.preventDefault();
      const s = stateRef.current;
      switch (action) {
        case 'togglePause':
          void run(player.setPaused(!s.paused));
          break;
        case 'seekBack':
          void run(player.seekBy(-5));
          break;
        case 'seekForward':
          void run(player.seekBy(5));
          break;
        case 'volumeUp':
          void run(player.setVolume(Math.min(100, s.volume + 5)));
          break;
        case 'volumeDown':
          void run(player.setVolume(Math.max(0, s.volume - 5)));
          break;
        case 'toggleMute':
          void run(player.setMute(!s.mute));
          break;
        case 'toggleFullscreen':
          toggleFullscreen();
          break;
        case 'exitFullscreen':
          if (fullscreen) toggleFullscreen(false);
          break;
        case 'toggleLeft':
          if (!fullscreen) setPrefs((p) => ({ ...p, leftCollapsed: !p.leftCollapsed }));
          break;
        case 'toggleRight':
          if (!fullscreen) setPrefs((p) => ({ ...p, rightCollapsed: !p.rightCollapsed }));
          break;
        case 'focusChat':
          if (fullscreen) setOverlayInput(true);
          else {
            setPrefs((p) => ({ ...p, leftCollapsed: false }));
            requestAnimationFrame(() => chatInputRef.current?.focus());
          }
          break;
        case 'cycleAudio':
          cycleTrack('audio');
          break;
        case 'cycleSub':
          cycleTrack('sub');
          break;
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });

  // In fullscreen, Enter opens the overlay chat input.
  useEffect(() => {
    if (!fullscreen) return;
    const onEnter = (e: KeyboardEvent) => {
      if (e.key === 'Enter' && !overlayInput && (e.target as HTMLElement).tagName !== 'INPUT') {
        e.preventDefault();
        setOverlayInput(true);
      }
    };
    window.addEventListener('keydown', onEnter);
    return () => window.removeEventListener('keydown', onEnter);
  }, [fullscreen, overlayInput]);

  // --- resizers -------------------------------------------------------------------------
  const startDrag = (side: 'left' | 'right') => (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    const startX = e.clientX;
    const startW = side === 'left' ? cols.left : cols.right;
    const other = side === 'left' ? cols.right : cols.left;
    const target = e.currentTarget;
    target.setPointerCapture(e.pointerId);
    const move = (ev: PointerEvent) => {
      const w = dragWidth(side, startW, ev.clientX - startX, window.innerWidth, other);
      setPrefs((p) => (side === 'left' ? { ...p, leftWidth: w } : { ...p, rightWidth: w }));
    };
    const up = () => {
      target.removeEventListener('pointermove', move);
      target.removeEventListener('pointerup', up);
    };
    target.addEventListener('pointermove', move);
    target.addEventListener('pointerup', up);
  };

  const markers = chat.filter((m) => m.mediaTimeMs != null && !m.divider).map((m) => (m.mediaTimeMs ?? 0) / 1000);

  return (
    <div className="flex h-full flex-col">
      {!fullscreen && (
        <header className="flex h-10 shrink-0 items-center gap-4 border-b border-border bg-bg px-3">
          <img src={logoUrl} alt="SyncWatch" className="h-5" draggable={false} />
          <span className="font-brand text-sm tracking-[0.2em] text-muted" title="Room code (mock)">
            K7QM-4XRP
          </span>
          <button type="button" className="rounded px-2 py-0.5 text-xs text-muted hover:bg-surface hover:text-text" onClick={onLeave}>
            Leave
          </button>
          {import.meta.env.DEV && (
            <div className="ml-auto flex items-center gap-2 text-xs text-muted" aria-label="Developer view switch">
              View as
              {(['host', 'member'] as const).map((v) => (
                <button
                  key={v}
                  type="button"
                  aria-pressed={viewAsHost === (v === 'host')}
                  className={`rounded px-2 py-0.5 ${viewAsHost === (v === 'host') ? 'bg-accent text-on-accent' : 'bg-elevated hover:text-text'}`}
                  onClick={() => setViewAsHost(v === 'host')}
                >
                  {v}
                </button>
              ))}
            </div>
          )}
        </header>
      )}
      {error && (
        <div role="alert" className="flex items-center gap-3 bg-danger/15 px-3 py-1.5 text-sm text-danger">
          <span className="flex-1">{error}</span>
          <button type="button" className="text-text underline" onClick={() => setError(null)}>
            Dismiss
          </button>
        </div>
      )}
      <main className="flex min-h-0 flex-1">
        {!fullscreen && cols.left > 0 && (
          <>
            <div ref={leftRef} style={{ width: cols.left }} className="h-full shrink-0" data-testid="col-left">
              <ChatPanel ref={chatInputRef} messages={chat} onCollapse={() => setPrefs((p) => ({ ...p, leftCollapsed: true }))} onChipClick={(s) => void run(player.seek(s))} onSend={sendChat} />
            </div>
            <div className="w-1 shrink-0 cursor-col-resize bg-border hover:bg-accent" role="separator" aria-orientation="vertical" aria-label="Resize chat" onPointerDown={startDrag('left')} data-testid="resizer-left" />
          </>
        )}
        {!fullscreen && cols.left === 0 && <CollapsedRail side="left" label="Show chat ([)" onClick={() => setPrefs((p) => ({ ...p, leftCollapsed: false }))} />}
        <VideoArea
          ref={centerRef}
          state={state}
          fullscreen={fullscreen}
          markers={markers}
          onTogglePause={() => void run(player.setPaused(!state.paused))}
          onSeek={(s) => void run(player.seek(s))}
          onVolume={(v) => void run(player.setVolume(v))}
          onToggleMute={() => void run(player.setMute(!state.mute))}
          onToggleFullscreen={() => toggleFullscreen()}
          onOpenFile={() => void openFile()}
        >
          {fullscreen && <ChatOverlay messages={chat} inputOpen={overlayInput} onCloseInput={() => setOverlayInput(false)} onSend={sendChat} />}
        </VideoArea>
        {!fullscreen && cols.right === 0 && <CollapsedRail side="right" label="Show members (])" onClick={() => setPrefs((p) => ({ ...p, rightCollapsed: false }))} />}
        {!fullscreen && cols.right > 0 && (
          <>
            <div className="w-1 shrink-0 cursor-col-resize bg-border hover:bg-accent" role="separator" aria-orientation="vertical" aria-label="Resize members" onPointerDown={startDrag('right')} data-testid="resizer-right" />
            <div ref={rightRef} style={{ width: cols.right }} className="flex h-full shrink-0 flex-col overflow-y-auto bg-surface" data-testid="col-right">
              <Section title="Members" count={mockMembers.length} collapsed={prefs.membersCollapsed} onToggle={() => setPrefs((p) => ({ ...p, membersCollapsed: !p.membersCollapsed }))} onCollapseColumn={() => setPrefs((p) => ({ ...p, rightCollapsed: true }))}>
                <MembersPanel members={mockMembers} viewerIsHost={viewAsHost} />
              </Section>
              <Section title={viewAsHost ? 'Host controls' : 'Room'} collapsed={prefs.hostCollapsed} onToggle={() => setPrefs((p) => ({ ...p, hostCollapsed: !p.hostCollapsed }))}>
                <HostControls isHost={viewAsHost} tracks={tracks} mediaKey={mediaKey} onChangeSource={() => setChangeSource({})} onSelectTrack={(k, s) => void run(player.selectTrack(k, s))} onAddTrack={(k) => void addTrack(k)} />
              </Section>
            </div>
          </>
        )}
      </main>
      {changeSource && (
        <ChangeSourceDialog
          initialPath={changeSource.path}
          onClose={() => setChangeSource(null)}
          onLocalChosen={(p) => void run(player.open(p))}
        />
      )}
    </div>
  );
}

function Section(props: { title: string; count?: number; collapsed: boolean; onToggle: () => void; onCollapseColumn?: () => void; children: React.ReactNode }) {
  return (
    <section className="border-b border-border">
      <div className="flex items-center gap-1 pr-2">
        <button type="button" className="flex flex-1 items-center gap-2 px-3 py-2 text-left" aria-expanded={!props.collapsed} onClick={props.onToggle}>
          <span className="text-muted" aria-hidden>
            {props.collapsed ? '▸' : '▾'}
          </span>
          <h2 className="font-brand text-sm font-bold tracking-wide">{props.title}</h2>
          {props.count != null && <span className="tabular text-xs text-muted">{props.count}</span>}
        </button>
        {props.onCollapseColumn && (
          <button type="button" className="rounded px-2 text-muted hover:text-text" aria-label="Collapse side panel (])" title="Collapse side panel (])" onClick={props.onCollapseColumn}>
            »
          </button>
        )}
      </div>
      {!props.collapsed && props.children}
    </section>
  );
}

function CollapsedRail({ side, label, onClick }: { side: 'left' | 'right'; label: string; onClick: () => void }) {
  return (
    <button type="button" aria-label={label} title={label} onClick={onClick} className="flex h-full w-6 shrink-0 items-start justify-center bg-surface pt-2 text-muted hover:text-text" data-testid={`rail-${side}`}>
      {side === 'left' ? '»' : '«'}
    </button>
  );
}
