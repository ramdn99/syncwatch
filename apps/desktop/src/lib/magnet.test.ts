import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { parseMagnet } from './magnet';

// The SAME vectors the authoritative Rust parser is tested with.
const vectors = JSON.parse(
  readFileSync(join(__dirname, '..', '..', '..', '..', 'crates', 'protocol', 'tests', 'magnet-vectors.json'), 'utf8'),
) as {
  valid: { input: string; infohash: string; dn: string | null; trackers: string[] }[];
  invalid: { input: string; error: string }[];
};

describe('magnet mirror matches the Rust parser vectors', () => {
  it.each(vectors.valid)('valid: $input', (v) => {
    const r = parseMagnet(v.input);
    expect(r.ok).toBe(true);
    if (r.ok) {
      expect(r.magnet.infohash).toBe(v.infohash);
      expect(r.magnet.dn).toBe(v.dn);
      expect(r.magnet.trackers).toEqual(v.trackers);
    }
  });

  it.each(vectors.invalid)('invalid: $input', (v) => {
    const r = parseMagnet(v.input);
    expect(r).toEqual({ ok: false, error: v.error });
  });

  it('rejects oversized input', () => {
    expect(parseMagnet('magnet:?xt=urn:btih:' + 'a'.repeat(9000))).toEqual({ ok: false, error: 'TooLong' });
  });
});
