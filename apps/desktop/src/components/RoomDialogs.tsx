import { useEffect, useState } from 'react';
import { endText, friendlyError, loadName, PASSWORD_IGNORED_TEXT, room, saveName } from '../lib/room';
import { ERRORS, isNoServer } from '../strings/errors';
import { INVITE_TEXT } from '../strings/invites';
import { InviteJoin } from './InviteJoin';

/** Create or join a room: display name, (code), optional password. */
export function RoomEntryDialog(props: { mode: 'create' | 'join'; onClose: () => void; onEntered: () => void; onConnectionSettings?: () => void }) {
  const [name, setName] = useState(loadName);
  const [code, setCode] = useState('');
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ignoredNotice, setIgnoredNotice] = useState(false);
  const [roomName, setRoomName] = useState('');
  // Release builds join with manual invites (ADR 0014); dev builds may use a signaling server.
  const [handshake, setHandshake] = useState<'manual' | 'signaling'>('manual');
  useEffect(() => {
    void room
      .handshake()
      .then(setHandshake)
      .catch(() => {});
  }, []);
  const create = props.mode === 'create';
  const manual = handshake === 'manual';
  // Manual join: once the reply is ready, the dialog shows only the reply step.
  const [replyStep, setReplyStep] = useState(false);

  const submit = async () => {
    // Joining with an invite happens inside InviteJoin ("Create my reply"). Enter in the name
    // field must not submit this form: it used to call the old room-code join and show the
    // room-code rules (Milestone 9 sandbox finding).
    if (!create && manual) return;
    setError(null);
    setBusy(true);
    try {
      saveName(name.trim());
      const outcome = create ? await room.create(name, password || null, manual ? roomName.trim() || null : null) : await room.join(name, code, password || null);
      if (outcome.status === 'ended') {
        setError(endText(outcome.ended) ?? 'Could not join the room.');
        return;
      }
      // Joining an open room with a password works; say that the password was not used.
      if (outcome.passwordIgnored) {
        setIgnoredNotice(true);
        return;
      }
      props.onEntered();
    } catch (e) {
      setError(friendlyError(e).text);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60" role="dialog" aria-modal="true" aria-labelledby="room-dialog-title">
      <form
        className="w-[26rem] space-y-4 rounded-xl border border-border bg-elevated p-5 shadow-xl"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <h2 id="room-dialog-title" className="font-brand text-lg font-bold">
          {create ? 'Create a room' : 'Join a room'}
        </h2>
        <label className="block text-sm" hidden={replyStep}>
          <span className="text-muted">Your name</span>
          <input
            autoFocus
            required
            maxLength={40}
            value={name}
            onChange={(e) => setName(e.target.value)}
            className="mt-1 w-full rounded-md border border-border bg-surface px-3 py-2 text-text focus:border-accent focus:outline-none"
          />
        </label>
        {!create && manual && (
          <>
            {!replyStep && <p className="text-xs text-muted">{INVITE_TEXT.joinIntro}</p>}
            <InviteJoin
              name={name}
              onReplyStep={() => setReplyStep(true)}
              onEntered={props.onEntered}
              onCancel={props.onClose}
              onNameRequired={() => {
                if (name.trim()) {
                  saveName(name.trim());
                  return true;
                }
                setError('Enter your name first.');
                return false;
              }}
            />
          </>
        )}
        {create && manual && (
          <label className="block text-sm">
            <span className="text-muted">Room name (shown to the people you invite)</span>
            <input
              maxLength={40}
              value={roomName}
              placeholder={name.trim() ? `${name.trim()}'s room` : 'Movie night'}
              onChange={(e) => setRoomName(e.target.value)}
              data-testid="room-name-input"
              className="mt-1 w-full rounded-md border border-border bg-surface px-3 py-2 text-text focus:border-accent focus:outline-none"
            />
            <span className="mt-1 block text-xs text-muted">{INVITE_TEXT.createIntro}</span>
          </label>
        )}
        {!create && !manual && (
          <label className="block text-sm">
            <span className="text-muted">Room code</span>
            <input
              required
              value={code}
              placeholder="XXXX-XXXX"
              spellCheck={false}
              onChange={(e) => setCode(e.target.value.toUpperCase())}
              className="mt-1 w-full rounded-md border border-border bg-surface px-3 py-2 font-brand tracking-[0.2em] text-text focus:border-accent focus:outline-none"
            />
          </label>
        )}
        <label className="block text-sm" hidden={!create && manual}>
          <span className="text-muted">{create ? 'Password (optional)' : 'Password (if the room has one)'}</span>
          <input
            type="password"
            maxLength={256}
            value={password}
            autoComplete="off"
            onChange={(e) => setPassword(e.target.value)}
            className="mt-1 w-full rounded-md border border-border bg-surface px-3 py-2 text-text focus:border-accent focus:outline-none"
          />
          {create && (
            <span className="mt-1 block text-xs text-muted">
              Without a password, everyone compares four words with you to make sure nobody is listening in.
            </span>
          )}
        </label>
        {error && isNoServer(error) && (
          <div role="alert" className="space-y-2 rounded-md border border-danger/40 bg-danger/10 px-3 py-2 text-sm text-danger" data-testid="error-noServer">
            <p>
              <span className="font-medium">{ERRORS.noServer.title}.</span> {ERRORS.noServer.text}
            </p>
            <p className="text-xs text-muted">{error}</p>
            <div className="flex gap-3">
              <button type="submit" className="text-text underline" disabled={busy}>
                {ERRORS.noServer.action!.label}
              </button>
              {props.onConnectionSettings && (
                <button type="button" className="text-text underline" onClick={props.onConnectionSettings}>
                  Connection settings...
                </button>
              )}
            </div>
          </div>
        )}
        {error && !isNoServer(error) && (
          <p role="alert" className="rounded-md border border-danger/40 bg-danger/10 px-3 py-2 text-sm text-danger">
            {error}
          </p>
        )}
        {ignoredNotice && (
          <div role="status" className="space-y-2 rounded-md border border-info/40 bg-info/10 px-3 py-2 text-sm text-info">
            <p>You joined the room. {PASSWORD_IGNORED_TEXT}</p>
            <button type="button" className="rounded-md bg-accent px-3 py-1.5 font-semibold text-on-accent" onClick={props.onEntered}>
              Continue to the room
            </button>
          </div>
        )}
        <div className="flex justify-end gap-2" hidden={ignoredNotice || (!create && manual)}>
          <button type="button" className="rounded-md px-4 py-2 text-muted hover:text-text" onClick={props.onClose}>
            Cancel
          </button>
          <button type="submit" disabled={busy} className="rounded-md bg-accent px-4 py-2 font-semibold text-on-accent hover:bg-accent-hover disabled:opacity-50">
            {busy ? 'Connecting...' : create ? 'Create room' : 'Join room'}
          </button>
        </div>
      </form>
    </div>
  );
}

