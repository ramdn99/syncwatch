// Milestone 8 design-system checks over what the UI actually uses:
// - every text color class, on every background it can sit on (the class's own bg-* if any,
//   composed over each panel background), meets WCAG AA (4.5:1; 3:1 for large text);
// - control borders (inputs, selects, text areas) reach 3:1 as UI components;
// - visible focus rings; no light theme; no remote resources; no white flash at startup.
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { contrast, tokens } from './tokens';

const SRC = join(__dirname, '..');
const APP = join(SRC, '..');

function files(dir: string, out: string[] = []): string[] {
  for (const n of readdirSync(dir)) {
    const p = join(dir, n);
    if (statSync(p).isDirectory()) files(p, out);
    else if (/\.(tsx|ts|css)$/.test(n) && !/\.test\./.test(n)) out.push(p);
  }
  return out;
}
const sources = files(SRC);

/** Token name -> hex, plus the plain colors Tailwind gives (black, white). */
const COLORS: Record<string, string> = {
  bg: tokens.bg,
  surface: tokens.surface,
  elevated: tokens.elevated,
  border: tokens.border,
  text: tokens.text,
  muted: tokens.muted,
  disabled: tokens.disabled,
  accent: tokens.accent,
  'accent-hover': tokens.accentHover,
  'accent-pressed': tokens.accentPressed,
  'on-accent': tokens.onAccent,
  success: tokens.success,
  warning: tokens.warning,
  danger: tokens.danger,
  info: tokens.info,
  black: '#000000',
  white: '#ffffff',
};
const PANELS = [tokens.bg, tokens.surface, tokens.elevated];

