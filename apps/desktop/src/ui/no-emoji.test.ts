// The app's own text has no emoji or pictographs (Milestone 8). The CI step
// `node apps/desktop/scripts/emoji-scan.mjs` also scans the built bundle after the build.
import { execFileSync } from 'node:child_process';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

describe('no emoji in the app', () => {
  it('sources, strings files, Rust user-facing strings and scripts are clean', () => {
    const script = join(__dirname, '..', '..', 'scripts', 'emoji-scan.mjs');
    let out = '';
    try {
      out = execFileSync(process.execPath, [script, '--sources'], { encoding: 'utf8' });
    } catch (e) {
      out = String((e as { stderr?: string }).stderr ?? e);
    }
    expect(out).toMatch(/^PASS {2}emoji scan/);
  });
});
