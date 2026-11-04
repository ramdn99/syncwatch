import { useEffect, useId, useState } from 'react';
import { trackLabel, type Track, type TrackSelection } from '../lib/player';

export interface TrackListProps {
  kind: 'audio' | 'sub';
  tracks: Track[];
  isHost: boolean;
  /** Changes whenever a new video loads; the list collapses again each time. */
  mediaKey: string;
  onSelect: (sel: TrackSelection) => void;
  onAdd: () => void;
}

const TITLES = { audio: 'Audio', sub: 'Subtitles' } as const;
const ADD_LABELS = { audio: 'Add audio file...', sub: 'Add subtitle file...' } as const;
const LOCK_TIP = 'Only the host can change tracks';

/**
 * Collapsible track list (spec section 7): collapsed by default each time a video loads; the
 * header shows title, selected track and count; the FIRST item inside is "Add ... file..."
 * (host only); a small "+" in the header works while collapsed. Members see read-only lists.
 */
export function TrackList({ kind, tracks, isHost, mediaKey, onSelect, onAdd }: TrackListProps) {
  const [expanded, setExpanded] = useState(false);
  const listId = useId();
  useEffect(() => setExpanded(false), [mediaKey]);

  const items = tracks.filter((t) => t.kind === kind);
  const selected = items.find((t) => t.selected);
  const selectedText = selected ? trackLabel(selected) : kind === 'sub' ? 'Off' : 'None';

  return (
    <section className="rounded-lg border border-border bg-elevated" data-testid={`tracklist-${kind}`}>
      <div className="flex items-center gap-1 pr-1">
        <button
          type="button"
          className="flex min-w-0 flex-1 items-center gap-2 px-3 py-2 text-left"
          aria-expanded={expanded}
          aria-controls={listId}
          onClick={() => setExpanded((v) => !v)}
          title={isHost ? undefined : LOCK_TIP}
        >
          <span className="text-muted" aria-hidden>
            {expanded ? '▾' : '▸'}
          </span>
          <span className="font-brand font-medium">{TITLES[kind]}</span>
          <span className="min-w-0 flex-1 truncate text-muted" data-testid="selected-track">
            {selectedText}
          </span>
          <span className="tabular rounded bg-surface px-1.5 text-xs text-muted" aria-label={`${items.length} tracks`}>
            {items.length}
          </span>
          {!isHost && (
            <span className="text-muted" aria-label={LOCK_TIP} title={LOCK_TIP}>
              🔒
            </span>
          )}
        </button>
        {isHost && (
          <button
            type="button"
            className="rounded px-2 py-1 text-accent hover:bg-surface hover:text-accent-hover"
            aria-label={ADD_LABELS[kind]}
            title={ADD_LABELS[kind]}
            onClick={onAdd}
          >
            +
          </button>
        )}
      </div>
      {expanded && (
        <ul id={listId} className="max-h-64 overflow-y-auto border-t border-border py-1" role="listbox" aria-label={TITLES[kind]}>
          {isHost && (
            <li>
              <button type="button" className="w-full px-3 py-1.5 text-left text-accent hover:bg-surface" onClick={onAdd}>
                {ADD_LABELS[kind]}
              </button>
            </li>
          )}
          {kind === 'sub' && (
            <TrackRow label="Off" selected={!selected} disabled={!isHost} onClick={() => onSelect({ mode: 'off' })} />
          )}
          {items.map((t) => (
            <TrackRow
              key={`${t.kind}-${t.id}`}
              label={trackLabel(t)}
              badge={t.external ? 'external' : t.default ? 'default' : undefined}
              selected={t.selected}
              disabled={!isHost}
              onClick={() => onSelect({ mode: 'id', id: t.id })}
            />
          ))}
          {items.length === 0 && <li className="px-3 py-1.5 text-muted">No {TITLES[kind].toLowerCase()} tracks</li>}
        </ul>
      )}
    </section>
  );
}

function TrackRow(props: { label: string; badge?: string; selected: boolean; disabled: boolean; onClick: () => void }) {
  return (
    <li role="option" aria-selected={props.selected}>
      <button
        type="button"
        disabled={props.disabled}
        title={props.disabled ? LOCK_TIP : undefined}
        className={`flex w-full items-center gap-2 px-3 py-1.5 text-left ${
          props.disabled ? 'cursor-default' : 'hover:bg-surface'
        } ${props.selected ? 'text-text' : 'text-muted'}`}
        onClick={props.onClick}
      >
        <span className={props.selected ? 'text-accent' : 'invisible'} aria-hidden>
          ●
        </span>
        <span className="min-w-0 flex-1 truncate">{props.label}</span>
        {props.badge && <span className="text-xs text-muted">{props.badge}</span>}
      </button>
    </li>
  );
}
