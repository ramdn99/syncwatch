import type { RoomState } from '../lib/room';

export interface UiErrorEntry {
  at: number;
  text: string;
}

/** Plain-text diagnostics (no chat content, no credentials, no IP addresses). */
export function diagnosticsText(rs: RoomState, uiErrors: UiErrorEntry[]): string {
  const lines: string[] = [];
  lines.push('SyncWatch room diagnostics');
  lines.push(`role: ${rs.isHost ? 'host' : 'member'}; phase: ${rs.phase ?? 'none'}; active: ${rs.active}`);
  lines.push(
    `signaling: ${rs.diag.signalingHost || '-'} (${rs.diag.signalingConnected ? 'connected' : 'not connected'}, reconnects ${rs.diag.signalingReconnects})`,
  );
  lines.push(`link to host: ${rs.isHost ? '-' : (rs.hostLink ?? '-')}`);
  for (const m of rs.members) {
    if (m.isYou) continue;
    lines.push(`member ${m.name}: ${m.connected ? (m.link ?? 'unknown') : 'dropped'}; media ${m.media}`);
  }
  lines.push('rejected:');
  for (const r of rs.diag.rejected) lines.push(`  ${r.who}: ${r.what} x${r.count}`);
  lines.push('ui errors:');
  for (const e of uiErrors) lines.push(`  ${new Date(e.at).toISOString()} ${e.text}`);
  lines.push('events:');
  for (const e of rs.diag.events) lines.push(`  +${e.t.toFixed(1)}s ${e.text}`);
  return lines.join('\n');
}

export function DiagnosticsDialog({ rs, uiErrors, onClose }: { rs: RoomState; uiErrors: UiErrorEntry[]; onClose: () => void }) {
  const linkRows = rs.members.filter((m) => !m.isYou);
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-6" role="dialog" aria-modal="true" aria-labelledby="diag-title">
      <div className="flex max-h-full w-[44rem] flex-col rounded-xl border border-border bg-elevated shadow-xl">
        <header className="flex items-center justify-between border-b border-border px-5 py-3">
          <h2 id="diag-title" className="font-brand text-lg font-bold">
            Diagnostics
          </h2>
          <div className="flex gap-2">
            <button
              type="button"
              className="rounded-md border border-border px-3 py-1.5 text-sm hover:border-accent"
              onClick={() => void navigator.clipboard?.writeText(diagnosticsText(rs, uiErrors))}
            >
              Copy diagnostics
            </button>
            <button type="button" className="rounded-md px-3 py-1.5 text-sm text-muted hover:text-text" onClick={onClose}>
              Close
            </button>
          </div>
        </header>
        <div className="min-h-0 space-y-5 overflow-y-auto px-5 py-4 text-sm select-text">
          <section aria-labelledby="diag-conn">
            <h3 id="diag-conn" className="mb-1 font-semibold">
              Connection
            </h3>
            <dl className="grid grid-cols-[10rem_1fr] gap-x-3 gap-y-0.5 text-muted">
              <dt>Role</dt>
              <dd className="text-text">{rs.isHost ? 'Host' : 'Member'}</dd>
              <dt>Room phase</dt>
              <dd className="text-text">{rs.phase ?? 'not in a room'}</dd>
              <dt>Signaling server</dt>
              <dd className="text-text">
                {rs.diag.signalingHost || '-'} · {rs.diag.signalingConnected ? 'connected' : 'not connected'} · {rs.diag.signalingReconnects} reconnects
              </dd>
              {!rs.isHost && (
                <>
                  <dt>Link to the host</dt>
                  <dd className="text-text">{rs.hostLink ?? '-'}</dd>
                </>
              )}
            </dl>
            {rs.isHost && linkRows.length > 0 && (
              <table className="mt-2 w-full text-left">
                <thead className="text-xs text-muted">
                  <tr>
                    <th className="font-normal">Member</th>
                    <th className="font-normal">Link</th>
                    <th className="font-normal">Media</th>
                  </tr>
                </thead>
                <tbody>
                  {linkRows.map((m) => (
                    <tr key={m.id}>
                      <td>{m.name}</td>
                      <td>{m.connected ? (m.link ?? '-') : 'dropped (seat kept)'}</td>
                      <td>{m.media}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </section>
          <section aria-labelledby="diag-rej">
            <h3 id="diag-rej" className="mb-1 font-semibold">
              Rejected requests
            </h3>
            {rs.diag.rejected.length === 0 && uiErrors.length === 0 ? (
              <p className="text-muted">None.</p>
            ) : (
              <ul className="space-y-0.5">
                {rs.diag.rejected.map((r, i) => (
                  <li key={i}>
                    <span className="font-medium">{r.who}</span>: {r.what} <span className="tabular text-muted">x{r.count}</span>
                  </li>
                ))}
                {uiErrors.map((e) => (
                  <li key={e.at} className="text-danger">
                    This window: {e.text}
                  </li>
                ))}
              </ul>
            )}
          </section>
          <section aria-labelledby="diag-ev">
            <h3 id="diag-ev" className="mb-1 font-semibold">
              Room events
            </h3>
            <ol className="tabular space-y-0.5 font-mono text-xs text-muted">
              {[...rs.diag.events].reverse().map((e, i) => (
                <li key={i}>
                  <span className="tabular text-muted">+{e.t.toFixed(1)} s</span> {e.text}
                </li>
              ))}
            </ol>
          </section>
        </div>
      </div>
    </div>
  );
}
