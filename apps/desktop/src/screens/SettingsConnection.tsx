import { useState } from 'react';
import { connection, type ConnectionStatus, type TestReport } from '../lib/connection';
import { settingsText, statusText } from '../strings/connection-help';
import { Results } from './ConnectionSetup';

export function SettingsConnection({ status, onReplace, onBack }: { status: ConnectionStatus; onReplace: () => void; onBack: () => void }) {
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
    <div className="h-full overflow-y-auto bg-bg">
      <div className="mx-auto max-w-2xl px-6 py-8">
        <button type="button" className="mb-4 text-sm text-muted hover:text-text" onClick={onBack}>
          ← Back
        </button>
        <h1 className="font-brand text-2xl font-bold">Settings · {settingsText.title}</h1>

        <section className="mt-6 rounded-xl border border-border bg-elevated p-5">
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

        <section className="mt-6 rounded-xl border border-border bg-elevated p-5">
          <label className="flex items-start gap-3">
            <input type="checkbox" className="mt-1 accent-[#E8A33D]" checked={status.relayOnly} onChange={(e) => void connection.setRelayOnly(e.target.checked)} />
            <span>
              <span className="font-medium">{settingsText.relayOnly}</span>
              <span className="block text-sm text-muted">{settingsText.relayOnlyHint}</span>
            </span>
          </label>
          <p className="mt-3 text-xs text-muted">{settingsText.torrentNote}</p>
        </section>
      </div>
    </div>
  );
}

const btn = 'rounded-md border border-border bg-surface px-3 py-1.5 text-sm hover:border-accent disabled:opacity-50';
