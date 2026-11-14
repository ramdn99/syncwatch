import { useState } from 'react';
import { RichText } from '../components/RichText';
import { connection, looksLikeCredentialsUrl, type ConnectionStatus, type TestOutcome, type TestReport } from '../lib/connection';
import { Icon } from '../ui/icons';
import {
  fetchErrors,
  inputErrors,
  parseErrors,
  probeErrors,
  resultText,
  setupText,
  type Rich,
} from '../strings/connection-help';

type Tab = 'apiKey' | 'iceServers';

export function ConnectionSetup({ status, onDone, onBack }: { status: ConnectionStatus; onDone: () => void; onBack: () => void }) {
  const [tab, setTab] = useState<Tab>(status.mode === 'iceServers' ? 'iceServers' : 'apiKey');
  const [app, setApp] = useState('');
  const [key, setKey] = useState('');
  const [url, setUrl] = useState('');
  const [ice, setIce] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<Rich | null>(null);
  const [info, setInfo] = useState<string | null>(null);
  const [report, setReport] = useState<{ report: TestReport; saved: boolean } | null>(null);

  const splitUrl = async (text: string) => {
    setUrl(text);
    setError(null);
    setInfo(null);
    if (!looksLikeCredentialsUrl(text)) return;
    try {
      const r = await connection.splitUrl(text);
      if (r) {
        setApp(r.app);
        setKey(r.key);
        setUrl('');
        setInfo(setupText.apiKey.urlSplit);
      }
    } catch (e) {
      setError(inputErrors[String(e)] ?? inputErrors.badUrl!);
    }
  };

  const show = (o: TestOutcome | undefined) => {
    if (!o) return;
    switch (o.outcome) {
      case 'inputError':
        setError(inputErrors[o.error] ?? inputErrors.badKey!);
        break;
      case 'parseError':
        setError(parseErrors[o.code] ?? parseErrors.syntax!);
        break;
      case 'fetchError':
        setError(fetchErrors[o.error.kind] ?? fetchErrors.server!);
        break;
      case 'storeError':
        setError(['The Windows credential store is not available, so your setup could not be saved.']);
        break;
      case 'tested':
        setReport({ report: o.report, saved: o.saved });
        if (o.saved) setTimeout(onDone, 1800);
        break;
    }
  };

  const test = async () => {
    setBusy(true);
    setError(null);
    setReport(null);
    setInfo(null);
    try {
      show(tab === 'apiKey' ? await connection.testApiKey(app, key) : await connection.testIce(ice));
    } finally {
      setBusy(false);
    }
  };

  const canTest = !busy && (tab === 'apiKey' ? app.trim() !== '' && key.trim() !== '' : ice.trim() !== '');
  const card = setupText[tab];

  return (
    <div className="h-full overflow-y-auto bg-bg">
      <div className="mx-auto max-w-3xl px-6 py-8">
        <button type="button" className="mb-4 inline-flex items-center gap-1 text-sm text-muted hover:text-text" onClick={onBack}>
          <Icon name="back" />
          Back
        </button>
        <h1 className="font-brand text-2xl font-bold">{setupText.title}</h1>
        <p className="mt-2 text-muted">{setupText.intro}</p>
        {status.devBypass && <p className="mt-2 text-sm text-warning">{setupText.devBypass}</p>}

        <div className="mt-6 flex gap-1 border-b border-border" role="tablist">
          {(['apiKey', 'iceServers'] as const).map((t) => (
            <button
              key={t}
              type="button"
              role="tab"
              aria-selected={tab === t}
              className={`-mb-px border-b-2 px-4 py-2 ${tab === t ? 'border-accent text-text' : 'border-transparent text-muted hover:text-text'}`}
              onClick={() => {
                setTab(t);
                setError(null);
                setReport(null);
              }}
            >
              {setupText.tabs[t]} <span className="text-xs text-muted">({setupText.tabHints[t]})</span>
            </button>
          ))}
        </div>

        <section className="mt-6 rounded-xl border border-border bg-elevated p-5" aria-label={card.heading}>
          <h2 className="font-brand text-lg font-bold">{card.heading}</h2>
          <ol className="mt-3 list-decimal space-y-1.5 pl-5 text-muted">
            {card.steps.map((s, i) => (
              <li key={i}>
                <RichText value={s} />
              </li>
            ))}
          </ol>
          <button
            type="button"
            className="mt-4 rounded-md border border-info px-3 py-1.5 text-sm text-info underline hover:bg-surface"
            onClick={() => void connection.openHelp('metered')}
          >
            {setupText.openSite}
          </button>
        </section>

        <section className="mt-6 space-y-3">
          {tab === 'apiKey' ? (
            <>
              <div className="grid gap-3 sm:grid-cols-2">
                <Field label={setupText.apiKey.appLabel}>
                  <input className={inputCls} value={app} placeholder={setupText.apiKey.appPlaceholder} autoComplete="off" spellCheck={false} onChange={(e) => setApp(e.target.value)} />
                </Field>
                <Field label={setupText.apiKey.keyLabel}>
                  <input className={inputCls} type="password" value={key} placeholder={setupText.apiKey.keyPlaceholder} autoComplete="off" spellCheck={false} onChange={(e) => setKey(e.target.value)} />
                </Field>
              </div>
              <Field label={setupText.apiKey.urlLabel}>
                <input className={inputCls} type="password" value={url} placeholder={setupText.apiKey.urlPlaceholder} autoComplete="off" spellCheck={false} onChange={(e) => void splitUrl(e.target.value)} />
              </Field>
            </>
          ) : (
            <Field label={setupText.iceServers.pasteLabel}>
              <textarea className={`${inputCls} h-48 font-mono text-xs`} value={ice} placeholder={setupText.iceServers.pastePlaceholder} spellCheck={false} onChange={(e) => setIce(e.target.value)} />
            </Field>
          )}
          <button type="button" disabled={!canTest} className="rounded-md bg-accent px-5 py-2 font-semibold text-on-accent hover:bg-accent-hover active:bg-accent-pressed disabled:cursor-not-allowed disabled:opacity-50" onClick={() => void test()}>
            {busy ? setupText.testing : setupText.testButton}
          </button>
          {info && <p className="text-sm text-success">{info}</p>}
          {error && (
            <p role="alert" className="rounded-md border border-danger/40 bg-danger/10 px-3 py-2 text-sm text-danger">
              <RichText value={error} />
            </p>
          )}
          {report && <Results report={report.report} saved={report.saved} />}
        </section>

        <p className="mt-8 text-xs text-muted">
          {setupText.note.join(' ')} {setupText.freePlanEnough}
        </p>
      </div>
    </div>
  );
}

