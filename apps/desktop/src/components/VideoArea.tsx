import { forwardRef, useEffect, useRef, useState, type ReactNode } from 'react';
import type { PlaybackState } from '../lib/player';
import { formatTime } from '../lib/time';

export interface VideoAreaProps {
  state: PlaybackState;
  fullscreen: boolean;
  /** Seek-bar markers (seconds) where chat messages were sent. */
  markers: number[];
  onTogglePause: () => void;
  onSeek: (seconds: number) => void;
  onVolume: (v: number) => void;
  onToggleMute: () => void;
  onToggleFullscreen: () => void;
  onOpenFile: () => void;
  children?: ReactNode;
}

const HIDE_AFTER_MS = 2500;

/**
 * CENTER column. The element itself is transparent: the mpv window sits underneath and is
 * kept aligned to this element's rect by the native side. Overlay controls cover only this
 * area and auto-hide (with a dark scrim) during playback.
 */
export const VideoArea = forwardRef<HTMLDivElement, VideoAreaProps>(function VideoArea(props, ref) {
  const { state, fullscreen, markers } = props;
  const [visible, setVisible] = useState(true);
  const timer = useRef<number | undefined>(undefined);
  const playing = state.fileLoaded && !state.paused;

  const poke = () => {
    setVisible(true);
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setVisible(false), HIDE_AFTER_MS);
  };
  useEffect(() => {
    if (!playing) {
      window.clearTimeout(timer.current);
      setVisible(true);
    } else {
      poke();
    }
    return () => window.clearTimeout(timer.current);
  }, [playing]);

  const showControls = visible || !playing;
  const duration = state.duration ?? 0;
  const pos = state.timePos ?? 0;
  const pct = duration > 0 ? Math.min(100, (pos / duration) * 100) : 0;

  return (
    <div
      ref={ref}
      className={`video-hole relative h-full min-w-0 flex-1 ${showControls ? '' : 'cursor-none'}`}
      onMouseMove={poke}
      onDoubleClick={props.onToggleFullscreen}
      data-testid="video-area"
    >
      {!state.fileLoaded && (
        <div className="absolute inset-0 flex flex-col items-center justify-center gap-3 text-center">
          <p className="font-brand text-lg">No video loaded</p>
          <p className="max-w-xs text-sm text-muted">Open a local file or drop it here. Your copy plays locally; it is never sent to anyone.</p>
          <button type="button" className="rounded-md bg-accent px-4 py-2 font-semibold text-on-accent hover:bg-accent-hover active:bg-accent-pressed" onClick={props.onOpenFile}>
            Open file...
          </button>
        </div>
      )}
      {state.fileLoaded && state.pausedForCache && (
        <div className="absolute left-1/2 top-4 -translate-x-1/2 rounded-full bg-black/70 px-3 py-1 text-sm">Buffering...</div>
      )}
      {props.children}
      <div
        className={`scrim absolute inset-x-0 bottom-0 px-4 pb-3 pt-10 transition-opacity duration-300 ${showControls ? 'opacity-100' : 'pointer-events-none opacity-0'}`}
        onClick={(e) => e.stopPropagation()}
        onDoubleClick={(e) => e.stopPropagation()}
      >
        <SeekBar pct={pct} duration={duration} markers={markers} cacheAhead={state.cacheAhead} pos={pos} onSeek={props.onSeek} />
        <div className="mt-2 flex items-center gap-3">
          <IconButton label={state.paused ? 'Play (Space)' : 'Pause (Space)'} onClick={props.onTogglePause}>
            {state.paused ? '▶' : '❚❚'}
          </IconButton>
          <span className="tabular text-sm">
            {formatTime(state.timePos)} <span className="text-muted">/ {formatTime(state.duration)}</span>
          </span>
          <div className="ml-auto flex items-center gap-2">
            <IconButton label={state.mute ? 'Unmute (M)' : 'Mute (M)'} onClick={props.onToggleMute}>
              {state.mute ? '🔇' : '🔊'}
            </IconButton>
            <input
              type="range"
              min={0}
              max={100}
              value={Math.round(state.volume)}
              aria-label="Volume"
              className="w-24 accent-[#E8A33D]"
              onChange={(e) => props.onVolume(Number(e.target.value))}
            />
            <IconButton label={fullscreen ? 'Exit fullscreen (F)' : 'Fullscreen (F)'} onClick={props.onToggleFullscreen}>
              {fullscreen ? '⤡' : '⤢'}
            </IconButton>
          </div>
        </div>
      </div>
    </div>
  );
});

function IconButton(props: { label: string; onClick: () => void; children: ReactNode }) {
  return (
    <button type="button" aria-label={props.label} title={props.label} onClick={props.onClick} className="flex h-8 min-w-8 items-center justify-center rounded-md px-1 text-text hover:bg-white/10">
      {props.children}
    </button>
  );
}

function SeekBar(props: { pct: number; pos: number; duration: number; markers: number[]; cacheAhead: number | null; onSeek: (s: number) => void }) {
  const barRef = useRef<HTMLDivElement>(null);
  const { duration } = props;
  const seekAt = (clientX: number) => {
    const el = barRef.current;
    if (!el || duration <= 0) return;
    const r = el.getBoundingClientRect();
    props.onSeek(((clientX - r.left) / r.width) * duration);
  };
  const cachePct = duration > 0 && props.cacheAhead ? Math.min(100, ((props.pos + props.cacheAhead) / duration) * 100) : 0;
  return (
    <div
      ref={barRef}
      className="group relative h-4 cursor-pointer"
      role="slider"
      aria-label="Seek"
      aria-valuemin={0}
      aria-valuemax={Math.round(duration)}
      aria-valuenow={Math.round(props.pos)}
      tabIndex={-1}
      onPointerDown={(e) => {
        (e.target as HTMLElement).setPointerCapture?.(e.pointerId);
        seekAt(e.clientX);
      }}
    >
      <div className="absolute inset-x-0 top-1.5 h-1 rounded-full bg-white/20 group-hover:h-1.5">
        <div className="absolute inset-y-0 left-0 rounded-full bg-white/25" style={{ width: `${cachePct}%` }} />
        <div className="absolute inset-y-0 left-0 rounded-full bg-accent" style={{ width: `${props.pct}%` }} />
      </div>
      {duration > 0 &&
        props.markers.map((m, i) => (
          <span key={i} className="absolute top-0 h-1.5 w-0.5 rounded bg-info" style={{ left: `${Math.min(100, (m / duration) * 100)}%` }} aria-hidden />
        ))}
    </div>
  );
}
