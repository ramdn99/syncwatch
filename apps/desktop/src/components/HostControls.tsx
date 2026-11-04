import { useState } from 'react';
import type { Track, TrackSelection } from '../lib/player';
import { TrackList } from './TrackList';

type Preset = 'everyone' | 'onlyMe' | 'custom';

export interface HostControlsProps {
  isHost: boolean;
  tracks: Track[];
  mediaKey: string;
  onSelectTrack: (kind: 'audio' | 'sub', sel: TrackSelection) => void;
  onAddTrack: (kind: 'audio' | 'sub') => void;
}

export function HostControls({ isHost, tracks, mediaKey, onSelectTrack, onAddTrack }: HostControlsProps) {
  const [preset, setPreset] = useState<Preset>('everyone');
  const [waitForEveryone, setWaitForEveryone] = useState(false);
  return (
    <div className="space-y-3 p-3">
      {isHost ? (
        <>
          <fieldset>
            <legend className="mb-1 text-xs text-muted">Who can control playback</legend>
            <div className="grid grid-cols-3 overflow-hidden rounded-md border border-border text-xs" role="radiogroup">
              {(
                [
                  ['everyone', 'Everyone'],
                  ['onlyMe', 'Only me'],
                  ['custom', 'Custom'],
                ] as const
              ).map(([v, label]) => (
                <button
                  key={v}
                  type="button"
                  role="radio"
                  aria-checked={preset === v}
                  className={`px-2 py-1.5 ${preset === v ? 'bg-accent font-semibold text-on-accent' : 'bg-elevated text-muted hover:text-text'}`}
                  onClick={() => setPreset(v)}
                >
                  {label}
                </button>
              ))}
            </div>
          </fieldset>
          <button
            type="button"
            className="w-full rounded-md border border-border bg-elevated px-3 py-2 text-left hover:border-accent"
            title="Available from Milestone 3"
            disabled
          >
            <span className="font-medium">Change source...</span>
            <span className="block text-xs text-muted">Local file or torrent magnet (coming soon)</span>
          </button>
        </>
      ) : (
        <p className="rounded-md bg-elevated px-3 py-2 text-xs text-muted">
          The host controls tracks and permissions. You can play, pause and seek.
        </p>
      )}
      <TrackList kind="audio" tracks={tracks} isHost={isHost} mediaKey={mediaKey} onSelect={(s) => onSelectTrack('audio', s)} onAdd={() => onAddTrack('audio')} />
      <TrackList kind="sub" tracks={tracks} isHost={isHost} mediaKey={mediaKey} onSelect={(s) => onSelectTrack('sub', s)} onAdd={() => onAddTrack('sub')} />
      {isHost && (
        <label className="flex items-center gap-2 text-xs text-muted">
          <input type="checkbox" checked={waitForEveryone} onChange={(e) => setWaitForEveryone(e.target.checked)} className="accent-[#E8A33D]" />
          Wait for everyone before playing
        </label>
      )}
    </div>
  );
}
