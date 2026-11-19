// Join with a manual invite (ADR 0014): paste the host's invite, see what it is, then send
// the reply back and wait to be admitted.
import { useEffect, useState } from 'react';
import { endText, friendlyError, onRoomState, room, type InvitePreview, type RoomState } from '../lib/room';
import { Icon } from '../ui/icons';
import { INVITE_TEXT } from '../strings/invites';

/** Minutes left until `unix` (at least 0). */
export function minutesLeft(unix: number, nowMs = Date.now()): number {
  return Math.max(0, Math.ceil((unix * 1000 - nowMs) / 60_000));
}

export function CopyBox(props: { label: string; text: string; testId: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="space-y-1">
      <div className="flex items-center justify-between text-sm">
        <span className="text-muted">{props.label}</span>
        <button
          type="button"
          className="inline-flex items-center gap-1 rounded-md bg-accent px-2.5 py-1 text-xs font-semibold text-on-accent hover:bg-accent-hover"
          onClick={() => {
            void navigator.clipboard?.writeText(props.text).then(() => {
              setCopied(true);
              setTimeout(() => setCopied(false), 1500);
            });
          }}
        >
          <Icon name={copied ? 'complete' : 'copy'} size="xs" />
          {copied ? 'Copied' : 'Copy'}
        </button>
      </div>
      <textarea
        readOnly
        value={props.text}
        rows={3}
        spellCheck={false}
        data-testid={props.testId}
        aria-label={props.label}
        className="w-full resize-none rounded-md border border-border bg-surface px-2 py-1.5 font-mono text-[11px] break-all text-muted"
        onFocus={(e) => e.currentTarget.select()}
      />
    </div>
  );
}

export function Words({ words, label }: { words: readonly string[]; label: string }) {
  return (
    <div className="rounded-md border border-accent/40 bg-accent/10 px-3 py-2 text-sm" data-testid="invite-words">
      <p className="text-muted">{label}</p>
      <p className="mt-1 font-brand text-base font-bold tracking-wide text-text">{words.join(' ')}</p>
    </div>
  );
}

