import type { RoomState } from './room';
import type { ErrorId } from '../strings/errors';

/** The error states that apply to this window in the room right now (Milestone 8). */
export function roomErrors(rs: RoomState, slowLink: boolean): ErrorId[] {
  const out: ErrorId[] = [];
  if (rs.loadError) out.push('unsupportedFile');
  const me = rs.members.find((m) => m.isYou);
  if (me && me.media === 'ready' && !me.tracksInSync) out.push('trackUnresolved');
  if (rs.torrent?.phase === 'noPeers') out.push('noPeers');
  if (rs.torrent?.error && /not enough free disk space|disk is full|cache is full/i.test(rs.torrent.error)) out.push('diskFull');
  if (slowLink) out.push('firewall');
  return out;
}
