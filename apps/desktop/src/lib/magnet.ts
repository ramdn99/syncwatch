// TypeScript MIRROR of crates/protocol/src/magnet.rs, for instant UI feedback. The Rust parser
// is authoritative (the app always confirms with `magnet_inspect` before using a magnet). Both
// are tested against crates/protocol/tests/magnet-vectors.json.

export type MagnetError =
  | 'NotMagnet' | 'TooLong' | 'NoInfohash' | 'BadInfohash' | 'ConflictingInfohash' | 'V2Only' | 'BadEncoding';

export interface Magnet {
  infohash: string; // 40 lower-case hex chars
  dn: string | null;
  trackers: string[];
}

export type MagnetResult = { ok: true; magnet: Magnet } | { ok: false; error: MagnetError };

const MAX_MAGNET_BYTES = 8 * 1024;
const MAX_TRACKERS = 32;
const MAX_DN_CHARS = 255;

function percentDecode(s: string, plusIsSpace: boolean): string | null {
  const bytes: number[] = [];
  for (let i = 0; i < s.length; ) {
    const c = s[i]!;
    if (c === '%') {
      const h = s.slice(i + 1, i + 3);
      if (!/^[0-9a-fA-F]{2}$/.test(h)) return null;
      bytes.push(parseInt(h, 16));
      i += 3;
    } else if (c === '+' && plusIsSpace) {
      bytes.push(0x20);
      i += 1;
    } else {
      const enc = new TextEncoder().encode(c);
      bytes.push(...enc);
      i += c.length;
    }
  }
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(new Uint8Array(bytes));
  } catch {
    return null;
  }
}

function parseInfohash(s: string): string | null {
  if (s.length === 40) return /^[0-9a-fA-F]{40}$/.test(s) ? s.toLowerCase() : null;
  if (s.length === 32) {
    const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567';
    let bits = 0;
    let acc = 0;
    let out = '';
    for (const ch of s.toUpperCase()) {
      const v = alphabet.indexOf(ch);
      if (v < 0) return null;
      acc = (acc << 5) | v;
      bits += 5;
      if (bits >= 8) {
        bits -= 8;
        out += ((acc >> bits) & 0xff).toString(16).padStart(2, '0');
      }
      acc &= 0xff;
    }
    return out;
  }
  return null;
}

const isControl = (ch: string) => {
  const c = ch.codePointAt(0)!;
  return c < 0x20 || (c >= 0x7f && c < 0xa0);
};

export function parseMagnet(input: string): MagnetResult {
  const s = input.trim();
  if (new TextEncoder().encode(s).length > MAX_MAGNET_BYTES) return { ok: false, error: 'TooLong' };
  if (s.slice(0, 8).toLowerCase() !== 'magnet:?') return { ok: false, error: 'NotMagnet' };
  let infohash: string | null = null;
  let sawV2 = false;
  let dn: string | null = null;
  const trackers: string[] = [];
  for (const pair of s.slice(8).split('&')) {
    if (!pair) continue;
    const eq = pair.indexOf('=');
    const k = (eq >= 0 ? pair.slice(0, eq) : pair).toLowerCase();
    const v = eq >= 0 ? pair.slice(eq + 1) : '';
    if (k === 'xt' || k === 'xt.1' || k === 'xt.2') {
      const d = percentDecode(v, false);
      if (d === null) return { ok: false, error: 'BadEncoding' };
      const lower = d.toLowerCase();
      if (lower.startsWith('urn:btih:')) {
        const ih = parseInfohash(d.slice(9));
        if (!ih) return { ok: false, error: 'BadInfohash' };
        if (infohash && infohash !== ih) return { ok: false, error: 'ConflictingInfohash' };
        infohash = ih;
      } else if (lower.startsWith('urn:btmh:')) {
        sawV2 = true;
      }
    } else if (k === 'dn') {
      const d = percentDecode(v, true);
      if (d === null) return { ok: false, error: 'BadEncoding' };
      const clean = Array.from(d).filter((ch) => !isControl(ch)).slice(0, MAX_DN_CHARS).join('').trim();
      if (clean) dn = clean;
    } else if (k === 'tr' && trackers.length < MAX_TRACKERS) {
      const t = percentDecode(v, false);
      if (t === null) return { ok: false, error: 'BadEncoding' };
      if (t.length <= 512 && !trackers.includes(t)) trackers.push(t);
    }
  }
  if (infohash) return { ok: true, magnet: { infohash, dn, trackers } };
  return { ok: false, error: sawV2 ? 'V2Only' : 'NoInfohash' };
}
