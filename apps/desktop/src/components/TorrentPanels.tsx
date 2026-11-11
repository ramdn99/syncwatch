import type { RoomMember, TorrentState } from '../lib/room';
import { fmtBytes, fmtEta, fmtRate, torrentStatusText, TORRENT_NOTICE } from '../lib/room';

/**
 * Torrent progress block (spec section 10, Host controls): this window's own download (speeds,
 * peers, ETA, progress) and, for the host, every member's status.
 */
export function TorrentBlock({ torrent, members, isHost }: { torrent: TorrentState | null; members: RoomMember[]; isHost: boolean }) {
  const any = torrent || members.some((m) => m.torrent);
  if (!any) return null;
  const pct = torrent && torrent.size > 0 ? Math.floor((100 * torrent.haveBytes) / torrent.size) : 0;
  return (
    <section className="space-y-2 text-xs" aria-label="Torrent" data-testid="torrent-block">
      <h3 className="text-muted">Torrent</h3>
      {torrent && (
        <div className="space-y-1 rounded bg-elevated px-2 py-1.5" data-testid="torrent-own">
          <div className="flex items-center justify-between">
            <span data-testid="torrent-phase">{torrentStatusText({ phase: torrent.phase, permille: pct * 10 }, torrent.buffering != null)}</span>
            <span className="tabular text-muted">
              {fmtBytes(torrent.haveBytes)} / {fmtBytes(torrent.size)}
            </span>
          </div>
          <div className="h-1 rounded-full bg-white/10" aria-hidden>
            <div className="h-1 rounded-full bg-accent" style={{ width: `${pct}%` }} />
          </div>
          {torrent.phase !== 'complete' && torrent.phase !== 'failed' && (
            <dl className="grid grid-cols-2 gap-x-2 tabular text-muted">
              <dt>Down</dt>
              <dd data-testid="torrent-down">{fmtRate(torrent.downBps)}</dd>
              <dt>Up</dt>
              <dd data-testid="torrent-up">{fmtRate(torrent.upBps)}</dd>
              <dt>Peers</dt>
              <dd data-testid="torrent-peers">{torrent.peers}</dd>
              <dt>Time left</dt>
              <dd>{fmtEta(torrent.etaSecs)}</dd>
            </dl>
          )}
          {torrent.phase === 'noPeers' && <p className="text-warning">Waiting for peers. Retrying automatically; what is downloaded keeps playing.</p>}
          {torrent.completeMessage && (
            <p className="text-success" data-testid="torrent-complete" role="status">
              {torrent.completeMessage}
            </p>
          )}
          {torrent.error && (
            <p className="text-danger" role="alert">
              {torrent.error}
            </p>
          )}
        </div>
      )}
      {isHost && (
        <ul className="space-y-0.5" aria-label="Members' downloads" data-testid="torrent-members">
          {members
            .filter((m) => !m.isYou)
            .map((m) => (
              <li key={m.id} className="flex justify-between gap-2">
                <span className="min-w-0 truncate">{m.name}</span>
                <span className="shrink-0 text-muted" data-testid="torrent-member-status">
                  {m.media === 'declined' ? 'declined' : (torrentStatusText(m.torrent, m.buffering) ?? 'not started')}
                </span>
              </li>
            ))}
        </ul>
      )}
    </section>
  );
}

/** Member: the host shared a magnet; nothing is downloaded before the member agrees. */
export function ConfirmMagnetDialog(props: { name: string; size: number; onAccept: () => void; onDecline: () => void }) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4" role="alertdialog" aria-labelledby="magnet-title">
      <div className="w-[30rem] space-y-3 rounded-xl border border-border bg-elevated p-5">
        <h2 id="magnet-title" className="font-brand text-lg font-bold">
          The host wants to play a torrent
        </h2>
        <p className="text-sm">
          <span className="font-medium" data-testid="magnet-name">
            {props.name}
          </span>{' '}
          <span className="text-muted">({fmtBytes(props.size)})</span>
        </p>
        <p className="text-sm text-muted">Your copy downloads from the torrent swarm and plays while it downloads. When it is complete, downloading and sharing stop.</p>
        <p className="text-xs text-warning">{TORRENT_NOTICE}</p>
        <div className="flex justify-end gap-2">
          <button type="button" className="rounded-md border border-border px-4 py-2 text-sm hover:bg-surface" onClick={props.onDecline}>
            Decline
          </button>
          <button type="button" className="rounded-md bg-accent px-4 py-2 text-sm font-semibold text-on-accent hover:bg-accent-hover" onClick={props.onAccept}>
            Download and play
          </button>
        </div>
      </div>
    </div>
  );
}

/** Seeking into a part that is not downloaded yet: the room waits (ready gate). */
export function BufferingOverlay({ buffering }: { buffering: { have: number; total: number } }) {
  const pct = buffering.total > 0 ? Math.floor((100 * buffering.have) / buffering.total) : 0;
  return (
    <div className="absolute left-1/2 top-1/2 z-10 -translate-x-1/2 -translate-y-1/2 rounded-lg bg-black/75 px-4 py-2 text-center text-sm" role="status" data-testid="torrent-buffering">
      <p className="font-medium">Buffering {pct}%</p>
      <p className="text-xs text-muted">
        {fmtBytes(buffering.have)} of {fmtBytes(buffering.total)} needed to continue
      </p>
    </div>
  );
}
