import { useState } from 'react';
import { connection, type ConnectionStatus, type TestReport } from '../lib/connection';
import { settingsText, statusText } from '../strings/connection-help';
import { Results } from './ConnectionSetup';

/** Settings · Connection (Metered TURN/STUN): status, test, replace, remove. */
export function SettingsConnection({ status, onReplace }: { status: ConnectionStatus; onReplace: () => void }) {
  const [busy, setBusy] = useState(false);
  const [report, setReport] = useState<TestReport | null>(null);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const verifiedAt = status.verifiedAtUnix ? new Date(status.verifiedAtUnix * 1000).toLocaleString() : null;

  const test = async () => {
    setBusy(true);
    setReport(null);
    try {
      const o = await connection.retest();
      if (o && o.outcome === 'tested') setReport(o.report);
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
        <section className="rounded-xl border border-border bg-elevated p-5" aria-label={settingsText.title}>
          <p className={`font-medium ${status.verified ? 'text-success' : status.configured ? 'text-warning' : 'text-muted'}`}>
            {status.verified ? statusText.verified : status.configured ? statusText.notVerified : statusText.notConfigured}
            {verifiedAt && <span className="ml-2 text-xs text-muted">({verifiedAt})</span>}
          </p>
          {status.warning && <p className="mt-2 text-sm text-warning">{statusText.warning[status.warning]}</p>}
          {status.configured && (
            <>
              <p className="mt-3 text-xs text-muted">{settingsText.hostnames}</p>
              <ul className="mt-1 font-mono text-sm">
                {status.hostnames.map((h) => (
                  <li key={h}>{h}</li>
                ))}
              </ul>
              <p className="mt-3 text-sm text-muted">
                {status.mode === 'apiKey' ? 'API key' : 'ICE servers'}: <span className="font-mono">{settingsText.masked}</span>
              </p>
              <p className="text-xs text-muted">{status.portable ? settingsText.savedPortable : settingsText.saved}</p>
            </>
          )}
          <div className="mt-4 flex flex-wrap gap-2">
            {status.configured && (
              <button type="button" disabled={busy} className={btn} onClick={() => void test()}>
                {busy ? 'Testing...' : settingsText.test}
              </button>
            )}
            <button type="button" className={btn} onClick={onReplace}>
              {status.configured ? settingsText.replace : settingsText.help}
            </button>
            {status.configured && !confirmRemove && (
              <button type="button" className={`${btn} text-danger`} onClick={() => setConfirmRemove(true)}>
                {settingsText.remove}
              </button>
            )}
          </div>
          {confirmRemove && (
            <div className="mt-3 flex items-center gap-2 text-sm" role="alertdialog" aria-label={settingsText.removeConfirm}>
              <span>{settingsText.removeConfirm}</span>
              <button type="button" className={`${btn} text-danger`} onClick={() => void connection.remove().then(() => setConfirmRemove(false))}>
                {settingsText.removeYes}
              </button>
              <button type="button" className={btn} onClick={() => setConfirmRemove(false)}>
                {settingsText.removeNo}
              </button>
            </div>
          )}
          {report && (
            <div className="mt-4">
              <Results report={report} saved={false} />
            </div>
          )}
        </section>

    </>
  );
}

const btn = 'rounded-md border border-border bg-surface px-3 py-1.5 text-sm hover:border-accent disabled:opacity-50';
