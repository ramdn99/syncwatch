import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative } from 'node:path';
import { describe, expect, it } from 'vitest';
import { fetchErrors, parseErrors, probeErrors, setupText } from './connection-help';

const repo = join(__dirname, '..', '..', '..', '..');
const MENU_NAMES = ['TURN Server', 'TURN Credentials', 'Show API Key', 'Show ICE Servers Array'];

function files(dir: string, exts: string[]): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    if (name === 'node_modules' || name === 'target' || name === 'dist' || name === 'gen') continue;
    const p = join(dir, name);
    if (statSync(p).isDirectory()) out.push(...files(p, exts));
    else if (exts.some((e) => name.endsWith(e))) out.push(p);
  }
  return out;
}

describe('connection help strings', () => {
  it('Metered menu names appear ONLY in connection-help.ts (not in UI code or Rust sources)', () => {
    const sources = [
      ...files(join(repo, 'apps', 'desktop', 'src'), ['.ts', '.tsx']),
      ...files(join(repo, 'apps', 'desktop', 'src-tauri', 'src'), ['.rs']),
      ...files(join(repo, 'crates'), ['.rs']),
    ].filter((f) => !/connection-help(\.test)?\.ts$/.test(f));
    expect(sources.length).toBeGreaterThan(20);
    const offenders = sources.filter((f) => MENU_NAMES.some((n) => readFileSync(f, 'utf8').includes(n)));
    expect(offenders.map((f) => relative(repo, f))).toEqual([]);
  });

  it('help cards follow the spec steps', () => {
    const flat = (r: (string | { b: string })[]) => r.map((x) => (typeof x === 'string' ? x : x.b)).join('');
    const a = setupText.apiKey.steps.map(flat);
    expect(a[2]).toBe('In the left section open TURN Server.');
    expect(a[3]).toBe('Scroll down to TURN Credentials and press Show API Key.');
    const b = setupText.iceServers.steps.map(flat);
    expect(b[3]).toBe('Scroll down to TURN Credentials and press Show ICE Servers Array.');
    // UI names are bold.
    expect(setupText.apiKey.steps[3]).toContainEqual({ b: 'Show API Key' });
  });

  it('unverified lines are marked TODO-VERIFY', () => {
    const src = readFileSync(join(__dirname, 'connection-help.ts'), 'utf8');
    const lines = src.split('\n');
    const markedBefore = (needle: string) => {
      const i = lines.findIndex((l) => l.includes(needle));
      return i > 0 && lines[i - 1]!.includes('TODO-VERIFY');
    };
    expect(markedBefore('freePlanEnough')).toBe(true);
    expect(markedBefore("server: ['Metered returned an error")).toBe(true);
    expect(markedBefore('quota:')).toBe(true);
  });

  it('every error code from Rust has a message', () => {
    for (const k of ['badKey', 'server', 'dns', 'refused', 'timeout', 'tls', 'offline', 'badResponse', 'badRedirect', 'input']) expect(fetchErrors[k]).toBeDefined();
    for (const k of ['empty', 'tooLarge', 'noArray', 'syntax', 'noServers', 'tooMany', 'badUrl', 'badScheme', 'missingCredential', 'tooLong']) expect(parseErrors[k]).toBeDefined();
    for (const k of ['dns', 'refused', 'timeout', 'tls', 'badCredentials', 'quota', 'capacity', 'server', 'protocol', 'offline']) expect(probeErrors[k]).toBeDefined();
  });

  it('no real-looking secrets or app names in the strings', () => {
    const src = readFileSync(join(__dirname, 'connection-help.ts'), 'utf8');
    expect(src).toContain('<app-name>');
    expect(src).toContain('<API_KEY>');
    expect(src).not.toMatch(/apiKey=[A-Za-z0-9]{8,}/);
  });
});