/** The room has a password: ask for it (again), honoring the host's retry delay. */
export function PasswordPrompt(props: { wrong: boolean; retryAfterMs: number; onLeave: () => void }) {
  const [password, setPassword] = useState('');
  const [wait, setWait] = useState(Math.ceil(props.retryAfterMs / 1000));
  useEffect(() => {
    setWait(Math.ceil(props.retryAfterMs / 1000));
    if (props.retryAfterMs <= 0) return;
    const t = window.setInterval(() => setWait((w) => Math.max(0, w - 1)), 1000);
    return () => window.clearInterval(t);
  }, [props.retryAfterMs, props.wrong]);
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60" role="dialog" aria-modal="true" aria-labelledby="pw-title">
      <form
        className="w-96 space-y-4 rounded-xl border border-border bg-elevated p-5 shadow-xl"
        onSubmit={(e) => {
          e.preventDefault();
          if (wait > 0 || !password) return;
          void room.password(password);
          setPassword('');
        }}
      >
        <h2 id="pw-title" className="font-brand text-lg font-bold">
          This room has a password
        </h2>
        {props.wrong && <p className="text-sm text-danger">That password was not right.</p>}
        <input
          type="password"
          autoFocus
          value={password}
          aria-label="Room password"
          onChange={(e) => setPassword(e.target.value)}
          className="w-full rounded-md border border-border bg-surface px-3 py-2 text-text focus:border-accent focus:outline-none"
        />
        {wait > 0 && <p className="text-sm text-muted">You can try again in {wait} s.</p>}
        <div className="flex justify-end gap-2">
          <button type="button" className="rounded-md px-4 py-2 text-muted hover:text-text" onClick={props.onLeave}>
            Leave
          </button>
          <button type="submit" disabled={wait > 0 || !password} className="rounded-md bg-accent px-4 py-2 font-semibold text-on-accent hover:bg-accent-hover disabled:opacity-50">
            Join
          </button>
        </div>
      </form>
    </div>
  );
}

const fmtSize = (n: number) => (n >= 1e9 ? `${(n / 1e9).toFixed(2)} GB` : `${(n / 1e6).toFixed(1)} MB`);
const fmtDuration = (ms: number | null) => {
  if (ms == null) return null;
  const s = Math.round(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  return h > 0 ? `${h} h ${m} min` : `${m} min`;
};

/**
 * "Choose my file" (spec section 6): shown automatically ONCE when joining a local-file room or
 * when the source changes to a local file. Needs no permission; the video is never sent.
 */
export function MissingFilePanel(props: {
  file: { name: string; size: number; durationMs: number | null };
  status: 'verifying' | 'verified' | 'mismatch' | null;
  verifying: boolean;
  onChoose: () => void;
  onClose: () => void;
}) {
  const { file } = props;
  const duration = fmtDuration(file.durationMs);
  return (
    <div className="absolute inset-0 z-20 flex items-center justify-center bg-black/70 p-6" role="dialog" aria-modal="false" aria-labelledby="missing-title">
      <div className="w-[28rem] space-y-3 rounded-xl border border-border bg-elevated p-5 shadow-xl">
        <h2 id="missing-title" className="font-brand text-lg font-bold">
          Choose your copy of the video
        </h2>
        <p className="text-sm text-muted">
          Everyone plays their own file; the host never sends the video. Pick your copy of:
        </p>
        <div className="rounded-md bg-surface px-3 py-2">
          <p className="font-medium break-all">{file.name}</p>
          <p className="tabular text-xs text-muted">
            {fmtSize(file.size)}
            {duration && ` · ${duration}`}
          </p>
        </div>
        <p className="text-xs text-muted">You can also drop the file onto this window.</p>
        {props.verifying && <p className="text-sm text-warning">Checking your file...</p>}
        {props.status === 'mismatch' && (
          <p role="alert" className="rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-warning">
            Your file is not exactly the same as the host's. You can still watch; playback stays in sync, but scenes may not line up.
          </p>
        )}
        <div className="flex justify-end gap-2">
          <button type="button" className="rounded-md px-4 py-2 text-muted hover:text-text" onClick={props.onClose}>
            Later
          </button>
          <button type="button" className="rounded-md bg-accent px-4 py-2 font-semibold text-on-accent hover:bg-accent-hover" onClick={props.onChoose}>
            Choose my file
          </button>
        </div>
      </div>
    </div>
  );
}
