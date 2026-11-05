import logoUrl from '../assets/logo-lockup.svg';
import type { ConnectionStatus } from '../lib/connection';
import { statusText } from '../strings/connection-help';

export function Home(props: {
  status: ConnectionStatus;
  onSetup: () => void;
  onSettings: () => void;
  onCreate: () => void;
  onJoin: () => void;
}) {
  const { status } = props;
  const blocked = !status.canConnect;
  return (
    <div className="flex h-full flex-col bg-bg">
      <header className="flex items-center justify-between px-6 py-4">
        <img src={logoUrl} alt="SyncWatch" className="h-7" draggable={false} />
        <button type="button" className="rounded-md px-3 py-1.5 text-sm text-muted hover:bg-surface hover:text-text" onClick={props.onSettings}>
          Settings
        </button>
      </header>
      {!status.verified && (
        <div className="mx-6 flex flex-wrap items-center gap-3 rounded-xl border border-accent/40 bg-elevated px-4 py-3" role="status" data-testid="setup-banner">
          <div className="flex-1">
            <p className="font-brand font-bold">{statusText.banner}</p>
            <p className="text-sm text-muted">{statusText.bannerBody}</p>
          </div>
          <button type="button" className="rounded-md bg-accent px-4 py-2 font-semibold text-on-accent hover:bg-accent-hover" onClick={props.onSetup}>
            {statusText.bannerButton}
          </button>
        </div>
      )}
      {status.warning && (
        <p className="mx-6 mt-3 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-warning" role="status">
          {statusText.warning[status.warning]}
        </p>
      )}
      <main className="flex flex-1 flex-col items-center justify-center gap-4 px-6">
        <h1 className="font-brand text-3xl font-bold">Watch together, in sync.</h1>
        <p className="max-w-md text-center text-muted">Everyone plays their own copy; SyncWatch keeps you together.</p>
        <div className="mt-4 flex gap-3">
          <button
            type="button"
            disabled={blocked}
            title={blocked ? statusText.bannerBody : undefined}
            className="rounded-lg bg-accent px-6 py-3 font-semibold text-on-accent hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-40"
            onClick={props.onCreate}
          >
            Create room
          </button>
          <button
            type="button"
            disabled={blocked}
            title={blocked ? statusText.bannerBody : undefined}
            className="rounded-lg border border-border px-6 py-3 font-semibold text-text hover:border-accent disabled:cursor-not-allowed disabled:opacity-40"
            onClick={props.onJoin}
          >
            Join room
          </button>
        </div>
        {status.devBypass && !status.verified && <p className="text-xs text-warning">Developer bypass active (debug build).</p>}
      </main>
    </div>
  );
}
