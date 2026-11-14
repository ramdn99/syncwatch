// Milestone 8 persistence: the UI keeps only the user's own layout and display name; nothing
// from a room survives (ADR 0006). Window size and position live in config/window.json
// (src-tauri/src/window_state.rs), preferences in config/prefs.json.
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

function files(dir: string, out: string[] = []): string[] {
  for (const n of readdirSync(dir)) {
    const p = join(dir, n);
    if (statSync(p).isDirectory()) files(p, out);
    else if (/\.tsx?$/.test(n) && !n.includes('.test.') && !p.includes(`${join('src', 'dev')}`)) out.push(p);
  }
  return out;
}

describe('what the UI stores', () => {
  it('only the layout and the display name go to browser storage', () => {
    const writers = files(join(__dirname, '..')).filter((f) => /localStorage\.setItem|sessionStorage\.setItem|indexedDB/.test(readFileSync(f, 'utf8')));
    const rel = (f: string) => 'src/' + f.split(/[\\/]/).slice(-2).join('/');
    expect(writers.map(rel).sort()).toEqual(['src/lib/layout.ts', 'src/lib/room.ts']);
    const room = readFileSync(join(__dirname, 'room.ts'), 'utf8');
    // In room.ts the only stored value is the display name.
    expect(room.match(/localStorage\.setItem\(([^,]+),/g)).toEqual(['localStorage.setItem(NAME_KEY,']);
  });
});