const inputCls = 'w-full rounded-md border border-border bg-surface px-3 py-2 text-sm text-text placeholder:text-muted focus:border-accent focus:outline-none';

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="block">
      <span className="mb-1 block text-xs text-muted">{label}</span>
      {children}
    </label>
  );
}

export function Results({ report, saved }: { report: TestReport; saved: boolean }) {
  return (
    <div className="rounded-xl border border-border bg-surface p-4" data-testid="test-results">
      <p className={`font-medium ${report.verified ? 'text-success' : 'text-danger'}`}>{report.verified ? resultText.verified : resultText.notVerified}</p>
      <ul className="mt-3 space-y-1 text-sm">
        {report.results.map((r, i) => (
          <li key={i} className="flex flex-wrap items-baseline gap-x-3">
            <span className={r.ok ? 'text-success' : 'text-danger'}>{r.ok ? resultText.ok : resultText.fail}</span>
            <span className="font-mono text-xs">{r.label}</span>
            <span className="text-muted">{r.kind === 'relay' ? resultText.relay : resultText.stun}</span>
            <span className="tabular text-muted">{r.latencyMs != null ? `${r.latencyMs} ms` : ''}</span>
            {r.error && <span className="text-danger">{probeErrors[r.error] ?? r.error}</span>}
          </li>
        ))}
      </ul>
      {report.allocations > 0 && report.allocations === report.allocationsReleased && <p className="mt-2 text-xs text-muted">{resultText.released}</p>}
      {saved && <p className="mt-2 text-sm text-success">{setupText.saved}</p>}
      {!saved && !report.verified && <p className="mt-2 text-sm text-muted">{setupText.notSaved}</p>}
    </div>
  );
}
