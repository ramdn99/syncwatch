// Host, manual invites (ADR 0014): make invites, paste replies, compare words, admit or refuse.
import { useEffect, useState } from 'react';
import { friendlyError, room, type InviteView, type RoomState } from '../lib/room';
import { INVITE_TEXT } from '../strings/invites';
import { Icon } from '../ui/icons';
import { CopyBox, minutesLeft, Words } from './InviteJoin';

const OPEN: InviteView['status'][] = ['preparing', 'waiting', 'replied', 'admitted'];

export function InvitesPanel({ rs }: { rs: Pick<RoomState, 'invites' | 'inviteError' | 'members'> }) {
  const [reply, setReply] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), 15_000);
    return () => window.clearInterval(t);
  }, []);
  // A refused reply (wrong text, expired, used...) arrives through the room state.
  useEffect(() => {
    if (rs.inviteError) setError(rs.inviteError.message);
  }, [rs.inviteError?.seq]); // eslint-disable-line react-hooks/exhaustive-deps

  const run = (p: Promise<unknown>) => void p.catch((e) => setError(friendlyError(e).text));
  const open = rs.invites.filter((i) => OPEN.includes(i.status));
  const done = rs.invites.filter((i) => !OPEN.includes(i.status));
  const seatName = (m: number | null) => rs.members.find((x) => x.id === m)?.name;

  return (
    <section className="space-y-3" aria-label="Invites" data-testid="invites-panel">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold">Invites</h3>
        <button
          type="button"
          className="inline-flex items-center gap-1 rounded-md bg-accent px-2.5 py-1 text-xs font-semibold text-on-accent hover:bg-accent-hover"
          onClick={() => {
            setError(null);
            run(room.invite.create());
          }}
        >
          <Icon name="invite" size="xs" />
          Invite a member
        </button>
      </div>
      <p className="text-xs text-muted">{INVITE_TEXT.hostPanelIntro}</p>
      {rs.invites.some((i) => i.status === 'waiting') && (
        <form
          className="space-y-1"
          onSubmit={(e) => {
            e.preventDefault();
            setError(null);
            run(room.invite.reply(reply).then(() => setReply('')));
          }}
        >
          <label className="block text-xs text-muted" htmlFor="reply-input">
            {INVITE_TEXT.pasteReply}
          </label>
          <textarea
            id="reply-input"
            rows={2}
            value={reply}
            spellCheck={false}
            placeholder="SWREP1-..."
            data-testid="reply-input"
            onChange={(e) => setReply(e.target.value)}
            className="w-full resize-none rounded-md border border-border bg-surface px-2 py-1.5 font-mono text-[11px] break-all text-text focus:border-accent focus:outline-none"
          />
          <button type="submit" disabled={!reply.trim()} className="rounded-md bg-elevated px-2.5 py-1 text-xs font-semibold hover:bg-surface disabled:opacity-50">
            Add reply
          </button>
        </form>
      )}
      {error && (
        <p role="alert" className="rounded-md border border-danger/40 bg-danger/10 px-2 py-1.5 text-xs text-danger" data-testid="invite-error">
          {error}
        </p>
      )}
      {rs.invites.length === 0 && <p className="text-xs text-muted">{INVITE_TEXT.noInvites}</p>}
      <ul className="space-y-2">
        {[...open, ...done].map((inv) => (
          <li key={inv.id} className="space-y-2 rounded-md border border-border bg-surface p-2 text-xs" data-testid={`invite-${inv.status}`}>
            <div className="flex items-center gap-2">
              <span className={inv.status === 'replied' ? 'font-semibold text-accent' : inv.status === 'joined' ? 'text-success' : OPEN.includes(inv.status) ? 'text-text' : 'text-muted'}>
                {INVITE_TEXT.status[inv.status]}
              </span>
              {inv.forMember != null && <span className="text-muted">re-invite for {seatName(inv.forMember) ?? 'a member'}</span>}
              {(inv.status === 'waiting' || inv.status === 'replied') && (
                <span className="tabular text-muted">{minutesLeft(inv.expiresUnix, now)} min left</span>
              )}
              <button
                type="button"
                className="ml-auto rounded px-1 text-muted hover:text-text"
                aria-label="Remove this invite"
                title="Remove this invite"
                onClick={() => run(room.invite.decide(inv.id, 'remove'))}
              >
                <Icon name="close" size="xs" />
              </button>
            </div>
            {inv.status === 'waiting' && inv.text && <CopyBox label="Invite" text={inv.text} testId={`invite-text-${inv.id}`} />}
            {inv.status === 'replied' && inv.words && (
              <>
                <p className="text-sm">
                  <span className="font-semibold">{inv.memberName}</span> wants to join.
                </p>
                <Words words={inv.words} label={INVITE_TEXT.compareHost} />
                <div className="flex gap-2">
                  <button
                    type="button"
                    className="rounded-md bg-accent px-3 py-1.5 font-semibold text-on-accent hover:bg-accent-hover"
                    onClick={() => run(room.invite.decide(inv.id, 'admit'))}
                  >
                    Admit
                  </button>
                  <button type="button" className="rounded-md px-3 py-1.5 text-danger hover:bg-elevated" onClick={() => run(room.invite.decide(inv.id, 'refuse'))}>
                    Refuse
                  </button>
                </div>
              </>
            )}
            {inv.status === 'failed' && inv.reason && (
              <p role="alert" className="text-sm text-danger" data-testid="invite-fail-reason">
                Could not connect{inv.memberName ? ` to ${inv.memberName}` : ''}: {inv.reason}
              </p>
            )}
            {(inv.status === 'expired' || inv.status === 'failed') && (
              <button
                type="button"
                className="text-text underline"
                onClick={() => run(room.invite.decide(inv.id, 'remove').then(() => room.invite.create(inv.forMember)))}
              >
                Re-issue
              </button>
            )}
          </li>
        ))}
      </ul>
    </section>
  );
}
