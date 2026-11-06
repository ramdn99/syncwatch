// Chat lines with video timestamps (spec section 9). Pure helpers, unit-tested.

import type { MockChatMessage } from '../mock/data';
import type { RoomChat, RoomState } from './room';

const HOUR_MS = 3_600_000;
/** Clicking a chip seeks this far before the stamped moment. */
export const CHIP_LEAD_IN_MS = 2_000;
/** "Near current time" filter: messages within this distance of the playhead. */
export const NEAR_WINDOW_MS = 120_000;

/**
 * Chip text: `mm:ss`, or `h:mm:ss` when the video is an hour or longer (or the stamp itself is
 * past one hour). Never a placeholder: callers show "no timestamp" for null.
 */
export function chipText(ms: number, durationMs: number | null): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  const pad = (n: number) => n.toString().padStart(2, '0');
  const long = ms >= HOUR_MS || (durationMs != null && durationMs >= HOUR_MS);
  return long ? `${h}:${pad(m)}:${pad(sec)}` : `${pad(m)}:${pad(sec)}`;
}

export interface ChatLine extends MockChatMessage {
  /** Chip label, or null when the message has no timestamp. */
  chip: string | null;
  /** Where clicking the chip seeks (ms, lead-in applied), or null when it cannot. */
  seekMs: number | null;
  /** Stamped for an earlier media (kept, not seekable). */
  stale: boolean;
  /** Raw stamp of the current media, for filters and markers. */
  currentMs: number | null;
}

/** Turns room chat into display lines for the current media and this person's rights. */
export function chatLines(chat: RoomChat[], rs: Pick<RoomState, 'mediaEpoch' | 'isHost' | 'myPermissions'>, durationMs: number | null): ChatLine[] {
  const canSeek = rs.isHost || rs.myPermissions.canSeek;
  return chat.map((c, i) => {
    const current = c.mediaEpoch === rs.mediaEpoch;
    const stamped = c.mediaTimeMs != null;
    return {
      id: c.divider ? `d${i}` : String(c.id),
      senderId: String(c.sender),
      senderName: c.name,
      isHost: c.isHost,
      colorIndex: c.sender,
      text: c.text,
      wallTime: c.wallMs ? new Date(c.wallMs).toTimeString().slice(0, 5) : '',
      mediaTimeMs: c.mediaTimeMs,
      divider: c.divider ?? undefined,
      chip: stamped ? chipText(c.mediaTimeMs!, current ? durationMs : null) : null,
      seekMs: stamped && current && canSeek ? Math.max(0, c.mediaTimeMs! - CHIP_LEAD_IN_MS) : null,
      stale: stamped && !current,
      currentMs: stamped && current ? c.mediaTimeMs : null,
    };
  });
}

/** Messages near the playhead (dividers and unstamped messages are kept). */
export function nearCurrent(lines: ChatLine[], posMs: number | null): ChatLine[] {
  if (posMs == null) return lines;
  return lines.filter((l) => l.divider || l.currentMs == null || Math.abs(l.currentMs - posMs) <= NEAR_WINDOW_MS);
}

/** Spoiler guard: a message stamped after my position stays hidden until I get there. */
export function spoilerUntil(line: ChatLine, posMs: number | null): number | null {
  if (line.currentMs == null || posMs == null) return null;
  return line.currentMs > posMs ? line.currentMs : null;
}

/** Seek-bar markers (seconds) for messages of the current media. */
export function markerSeconds(lines: ChatLine[]): number[] {
  return lines.filter((l) => l.currentMs != null && !l.divider).map((l) => l.currentMs! / 1000);
}