/** The Join dialog body for manual invites. */
export function InviteJoin(props: {
  name: string;
  onEntered: () => void;
  onCancel: () => void;
  onNameRequired: () => boolean;
  /** The reply is ready (the dialog hides the name field). */
  onReplyStep?: () => void;
}) {
  const [text, setText] = useState('');
  const [preview, setPreview] = useState<InvitePreview | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [rs, setRs] = useState<RoomState | null>(null);

  // Check the pasted invite as soon as it changes (no network).
  useEffect(() => {
    if (!text.trim()) {
      setPreview(null);
      setPreviewError(null);
      return;
    }
    let live = true;
    room
      .invitePreview(text)
      .then((p) => live && (setPreview(p), setPreviewError(null)))
      .catch((e) => live && (setPreview(null), setPreviewError(friendlyError(e).text)));
    return () => {
      live = false;
    };
  }, [text]);

  // After the reply is ready: follow the room until we are admitted or it ends.
  const waiting = rs?.reply != null;
  const { onReplyStep } = props;
  useEffect(() => {
    if (waiting) onReplyStep?.();
  }, [waiting, onReplyStep]);
  useEffect(() => {
    if (!waiting) return;
    return onRoomState((s) => {
      setRs(s);
      if (s.phase === 'inRoom' && !s.ended) props.onEntered();
    });
  }, [waiting, props]);

  const submit = async () => {
    if (!props.onNameRequired()) return;
    setError(null);
    setBusy(true);
    try {
      const outcome = await room.joinInvite(props.name, text, password || null);
      if (outcome.status === 'ended') {
        setError(endText(outcome.ended) ?? INVITE_TEXT.joinFailed);
        return;
      }
      if (outcome.status === 'inRoom') {
        props.onEntered();
        return;
      }
      setRs(await room.state());
    } catch (e) {
      setError(friendlyError(e).text);
    } finally {
      setBusy(false);
    }
  };

  if (rs?.reply) {
    const ended = rs.ended ? endText(rs.ended) : null;
    return (
      <div className="space-y-3" data-testid="invite-reply-step">
        <p className="text-sm">{INVITE_TEXT.sendReply}</p>
        <CopyBox label="Your reply" text={rs.reply.text} testId="reply-text" />
        <Words words={rs.reply.words} label={INVITE_TEXT.compareMember} />
        {ended ? (
          <p role="alert" className="rounded-md border border-danger/40 bg-danger/10 px-3 py-2 text-sm text-danger">
            {ended}
          </p>
        ) : rs.needPassword ? (
          <form
            className="space-y-2"
            onSubmit={(e) => {
              e.preventDefault();
              void room.password(password).catch((err) => setError(friendlyError(err).text));
            }}
          >
            <p className="text-sm text-warning">{rs.needPassword.wrong ? 'Wrong password. Try again.' : INVITE_TEXT.passwordAfterAdmit}</p>
            <input
              type="password"
              maxLength={256}
              value={password}
              autoComplete="off"
              aria-label="Room password"
              onChange={(e) => setPassword(e.target.value)}
              className="w-full rounded-md border border-border bg-surface px-3 py-2 text-text focus:border-accent focus:outline-none"
            />
            <button type="submit" className="rounded-md bg-accent px-3 py-1.5 text-sm font-semibold text-on-accent" disabled={rs.needPassword.retryAfterMs > 0 && !password}>
              Enter
            </button>
          </form>
        ) : (
          <p className="flex items-center gap-2 text-sm text-muted" role="status">
            <Icon name="buffering" size="sm" />
            {INVITE_TEXT.waitingForAdmit}
          </p>
        )}
        <div className="flex justify-end">
          <button
            type="button"
            className="rounded-md px-4 py-2 text-muted hover:text-text"
            onClick={() => {
              void room.leave().catch(() => {});
              props.onCancel();
            }}
          >
            {ended ? 'Close' : 'Cancel'}
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="space-y-3">
      <label className="block text-sm">
        <span className="text-muted">Invite from the host</span>
        <textarea
          required
          rows={4}
          value={text}
          spellCheck={false}
          placeholder="SWINV1-..."
          data-testid="invite-input"
          onChange={(e) => setText(e.target.value)}
          className="mt-1 w-full resize-none rounded-md border border-border bg-surface px-2 py-1.5 font-mono text-xs break-all text-text focus:border-accent focus:outline-none"
        />
      </label>
      {previewError && (
        <p role="alert" className="text-sm text-danger" data-testid="invite-preview-error">
          {previewError}
        </p>
      )}
      {preview && (
        <div className="rounded-md border border-border bg-surface px-3 py-2 text-sm" data-testid="invite-preview">
          <p>
            <span className="text-muted">Room:</span> <span className="font-medium">{preview.roomName}</span>
          </p>
          <p className="text-xs text-muted">
            Host key {preview.hostId} · valid for {minutesLeft(preview.expiresUnix)} more minutes
            {preview.reinvite ? ' · a re-invite to your kept seat' : ''}
          </p>
        </div>
      )}
      {preview?.password && (
        <label className="block text-sm">
          <span className="text-muted">Room password</span>
          <input
            type="password"
            maxLength={256}
            value={password}
            autoComplete="off"
            onChange={(e) => setPassword(e.target.value)}
            className="mt-1 w-full rounded-md border border-border bg-surface px-3 py-2 text-text focus:border-accent focus:outline-none"
          />
        </label>
      )}
      {error && (
        <p role="alert" className="rounded-md border border-danger/40 bg-danger/10 px-3 py-2 text-sm text-danger">
          {error}
        </p>
      )}
      <div className="flex justify-end gap-2">
        <button type="button" className="rounded-md px-4 py-2 text-muted hover:text-text" onClick={props.onCancel}>
          Cancel
        </button>
        <button
          type="button"
          disabled={busy || !preview}
          onClick={() => void submit()}
          className="rounded-md bg-accent px-4 py-2 font-semibold text-on-accent hover:bg-accent-hover disabled:opacity-50"
        >
          {busy ? 'Preparing...' : 'Create my reply'}
        </button>
      </div>
    </div>
  );
}
