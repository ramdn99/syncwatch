import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { contrast, memberColors, tokens } from './tokens';

const AA = 4.5;
const AA_LARGE_UI = 3;

describe('dark theme tokens', () => {
  it('match the spec values and styles.css', () => {
    const css = readFileSync(join(__dirname, '..', 'styles.css'), 'utf8').toLowerCase();
    const pairs: Record<string, string> = {
      bg: '--color-bg', surface: '--color-surface', elevated: '--color-elevated', border: '--color-border',
      text: '--color-text', muted: '--color-muted', disabled: '--color-disabled', accent: '--color-accent',
      accentHover: '--color-accent-hover', accentPressed: '--color-accent-pressed', onAccent: '--color-on-accent',
      borderControl: '--color-border-control', success: '--color-success', warning: '--color-warning', danger: '--color-danger', info: '--color-info',
    };
    for (const [name, cssVar] of Object.entries(pairs)) {
      const value = tokens[name as keyof typeof tokens].toLowerCase();
      expect(css, `${cssVar} must be ${value}`).toContain(`${cssVar}: ${value};`);
    }
  });

  it('text, muted and status colors meet WCAG AA on every background', () => {
    for (const bg of [tokens.bg, tokens.surface, tokens.elevated]) {
      for (const fg of [tokens.text, tokens.muted, tokens.accent, tokens.success, tokens.danger, tokens.info]) {
        expect(contrast(fg, bg), `${fg} on ${bg}`).toBeGreaterThanOrEqual(AA);
      }
    }
  });

  it('on-accent text meets AA on accent in all states', () => {
    for (const a of [tokens.accent, tokens.accentHover, tokens.accentPressed]) {
      expect(contrast(tokens.onAccent, a), `on-accent on ${a}`).toBeGreaterThanOrEqual(AA);
    }
  });

  it('borders and focus rings are visible as UI components (3:1 for accent focus)', () => {
    expect(contrast(tokens.accent, tokens.bg)).toBeGreaterThanOrEqual(AA_LARGE_UI);
  });

  it('member color chips are readable on the surface', () => {
    for (const c of memberColors) expect(contrast(c, tokens.surface), c).toBeGreaterThanOrEqual(AA);
  });

  it('there is no light theme', () => {
    const css = readFileSync(join(__dirname, '..', 'styles.css'), 'utf8');
    expect(css).not.toMatch(/prefers-color-scheme/);
    expect(css).toMatch(/color-scheme:\s*dark/);
  });
});
