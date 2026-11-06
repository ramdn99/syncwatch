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
import { MissingFilePanel, PasswordPrompt } from './components/RoomDialogs';
import { DiagnosticsDialog, type UiErrorEntry } from './components/Diagnostics';
import { EMPTY_ROOM, endText, formatCode, friendlyError, onRoomState, PASSWORD_IGNORED_TEXT, room, type RoomAction, type RoomState } from './lib/room';
import { chatLines, markerSeconds } from './lib/chat';
import type { TrackSelection } from './lib/player';

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
  const [rs, setRs] = useState<RoomState>(EMPTY_ROOM);
  const [toasts, setToasts] = useState<{ id: number; text: string }[]>([]);
  const [dismissedPanel, setDismissedPanel] = useState(0);
  const [diagOpen, setDiagOpen] = useState(false);
  const [uiErrors, setUiErrors] = useState<UiErrorEntry[]>([]);
  const [ignoredDismissed, setIgnoredDismissed] = useState(false);
  const [noLimit, setNoLimit] = useState(false);
  const [relayedAsk, setRelayedAsk] = useState<{ kind: 'audio' | 'sub'; path: string; members: string[]; size: number } | null>(null);
  const [overlayInput, setOverlayInput] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [changeSource, setChangeSource] = useState<{ path?: string } | null>(null);

  const centerRef = useRef<HTMLDivElement>(null);
  const leftRef = useRef<HTMLDivElement>(null);
  const rightRef = useRef<HTMLDivElement>(null);
  const chatInputRef = useRef<HTMLTextAreaElement>(null);
  const stateRef = useRef(state);
  stateRef.current = state;
  const panelOpenRef = useRef(false);
  const chooseFileRef = useRef<((p: string) => void) | null>(null);

  // --- room state (owned by Rust; pushed as snapshots) -----------------------------------
  useEffect(() => {
    void room.state().then(setRs);
    return onRoomState((s) => {
      setRs(s);
      if (s.toasts.length) {
        const now = Date.now();
        setToasts((t) => [...t, ...s.toasts.map((text, i) => ({ id: now + i, text }))].slice(-4));
      }
    });
  }, []);
  useEffect(() => {
    if (toasts.length === 0) return;
    const t = window.setTimeout(() => setToasts((x) => x.slice(1)), 5000);
    return () => window.clearTimeout(t);
  }, [toasts]);
  // Leaving on purpose returns home right away.
  useEffect(() => {
    if (rs.ended?.kind === 'left') onLeave();
  }, [rs.ended, onLeave]);

  const inRoom = rs.active || rs.ended != null;
  const isHost = inRoom ? rs.isHost : viewAsHost;
  const durationMs = rs.mediaDurationMs ?? (state.duration != null ? Math.round(state.duration * 1000) : null);
  // Chips: stamped by the host from the room timeline; seekable only for the current media and
  // for people allowed to seek.
  const chat = useMemo(() => chatLines(rs.chat, rs, durationMs), [rs, durationMs]);
  const positionMs = state.fileLoaded && state.timePos != null ? Math.round(state.timePos * 1000) : null;
  // Every failed command shows a friendly message and keeps the real error for Diagnostics.
  const reportError = (e: unknown) => {
    const f = friendlyError(e);
    setError(f.text);
    setUiErrors((l) => [...l.slice(-49), { at: Date.now(), text: f.detail }]);
  };
  const act = (a: RoomAction) => void room.action(a).catch(reportError);
  // Playback controls: in a room they are REQUESTS (the host validates and everyone, the
  // requester included, applies the host's command); outside a room they drive the player.
  const ctl = {
    togglePause: () => (rs.active ? act({ type: state.paused ? 'play' : 'pause' }) : void run(player.setPaused(!state.paused))),
    seekTo: (s: number) => (rs.active ? act({ type: 'seek', ms: Math.max(0, Math.round(s * 1000)) }) : void run(player.seek(s))),
    seekBy: (d: number) =>
      rs.active
        ? act({ type: 'seek', ms: Math.max(0, Math.round(((stateRef.current.timePos ?? 0) + d) * 1000)) })
        : void run(player.seekBy(d)),
    selectTrack: (k: 'audio' | 'sub', sel: TrackSelection) => {
      if (!rs.active) return void run(player.selectTrack(k, sel));
      if (!rs.isHost) return setError('Only the host changes audio and subtitle tracks.');
      void room.selectTrack(k, sel.mode === 'id' ? sel.id : null).catch(reportError);
    },
  };
  const verifying = rs.fileStatus === 'verifying';
  // Open until the member picks a file (Rust closes it for good), or dismissed with Later.
  const showPanel = rs.missingFile != null && rs.missingFile.panelOpen && rs.missingFile.seq !== dismissedPanel;
  const leave = () => {
    if (rs.active) void room.leave().catch(() => onLeave());
    else onLeave();
  };

  panelOpenRef.current = showPanel;
  chooseFileRef.current = (p: string) => void chooseMyFile(p);

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

  const run = (p: Promise<unknown>) => p.catch((err: unknown) => reportError(err));

  const toggleFullscreen = useCallback(
    (on?: boolean) => {
      const next = on ?? !fullscreen;
      setFullscreen(next);
      setOverlayInput(false);
      void app.setFullscreen(next);
    },
    [fullscreen],
  );

  const chooseMyFile = async (dropped?: string) => {
    if (!inTauri()) return;
    let path = dropped ?? null;
    if (!path) {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const p = await open({ multiple: false, directory: false, filters: [{ name: 'Video', extensions: VIDEO_EXT }, { name: 'All files', extensions: ['*'] }] });
      path = typeof p === 'string' ? p : null;
    }
    if (!path) return;
    void room.chooseFile(path).catch(reportError);
  };

  const openFile = async () => {
    if (!inTauri()) return;
    if (rs.active) {
      // In a room, opening a file changes the room's source (confirmed in the dialog).
      setChangeSource({});
      return;
    }
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
    if (typeof path !== 'string') return;
    if (!rs.active) return void run(player.addExternal(kind, path));
    if (!rs.isHost) return setError('Only the host adds audio or subtitle files.');
    await addExternal(kind, path, 'ask');
  };
  const addExternal = async (kind: 'audio' | 'sub', path: string, policy: 'ask' | 'all' | 'skipRelayed') => {
    try {
      const out = await room.addExternal(kind, path, noLimit, policy);
      if (!out.added) setRelayedAsk({ kind, path, members: out.relayedMembers, size: out.size });
      else if (out.convertedFrom) setToasts((x) => [...x, { id: Date.now(), text: `Subtitle converted to UTF-8 from ${out.convertedFrom}.` }]);
    } catch (e) {
      reportError(e);
    }
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
          if (!p) return;
          // While the Choose-my-file panel is open, a dropped file is the member's own copy.
          if (panelOpenRef.current) return void chooseFileRef.current?.(p);
          // Otherwise dropping a file changes the room's source: confirm in the dialog first.
          setChangeSource((cur) => cur ?? { path: p });
        })
        .then((u) => (un = u)),
    );
    return () => un?.();
  }, []);

  // Leaving the room hides the video surface (an empty center rect hides the mpv window).
  useEffect(() => () => void app.setLayout({ x: 0, y: 0, w: 0, h: 0 }, [], false), []);

  const sendChat = (text: string) => {
    if (!rs.active) return setError('Chat works inside a room.');
    act({ type: 'chat', clientId: Date.now() % 2_000_000_000, text });
  };
  const chatDisabled = !rs.active
    ? null
    : !rs.chatConfig.enabled && !rs.isHost
      ? 'The host turned chat off.'
      : !rs.myPermissions.canChat && !rs.isHost
        ? 'The host has not allowed you to chat.'
        : null;

  const cycleTrack = (kind: 'audio' | 'sub') => {
    const list = tracks.filter((t) => t.kind === kind);
    if (list.length === 0) return;
    const i = list.findIndex((t) => t.selected);
    if (kind === 'sub' && i === list.length - 1) return ctl.selectTrack('sub', { mode: 'off' });
    const next = list[(i + 1) % list.length];
    if (next) ctl.selectTrack(kind, { mode: 'id', id: next.id });
  };

  // --- shortcuts ------------------------------------------------------------------------
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const action = mapKey(e, isHost);
      if (!action) return;
      e.preventDefault();
      const s = stateRef.current;
      switch (action) {
        case 'togglePause':
          ctl.togglePause();
          break;
        case 'seekBack':
          ctl.seekBy(-5);
          break;
        case 'seekForward':
          ctl.seekBy(5);
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

  const markers = markerSeconds(chat);

  return (
    <div className="flex h-full flex-col">
      {!fullscreen && (
        <header className="flex h-10 shrink-0 items-center gap-4 border-b border-border bg-bg px-3">
          <img src={logoUrl} alt="SyncWatch" className="h-5" draggable={false} />
          {rs.roomCode && (
            <button
              type="button"
              className="font-brand text-sm tracking-[0.2em] text-text hover:text-accent"
              title="Room code: click to copy"
              onClick={() => void navigator.clipboard?.writeText(formatCode(rs.roomCode))}
            >
              {formatCode(rs.roomCode)}
            </button>
          )}
          {rs.active && rs.hasPassword && <span className="text-xs text-muted">Password protected</span>}
          {rs.active && rs.locked && <span className="text-xs text-warning">Locked</span>}
          {rs.active && rs.phase !== 'inRoom' && rs.phase !== 'reconnecting' && <span className="text-xs text-muted">Connecting to the room...</span>}
          {rs.hostWords && !rs.isHost && (
            <span className="text-xs text-muted" title="Read these words to the host: they must match what the host sees next to your name.">
              Verify with the host: <span className="font-brand text-text">{rs.hostWords.join(' ')}</span>
            </span>
          )}
          {inRoom && (
            <button type="button" className="rounded px-2 py-0.5 text-xs text-muted hover:bg-surface hover:text-text" onClick={() => setDiagOpen(true)}>
              Diagnostics
            </button>
          )}
          <button type="button" className="rounded px-2 py-0.5 text-xs text-muted hover:bg-surface hover:text-text" onClick={leave}>
            {rs.active && rs.isHost ? 'Close room' : 'Leave'}
          </button>
          {import.meta.env.DEV && !inRoom && (
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
      {rs.active && rs.phase === 'reconnecting' && !rs.isHost && (
        <div role="status" className="bg-warning/15 px-3 py-1.5 text-sm text-warning">
          Connection interrupted. Reconnecting...
        </div>
      )}
      {rs.active && rs.passwordIgnored && !ignoredDismissed && (
        <div role="status" className="flex items-center gap-3 bg-info/15 px-3 py-1.5 text-sm text-info">
          <span className="flex-1">{PASSWORD_IGNORED_TEXT}</span>
          <button type="button" className="text-text underline" onClick={() => setIgnoredDismissed(true)}>
            OK
          </button>
        </div>
      )}
      {rs.active && rs.fileStatus === 'mismatch' && (
        <div role="alert" className="flex flex-wrap items-center gap-3 bg-warning/15 px-3 py-1.5 text-sm text-warning">
          <span className="flex-1">Your file is not exactly the same as the host's. Playback stays in sync, but scenes may not line up.</span>
          <button type="button" className="text-text underline" onClick={() => act({ type: 'proceedWithMismatch' })}>
            Watch anyway
          </button>
          <button type="button" className="text-text underline" onClick={() => void chooseMyFile()}>
            Choose a different file
          </button>
        </div>
      )}
      {rs.active && rs.gate.active && (
        <div role="status" className="flex flex-wrap items-center gap-3 bg-info/15 px-3 py-1.5 text-sm text-info" data-testid="gate">
          <span className="flex-1">
            Waiting for {rs.gate.waiting.length ? rs.gate.waiting.map((w) => `${w.name} (${w.reason})`).join(', ') : 'everyone'}...
          </span>
          {rs.isHost && (
            <button type="button" className="text-text underline" onClick={() => act({ type: 'resumeWithoutWaiting' })}>
              Resume without them
            </button>
          )}
        </div>
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
              <ChatPanel
                ref={chatInputRef}
                messages={chat}
                onCollapse={() => setPrefs((p) => ({ ...p, leftCollapsed: true }))}
                positionMs={positionMs}
                onChipClick={(s) => ctl.seekTo(s)}
                onSend={sendChat}
                onDelete={rs.active && rs.isHost ? (id) => act({ type: 'deleteChat', id: Number(id) }) : undefined}
                disabledReason={chatDisabled}
              />
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
          onTogglePause={ctl.togglePause}
          onSeek={(s) => ctl.seekTo(s)}
          onVolume={(v) => void run(player.setVolume(v))}
          onToggleMute={() => void run(player.setMute(!state.mute))}
          onToggleFullscreen={() => toggleFullscreen()}
          onOpenFile={() => void openFile()}
          emptyState={rs.active ? roomEmptyState(rs) : undefined}
        >
          {showPanel && rs.missingFile && (
            <MissingFilePanel
              file={rs.missingFile}
              status={rs.fileStatus}
              verifying={verifying}
              onChoose={() => void chooseMyFile()}
              onClose={() => setDismissedPanel(rs.missingFile?.seq ?? 0)}
            />
          )}
          {rs.active && rs.joining && (
            <div className="absolute left-1/2 top-4 z-10 -translate-x-1/2 rounded-full bg-black/70 px-3 py-1 text-sm" role="status">
              Joining... catching up with the room
            </div>
          )}
          {fullscreen && <ChatOverlay messages={chat} inputOpen={overlayInput} onCloseInput={() => setOverlayInput(false)} onSend={sendChat} />}
        </VideoArea>
        {!fullscreen && cols.right === 0 && <CollapsedRail side="right" label="Show members (])" onClick={() => setPrefs((p) => ({ ...p, rightCollapsed: false }))} />}
        {!fullscreen && cols.right > 0 && (
          <>
            <div className="w-1 shrink-0 cursor-col-resize bg-border hover:bg-accent" role="separator" aria-orientation="vertical" aria-label="Resize members" onPointerDown={startDrag('right')} data-testid="resizer-right" />
            <div ref={rightRef} style={{ width: cols.right }} className="flex h-full shrink-0 flex-col overflow-y-auto bg-surface" data-testid="col-right">
              <Section title="Members" count={rs.members.length} collapsed={prefs.membersCollapsed} onToggle={() => setPrefs((p) => ({ ...p, membersCollapsed: !p.membersCollapsed }))} onCollapseColumn={() => setPrefs((p) => ({ ...p, rightCollapsed: true }))}>
                <MembersPanel members={rs.members} viewerIsHost={isHost} onAction={act} />
              </Section>
              <Section title={isHost ? 'Host controls' : 'Room'} collapsed={prefs.hostCollapsed} onToggle={() => setPrefs((p) => ({ ...p, hostCollapsed: !p.hostCollapsed }))}>
                <HostControls
                  isHost={isHost}
                  room={rs}
                  tracks={tracks}
                  mediaKey={mediaKey}
                  hasMissingFile={rs.missingFile != null}
                  onChangeSource={() => setChangeSource({})}
                  onChooseFile={() => void chooseMyFile()}
                  onSelectTrack={(k, s) => ctl.selectTrack(k, s)}
                  noLimit={noLimit}
                  onNoLimit={setNoLimit}
                  onAddTrack={(k) => void addTrack(k)}
                  onAction={act}
                />
              </Section>
            </div>
          </>
        )}
      </main>
      {changeSource && (
        <ChangeSourceDialog
          initialPath={changeSource.path}
          onClose={() => setChangeSource(null)}
          onLocalChosen={(p) => void run(rs.active ? room.setLocalMedia(p) : player.open(p))}
        />
      )}
      {rs.active && rs.needPassword && <PasswordPrompt wrong={rs.needPassword.wrong} retryAfterMs={rs.needPassword.retryAfterMs} onLeave={leave} />}
      {rs.ended && rs.ended.kind !== 'left' && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60" role="alertdialog" aria-labelledby="ended-title">
          <div className="w-96 space-y-4 rounded-xl border border-border bg-elevated p-5">
            <h2 id="ended-title" className="font-brand text-lg font-bold">
              You left the room
            </h2>
            <p className="text-sm text-muted">{endText(rs.ended)}</p>
            <div className="flex justify-end">
              <button type="button" className="rounded-md bg-accent px-4 py-2 font-semibold text-on-accent hover:bg-accent-hover" onClick={onLeave}>
                Back to home
              </button>
            </div>
          </div>
        </div>
      )}
      {relayedAsk && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60" role="alertdialog" aria-labelledby="relayed-title">
          <div className="w-[28rem] space-y-3 rounded-xl border border-border bg-elevated p-5">
            <h2 id="relayed-title" className="font-brand text-lg font-bold">
              Large file to relayed members
            </h2>
            <p className="text-sm text-muted">
              This file is {(relayedAsk.size / 1e6).toFixed(0)} MB. {relayedAsk.members.join(', ')} {relayedAsk.members.length === 1 ? 'is' : 'are'} connected
              through a relay, so sending it to them uses your relay quota.
            </p>
            <div className="flex flex-wrap justify-end gap-2">
              <button type="button" className="rounded-md px-3 py-2 text-muted hover:text-text" onClick={() => setRelayedAsk(null)}>
                Cancel
              </button>
              <button
                type="button"
                className="rounded-md border border-border px-3 py-2 hover:border-accent"
                onClick={() => {
                  const a = relayedAsk;
                  setRelayedAsk(null);
                  void addExternal(a.kind, a.path, 'skipRelayed');
                }}
              >
                Skip relayed members
              </button>
              <button
                type="button"
                className="rounded-md bg-accent px-3 py-2 font-semibold text-on-accent"
                onClick={() => {
                  const a = relayedAsk;
                  setRelayedAsk(null);
                  void addExternal(a.kind, a.path, 'all');
                }}
              >
                Send to everyone
              </button>
            </div>
          </div>
        </div>
      )}
      {diagOpen && <DiagnosticsDialog rs={rs} uiErrors={uiErrors} onClose={() => setDiagOpen(false)} />}
      {toasts.length > 0 && (
        <div className="pointer-events-none fixed bottom-4 right-4 z-40 space-y-2" aria-live="polite">
          {toasts.map((t) => (
            <p key={t.id} className="rounded-md border border-border bg-elevated px-3 py-2 text-sm shadow-lg">
              {t.text}
            </p>
          ))}
        </div>
      )}
    </div>
  );
}

/** The in-room empty player: a new room starts with no video until the host picks a source. */
export function roomEmptyState(rs: RoomState): { title: string; body: string; action: string | null } {
  if (rs.missingFile) {
    return { title: 'No video yet', body: `Choose your copy of ${rs.missingFile.name} to watch along.`, action: null };
  }
  const canPick = rs.isHost || rs.myPermissions.canChangeMedia;
  return {
    title: 'No video yet',
    body: rs.isHost
      ? 'Pick a local file or a torrent magnet for the room. Everyone plays their own copy.'
      : 'The host has not picked a video yet.',
    action: canPick ? 'Choose a source...' : null,
  };
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
