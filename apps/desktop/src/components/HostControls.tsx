import { useState } from 'react';
import type { Track, TrackSelection } from '../lib/player';
import { permsSummary, type RoomAction, type RoomState } from '../lib/room';
import { TrackList } from './TrackList';

type Preset = 'everyone' | 'onlyMe';

export interface HostControlsProps {
  isHost: boolean;
  room: Pick<RoomState, 'locked' | 'chatConfig' | 'myPermissions' | 'externals' | 'declined' | 'active'>;
  /** Host: send external files above the 1 GB audio / 20 MB subtitle ceilings. */
  noLimit?: boolean;
  onNoLimit?: (on: boolean) => void;
  tracks: Track[];
  mediaKey: string;
  hasMissingFile: boolean;
  onSelectTrack: (kind: 'audio' | 'sub', sel: TrackSelection) => void;
  onAddTrack: (kind: 'audio' | 'sub') => void;
  onChangeSource: () => void;
  onChooseFile: () => void;
  onAction: (a: RoomAction) => void;
}

export function HostControls(p: HostControlsProps) {
  const { isHost, room } = p;
  const [preset, setPreset] = useState<Preset | null>(null);
  const [waitForEveryone, setWaitForEveryone] = useState(false);
  const cc = room.chatConfig;
  const canChangeSource = isHost || room.myPermissions.canChangeMedia;
  return (
    <div className="space-y-3 p-3">
      {isHost && (
        <fieldset>
          <legend className="mb-1 text-xs text-muted">Who can control playback</legend>
          <div className="grid grid-cols-2 overflow-hidden rounded-md border border-border text-xs" role="radiogroup">
            {(
              [
                ['everyone', 'Everyone'],
                ['onlyMe', 'Only me'],
              ] as const
            ).map(([v, label]) => (
              <button
                key={v}
                type="button"
                role="radio"
                aria-checked={preset === v}
                className={`px-2 py-1.5 ${preset === v ? 'bg-accent font-semibold text-on-accent' : 'bg-elevated text-muted hover:text-text'}`}
                onClick={() => {
                  setPreset(v);
                  p.onAction({ type: 'preset', preset: v });
                }}
              >
                {label}
              </button>
            ))}
          </div>
          <p className="mt-1 text-[11px] text-muted">Custom: use Permissions... in a member's menu.</p>
        </fieldset>
      )}
      {canChangeSource && (
        <button type="button" className="w-full rounded-md border border-border bg-elevated px-3 py-2 text-left hover:border-accent" onClick={p.onChangeSource}>
          <span className="font-medium">Change source...</span>
          <span className="block text-xs text-muted">Local file or torrent magnet</span>
        </button>
      )}
      {!isHost && <MyPermissions perms={room.myPermissions} />}
      {p.hasMissingFile && (
        <button type="button" className="w-full rounded-md border border-border px-3 py-1.5 text-left text-sm hover:border-accent" onClick={p.onChooseFile}>
          Choose a different file...
        </button>
      )}
      {room.active && room.externals.length > 0 && <Externals room={room} isHost={isHost} onAction={p.onAction} />}
      <TrackList kind="audio" tracks={p.tracks} isHost={isHost} mediaKey={p.mediaKey} onSelect={(s) => p.onSelectTrack('audio', s)} onAdd={() => p.onAddTrack('audio')} />
      <TrackList kind="sub" tracks={p.tracks} isHost={isHost} mediaKey={p.mediaKey} onSelect={(s) => p.onSelectTrack('sub', s)} onAdd={() => p.onAddTrack('sub')} />
      {isHost && (
        <div className="space-y-1.5 text-xs text-muted">
          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={waitForEveryone}
              onChange={(e) => {
                setWaitForEveryone(e.target.checked);
                p.onAction({ type: 'waitForEveryone', on: e.target.checked });
              }}
              className="accent-[#E8A33D]"
            />
            Wait for everyone before playing
          </label>
          {room.active && <Delays onAction={p.onAction} />}
          {room.active && (
            <label className="flex items-center gap-2" title="Allow audio files over 1 GB and subtitles over 20 MB">
              <input type="checkbox" checked={p.noLimit ?? false} onChange={(e) => p.onNoLimit?.(e.target.checked)} className="accent-[#E8A33D]" />
              No size limit for added files
            </label>
          )}
          <label className="flex items-center gap-2">
            <input type="checkbox" checked={room.locked} onChange={(e) => p.onAction({ type: 'lock', locked: e.target.checked })} className="accent-[#E8A33D]" />
            Lock the room (no new people)
          </label>
          <fieldset className="space-y-1.5 border-t border-border pt-2">
            <legend className="text-muted">Chat</legend>
            <label className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={cc.enabled}
                onChange={(e) => p.onAction({ type: 'chatConfig', ...cc, enabled: e.target.checked })}
                className="accent-[#E8A33D]"
              />
              Chat enabled
            </label>
            <label className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={cc.hiddenFromMembers}
                onChange={(e) => p.onAction({ type: 'chatConfig', ...cc, hiddenFromMembers: e.target.checked })}
                className="accent-[#E8A33D]"
              />
              Hide chat from members
            </label>
            <label className="flex items-center gap-2">
              Slow mode
              <select
                value={cc.slowModeSecs}
                onChange={(e) => p.onAction({ type: 'chatConfig', ...cc, slowModeSecs: Number(e.target.value) })}
                className="rounded border border-border bg-surface px-1 py-0.5 text-text"
              >
                {[0, 5, 10, 30, 60].map((s) => (
                  <option key={s} value={s}>
                    {s === 0 ? 'Off' : `${s} s`}
                  </option>
                ))}
              </select>
            </label>
            <button type="button" className="rounded px-2 py-1 text-danger hover:bg-surface" onClick={() => p.onAction({ type: 'clearChat' })}>
              Clear chat
            </button>
          </fieldset>
        </div>
      )}
    </div>
  );
}

