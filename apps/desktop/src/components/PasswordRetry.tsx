// Room password entry and retry (Milestone 9 fix). Used by the invite Join dialog (after the
// host admitted) and by the in-room prompt. A wrong password keeps the connection: the member
// tries again on it after the host's delay (1, 2, 4, 8, 8 s; a 10-minute ban after 6 wrong
// ones). The countdown is visible, the button re-enables by itself, Enter always works when
// allowed, and the field stays editable and focused. Nothing is ever left busy.
import { useEffect, useRef, useState } from 'react';
import type { RoomState } from '../lib/room';

export const MAX_PASSWORD_ATTEMPTS = 6;

export type NeedPassword = NonNullable<RoomState['needPassword']>;

/** The text above the field for a prompt and the seconds left before the next attempt. */
export function passwordPromptText(np: NeedPassword, secondsLeft: number): string {
  const wait = secondsLeft > 0 ? ` Try again in ${secondsLeft} ${secondsLeft === 1 ? 'second' : 'seconds'}.` : ' Try again.';
  if (np.throttled) return `Too many tries in a short time.${wait}`;
  if (np.wrong) return `Wrong password.${wait}`;
  return 'This room has a password. Type it to join.';
}

export function PasswordRetry(props: { needPassword: NeedPassword; onSubmit: (password: string) => Promise<unknown>; autoFocus?: boolean }) {
  const np = props.needPassword;
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [left, setLeft] = useState(Math.ceil(np.retryAfterMs / 1000));
  const field = useRef<HTMLInputElement>(null);
  // Every prompt from the host (seq changes) restarts the countdown from its own delay.
  useEffect(() => {
    const until = Date.now() + np.retryAfterMs;
    const tick = () => setLeft(Math.max(0, Math.ceil((until - Date.now()) / 1000)));
    tick();
    setBusy(false);
    setPassword('');
    field.current?.focus();
    if (np.retryAfterMs <= 0) return;
    const t = window.setInterval(tick, 250);
    return () => window.clearInterval(t);
  }, [np.seq, np.retryAfterMs]);
  const blocked = left > 0;
  const submit = async () => {
    if (blocked || busy || !password) return;
    setBusy(true);
    setError(null);
    // The host's answer arrives as a new prompt (or the room): never stay busy, whatever the
    // call does.
    window.setTimeout(() => setBusy(false), 1500);
    try {
      await props.onSubmit(password);
    } catch (e) {
      setError(String(e));
    }
  };
  return (
    <form
      className="space-y-2"
      data-testid="password-retry"
      onSubmit={(e) => {
        e.preventDefault();
        e.stopPropagation();
        void submit();
      }}
    >
      <p className={np.wrong || np.throttled ? 'text-sm text-warning' : 'text-sm'} role="status" aria-live="polite" data-testid="password-status">
        {passwordPromptText(np, left)}
      </p>
      {np.attempts > 0 && (
        <p className="text-xs text-muted" data-testid="password-attempts">
          Wrong attempts: {np.attempts} of {MAX_PASSWORD_ATTEMPTS}. After {MAX_PASSWORD_ATTEMPTS}, the host blocks you for 10 minutes.
        </p>
      )}
      <input
        ref={field}
        type="password"
        maxLength={256}
        value={password}
        autoComplete="off"
        autoFocus={props.autoFocus ?? true}
        aria-label="Room password"
        onChange={(e) => setPassword(e.target.value)}
        className="w-full rounded-md border border-border bg-surface px-3 py-2 text-text focus:border-accent focus:outline-none"
      />
      {error && <p className="text-xs text-danger">{error}</p>}
      <button
        type="submit"
        data-testid="password-submit"
        disabled={blocked || busy || !password}
        className="rounded-md bg-accent px-3 py-1.5 text-sm font-semibold text-on-accent disabled:opacity-50"
      >
        {blocked ? `Try again in ${left} s` : busy ? 'Checking...' : 'Enter'}
      </button>
    </form>
  );
}
