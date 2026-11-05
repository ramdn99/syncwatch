import { useState } from 'react';
import type { Track, TrackSelection } from '../lib/player';
import type { RoomAction, RoomState } from '../lib/room';
import { TrackList } from './TrackList';

type Preset = 'everyone' | 'onlyMe';

export interface HostControlsProps {
  isHost: boolean;
  room: Pick<RoomState, 'locked' | 'chatConfig' | 'myPermissions'>;
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
      {!isHost && (
        <p className="rounded-md bg-elevated px-3 py-2 text-xs text-muted">The host controls tracks and permissions.</p>
      )}
      {p.hasMissingFile && (
        <button type="button" className="w-full rounded-md border border-border px-3 py-1.5 text-left text-sm hover:border-accent" onClick={p.onChooseFile}>
          Choose a different file...
        </button>
      )}
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