/** A member's own current permissions, read-only, updated live when the host changes them. */
export function MyPermissions({ perms }: { perms: RoomState['myPermissions'] }) {
  const can = permsSummary(perms);
  return (
    <div className="rounded-md bg-elevated px-3 py-2 text-xs" aria-label="Your permissions" data-testid="my-permissions">
      <p className="mb-1 text-muted">{can.length ? 'You can:' : 'You can only watch: the host controls playback.'}</p>
      {can.length > 0 && (
        <ul className="flex flex-wrap gap-1">
          {can.map((c) => (
            <li key={c} className="rounded bg-surface px-1.5 py-0.5 text-text">
              {c}
            </li>
          ))}
        </ul>
      )}
      <p className="mt-1 text-muted">Tracks and permissions are set by the host.</p>
    </div>
  );
}
const fmtMb = (n: number) => (n >= 1e9 ? `${(n / 1e9).toFixed(2)} GB` : `${(n / 1e6).toFixed(1)} MB`);

/** The room's external subtitle/audio files: members see their download and can cancel. */
function Externals({ room, isHost, onAction }: { room: Pick<RoomState, 'externals' | 'declined'>; isHost: boolean; onAction: (a: RoomAction) => void }) {
  return (
    <section className="space-y-1 text-xs" aria-label="Added audio and subtitle files">
      <h3 className="text-muted">Added files</h3>
      <ul className="space-y-1">
        {room.externals.map((e) => {
          const done = e.received != null && e.received >= e.size;
          const pct = e.received != null ? Math.floor((100 * e.received) / Math.max(1, e.size)) : null;
          return (
            <li key={e.id} className="flex items-center gap-2 rounded bg-elevated px-2 py-1" data-testid="external">
              <span className="text-muted">{e.kind === 'sub' ? 'Subtitles' : 'Audio'}</span>
              <span className="min-w-0 flex-1 truncate" title={e.name}>
                {e.name}
              </span>
              <span className="tabular text-muted">{fmtMb(e.size)}</span>
              {!isHost && e.failed && <span className="text-danger">failed</span>}
              {!isHost && !e.failed && pct != null && !done && <span className="tabular text-warning">{pct}%</span>}
              {!isHost && !e.failed && pct != null && !done && (
                <button type="button" className="rounded px-1 text-danger hover:bg-surface" onClick={() => onAction({ type: 'cancelDownload', externalId: e.id })}>
                  Cancel
                </button>
              )}
            </li>
          );
        })}
      </ul>
      {isHost && room.declined.length > 0 && (
        <ul className="text-muted" aria-label="Declined">
          {room.declined.map((d) => (
            <li key={d}>{d}</li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** Host: subtitle and audio delay, applied on every member immediately. */
function Delays({ onAction }: { onAction: (a: RoomAction) => void }) {
  const [sub, setSub] = useState(0);
  const [audio, setAudio] = useState(0);
  const row = (label: string, v: number, set: (n: number) => void, kind: 'sub' | 'audio') => (
    <label className="flex items-center gap-2">
      <span className="w-24">{label}</span>
      <input
        type="number"
        step={100}
        value={v}
        aria-label={`${label} in milliseconds`}
        className="tabular w-20 rounded border border-border bg-surface px-1 py-0.5 text-text"
        onChange={(e) => set(Number(e.target.value) || 0)}
        onBlur={() => onAction({ type: 'setDelay', kind, ms: v })}
        onKeyDown={(e) => {
          if (e.key === 'Enter') onAction({ type: 'setDelay', kind, ms: v });
        }}
      />
      ms
    </label>
  );
  return (
    <fieldset className="space-y-1">
      <legend className="text-muted">Delays (for everyone)</legend>
      {row('Subtitle delay', sub, setSub, 'sub')}
      {row('Audio delay', audio, setAudio, 'audio')}
    </fieldset>
  );
}