import { useState } from 'react';
import type { LinkState, MediaText, Perms, RoomAction, RoomMember } from '../lib/room';
import { memberColors } from '../theme/tokens';

const CONN_TEXT: Record<LinkState, string> = {
  connecting: 'Connecting...',
  authenticating: 'Verifying...',
  direct: 'Direct',
  relayed: 'Relayed',
  reconnecting: 'Reconnecting...',
};
const CONN_COLOR: Record<LinkState, string> = {
  connecting: 'text-muted',
  authenticating: 'text-muted',
  direct: 'text-success',
  relayed: 'text-info',
  reconnecting: 'text-warning',
};
const MEDIA_TEXT: Record<MediaText, string> = {
  none: 'No video yet',
  ready: 'Ready',
  verifying: 'Verifying file...',
  missing: 'Missing file',
  mismatch: 'File differs',
  downloading: 'Downloading',
  declined: 'Declined',
  unavailable: 'Unavailable',
};

const PERM_LABELS: [keyof Perms, string][] = [
  ['canPlayPause', 'Play and pause'],
  ['canSeek', 'Seek'],
  ['canChangeSpeed', 'Change speed'],
  ['canChangeMedia', 'Change source'],
  ['canChat', 'Chat'],
];

export function MembersPanel({
  members,
  viewerIsHost,
  onAction,
}: {
  members: RoomMember[];
  viewerIsHost: boolean;
  onAction: (a: RoomAction) => void;
}) {
  const [menuFor, setMenuFor] = useState<number | null>(null);
  const [permsFor, setPermsFor] = useState<number | null>(null);
  const [locallyMuted, setLocallyMuted] = useState<Set<number>>(new Set());

  const act = (a: RoomAction) => {
    setMenuFor(null);
    onAction(a);
  };

  return (
    <ul className="space-y-1 p-2" aria-label="Members">
      {members.map((m) => {
        const items: { label: string; danger?: boolean; run: () => void }[] = viewerIsHost
          ? [
              { label: 'Permissions...', run: () => (setMenuFor(null), setPermsFor((v) => (v === m.id ? null : m.id))) },
              { label: m.chatMuted ? 'Unmute in chat' : 'Mute in chat', run: () => act({ type: 'muteChat', member: m.id, muted: !m.chatMuted }) },
              { label: 'Make host', run: () => act({ type: 'makeHost', member: m.id }) },
              { label: 'Kick', danger: true, run: () => act({ type: 'kick', member: m.id }) },
              { label: 'Ban for this session', danger: true, run: () => act({ type: 'ban', member: m.id }) },
            ]
          : [
              {
                label: locallyMuted.has(m.id) ? 'Unmute locally' : 'Mute locally',
                run: () => {
                  const muted = !locallyMuted.has(m.id);
                  setLocallyMuted((s) => {
                    const n = new Set(s);
                    if (muted) n.add(m.id);
                    else n.delete(m.id);
                    return n;
                  });
                  act({ type: 'localMute', member: m.id, muted });
                },
              },
            ];
        return (
          <li key={m.id} className="relative rounded-md px-2 py-1.5 hover:bg-elevated">
            <div className="flex items-center gap-2">
              <span className="h-2.5 w-2.5 shrink-0 rounded-full" style={{ background: memberColors[m.id % memberColors.length] }} aria-hidden />
              <span className="truncate font-medium">
                {m.name}
                {m.isYou && <span className="text-muted"> (you)</span>}
              </span>
              {m.isHost && <span className="rounded bg-accent px-1 text-[10px] font-bold text-on-accent">HOST</span>}
              {m.chatMuted && (
                <span className="text-muted" title="Muted in chat" aria-label="Muted in chat">
                  🔇
                </span>
              )}
              {!m.isYou && (viewerIsHost ? !m.isHost : true) && (
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
              {!m.isYou && m.link && <span className={CONN_COLOR[m.link]}>{CONN_TEXT[m.link]}</span>}
              <span className={m.media === 'ready' || m.media === 'none' ? 'text-muted' : m.media === 'missing' || m.media === 'mismatch' ? 'text-danger' : 'text-warning'}>
                {MEDIA_TEXT[m.media]}
              </span>
              {m.media !== 'none' && <span className={m.tracksInSync ? 'text-muted' : 'text-warning'}>{m.tracksInSync ? 'Tracks in sync' : 'Tracks differ'}</span>}
            </div>
            {viewerIsHost && m.words && (
              <p className="mt-0.5 pl-4.5 text-xs text-muted" title="Ask this person to read their four words: they must match these.">
                Verify: <span className="font-brand text-text">{m.words.join(' ')}</span>
              </p>
            )}
            {menuFor === m.id && (
              <div role="menu" className="absolute right-2 top-8 z-10 w-48 rounded-md border border-border bg-elevated py-1 shadow-lg">
                {items.map((it) => (
                  <button
                    key={it.label}
                    type="button"
                    role="menuitem"
                    className={`block w-full px-3 py-1.5 text-left hover:bg-surface ${it.danger ? 'text-danger' : ''}`}
                    onClick={it.run}
                  >
                    {it.label}
                  </button>
                ))}
              </div>
            )}
            {viewerIsHost && permsFor === m.id && (
              <fieldset className="mt-2 space-y-1 rounded-md border border-border bg-surface p-2 text-xs">
                <legend className="px-1 text-muted">Permissions for {m.name}</legend>
                {PERM_LABELS.map(([k, label]) => (
                  <label key={k} className="flex items-center gap-2">
                    <input
                      type="checkbox"
                      className="accent-[#E8A33D]"
                      checked={m.permissions[k]}
                      onChange={(e) => onAction({ type: 'setPermissions', member: m.id, permissions: { ...m.permissions, [k]: e.target.checked } })}
                    />
                    {label}
                  </label>
                ))}
              </fieldset>
            )}
          </li>
        );
      })}
    </ul>
  );
}
