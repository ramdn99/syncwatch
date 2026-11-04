import { useState } from 'react';
import type { MockMember } from '../mock/data';
import { memberColors } from '../theme/tokens';

const CONN_TEXT = { direct: 'Direct', relayed: 'Relayed', reconnecting: 'Reconnecting...' } as const;
const CONN_COLOR = { direct: 'text-success', relayed: 'text-info', reconnecting: 'text-warning' } as const;
const MEDIA_TEXT = {
  ready: 'Ready',
  verifying: 'Verifying file...',
  missing: 'Missing file',
  mismatch: 'File differs',
  buffering: 'Buffering',
} as const;

export function MembersPanel({ members, viewerIsHost }: { members: MockMember[]; viewerIsHost: boolean }) {
  const [menuFor, setMenuFor] = useState<string | null>(null);
  return (
    <ul className="space-y-1 p-2" aria-label="Members">
      {members.map((m) => (
        <li key={m.id} className="relative rounded-md px-2 py-1.5 hover:bg-elevated">
          <div className="flex items-center gap-2">
            <span className="h-2.5 w-2.5 shrink-0 rounded-full" style={{ background: memberColors[m.colorIndex % memberColors.length] }} aria-hidden />
            <span className="truncate font-medium">
              {m.name}
              {m.isYou && <span className="text-muted"> (you)</span>}
            </span>
            {m.isHost && <span className="rounded bg-accent px-1 text-[10px] font-bold text-on-accent">HOST</span>}
            {m.muted && (
              <span className="text-muted" title="Muted in chat" aria-label="Muted in chat">
                🔇
              </span>
            )}
            {!m.isYou && (
              <button
                type="button"
                className="ml-auto rounded px-1.5 text-muted hover:bg-surface hover:text-text"
                aria-label={`Actions for ${m.name}`}
                aria-haspopup="menu"
                aria-expanded={menuFor === m.id}
                onClick={() => setMenuFor((v) => (v === m.id ? null : m.id))}
              >
                ⋯
              </button>
            )}
          </div>
          <div className="mt-0.5 flex flex-wrap gap-x-3 pl-4.5 text-xs">
            {!m.isYou && (
              <span className={CONN_COLOR[m.conn]}>
                {CONN_TEXT[m.conn]}
                {m.latencyMs != null && <span className="tabular text-muted"> · {m.latencyMs} ms</span>}
              </span>
            )}
            <span className={m.media === 'ready' ? 'text-muted' : m.media === 'missing' || m.media === 'mismatch' ? 'text-danger' : 'text-warning'}>
              {MEDIA_TEXT[m.media]}
            </span>
            <span className={m.tracksInSync ? 'text-muted' : 'text-warning'}>{m.tracksInSync ? 'Tracks in sync' : 'Tracks differ'}</span>
          </div>
          {menuFor === m.id && (
            <div role="menu" className="absolute right-2 top-8 z-10 w-44 rounded-md border border-border bg-elevated py-1 shadow-lg">
              {(viewerIsHost ? ['Permissions...', 'Mute in chat', 'Make host', 'Kick', 'Ban for this session'] : ['Mute locally']).map((label) => (
                <button
                  key={label}
                  type="button"
                  role="menuitem"
                  className={`block w-full px-3 py-1.5 text-left hover:bg-surface ${label.startsWith('Kick') || label.startsWith('Ban') ? 'text-danger' : ''}`}
                  onClick={() => setMenuFor(null)}
                >
                  {label}
                </button>
              ))}
            </div>
          )}
        </li>
      ))}
    </ul>
  );
}