function hex(h: string): [number, number, number] {
  const n = parseInt(h.slice(1), 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}
function toHex([r, g, b]: [number, number, number]): string {
  return '#' + [r, g, b].map((v) => Math.round(v).toString(16).padStart(2, '0')).join('');
}
/** `fg` at `alpha` over `under`. */
function over(fg: string, alpha: number, under: string): string {
  const a = hex(fg);
  const b = hex(under);
  return toHex([0, 1, 2].map((i) => a[i]! * alpha + b[i]! * (1 - alpha)) as [number, number, number]);
}

/** Every class string in the sources (string and template literals that contain a class). */
function classStrings(): { file: string; cls: string }[] {
  const out: { file: string; cls: string }[] = [];
  for (const f of sources.filter((p) => p.endsWith('.tsx'))) {
    const src = readFileSync(f, 'utf8');
    for (const m of src.matchAll(/className=\{?[`'"]([^`'"]+)[`'"]/g)) out.push({ file: f, cls: m[1]! });
    for (const m of src.matchAll(/[`'"]((?:[\w:/.[\]-]+ )*(?:text|bg)-[\w/.[\]-]+(?: [\w:/.[\]-]+)*)[`'"]/g)) out.push({ file: f, cls: m[1]! });
  }
  return out;
}

/** Base (no state variant) class tokens of a class string. */
/** Base classes (no state variant), with placeholder colors checked like text. */
const base = (cls: string) =>
  cls
    .split(/\s+/)
    .filter((c) => c && (!c.includes(':') || c.startsWith('placeholder:')))
    .map((c) => c.replace(/^placeholder:/, ''));
const fgOf = (cls: string) => base(cls).map((c) => /^text-([a-z-]+)(?:\/(\d+))?$/.exec(c)).find((m) => m && m[1]! in COLORS);
const bgOf = (cls: string) => base(cls).map((c) => /^bg-([a-z-]+)(?:\/(\d+))?$/.exec(c)).find((m) => m && m[1]! in COLORS);
const large = (cls: string) => /\btext-(lg|xl|2xl|3xl|4xl)\b/.test(cls);

describe('design system in use (WCAG AA)', () => {
  it('every text color on every background it can sit on', () => {
    const failures: string[] = [];
    let checked = 0;
    for (const { file, cls } of classStrings()) {
      const fg = fgOf(cls);
      if (!fg) continue;
      checked++;
      const name = fg[1]!;
      const fgAlpha = fg[2] ? Number(fg[2]) / 100 : 1;
      const bg = bgOf(cls);
      const need = large(cls) ? 3 : 4.5;
      for (const panel of PANELS) {
        const under = bg ? over(COLORS[bg[1]!]!, bg[2] ? Number(bg[2]) / 100 : 1, panel) : panel;
        const text = fgAlpha < 1 ? over(COLORS[name]!, fgAlpha, under) : COLORS[name]!;
        const c = contrast(text, under);
        if (c < need) failures.push(`${file.replace(SRC, 'src')}: "${cls}" text-${name} on ${under} = ${c.toFixed(2)} (< ${need})`);
      }
    }
    expect([...new Set(failures)]).toEqual([]);
    expect(checked, 'class strings with a text color').toBeGreaterThan(100);
  });

  it('bundled fonts only (no system font names in the stacks)', () => {
    const css = readFileSync(join(SRC, 'styles.css'), 'utf8');
    expect(css).not.toMatch(/Segoe UI|system-ui|Arial|Helvetica/);
    expect(css).toMatch(/src: url\('\/fonts\//);
  });

  it('control borders reach 3:1 against every panel (UI components)', () => {
    const css = readFileSync(join(SRC, 'styles.css'), 'utf8');
    const m = /--color-border-control:\s*(#[0-9a-f]{6})/i.exec(css);
    expect(m, '--color-border-control is defined').not.toBeNull();
    for (const panel of PANELS) expect(contrast(m![1]!, panel), panel).toBeGreaterThanOrEqual(3);
    expect(css).toMatch(/input:not\(\[type='checkbox'\]\):not\(\[type='radio'\]\)[^{]*\{[^}]*border-color:\s*var\(--color-border-control\)/);
    expect(css).toMatch(/accent-color:\s*var\(--color-accent\)/);
  });

  it('visible focus rings (2 px, accent, 3:1 on every panel)', () => {
    const css = readFileSync(join(SRC, 'styles.css'), 'utf8');
    expect(css).toMatch(/:focus-visible\s*\{[^}]*outline:\s*2px solid var\(--color-accent\)/);
    for (const panel of PANELS) expect(contrast(tokens.accent, panel)).toBeGreaterThanOrEqual(3);
  });

  it('no light theme anywhere in the UI code', () => {
    for (const f of sources) {
      const s = readFileSync(f, 'utf8');
      expect(s, f).not.toMatch(/prefers-color-scheme|data-theme=|theme-light|lightTheme/);
    }
  });

  it('no remote resources (fonts, scripts, styles, images are bundled)', () => {
    const html = readFileSync(join(APP, 'index.html'), 'utf8');
    expect(html).not.toMatch(/(src|href)=["']https?:/);
    for (const f of sources) {
      const s = readFileSync(f, 'utf8');
      expect(s, f).not.toMatch(/url\(\s*['"]?https?:|@import\s+['"]?https?:|<(img|script|link)[^>]+(src|href)=["']https?:/);
    }
    const conf = JSON.parse(readFileSync(join(APP, 'src-tauri', 'tauri.conf.json'), 'utf8')) as { app: { security: { csp: Record<string, string> } } };
    for (const [k, v] of Object.entries(conf.app.security.csp)) {
      if (k === 'connect-src') continue; // ipc only
      expect(v, k).not.toMatch(/https?:/);
    }
  });

  it('no white flash: hidden until the first paint, dark everywhere before CSS loads', () => {
    const conf = JSON.parse(readFileSync(join(APP, 'src-tauri', 'tauri.conf.json'), 'utf8')) as { app: { windows: { visible: boolean; transparent: boolean; theme: string }[] } };
    const w = conf.app.windows[0]!;
    expect([w.visible, w.transparent, w.theme]).toEqual([false, true, 'Dark']);
    expect(readFileSync(join(APP, 'index.html'), 'utf8')).toMatch(/<meta name="color-scheme" content="dark"/);
    expect(readFileSync(join(SRC, 'styles.css'), 'utf8')).toMatch(/color-scheme:\s*dark/);
  });
});
