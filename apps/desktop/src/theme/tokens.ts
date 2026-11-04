// Dark theme tokens (spec section 4). The ONLY theme: no light mode, no toggle.
// styles.css mirrors these values as Tailwind @theme variables; tokens.test.ts keeps them in
// sync and enforces WCAG AA contrast.

export const tokens = {
  bg: '#0B0B0D',
  surface: '#141417',
  elevated: '#1C1C21',
  border: '#2A2A31',
  text: '#F2F2F4',
  muted: '#9A9AA5',
  disabled: '#5C5C66',
  accent: '#E8A33D',
  accentHover: '#F0B25A',
  accentPressed: '#C98A28',
  onAccent: '#141414',
  success: '#4CC38A',
  warning: '#E8A33D',
  danger: '#F26B6B',
  info: '#6AA8F7',
  letterbox: '#000000',
} as const;

export type TokenName = keyof typeof tokens;

function channel(c: number): number {
  const s = c / 255;
  return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
}

export function luminance(hex: string): number {
  const m = /^#([0-9a-f]{6})$/i.exec(hex);
  if (!m || !m[1]) throw new Error(`bad color ${hex}`);
  const n = parseInt(m[1], 16);
  return 0.2126 * channel((n >> 16) & 255) + 0.7152 * channel((n >> 8) & 255) + 0.0722 * channel(n & 255);
}

/** WCAG 2.x contrast ratio. */
export function contrast(a: string, b: string): number {
  const la = luminance(a);
  const lb = luminance(b);
  const [hi, lo] = la > lb ? [la, lb] : [lb, la];
  return (hi + 0.05) / (lo + 0.05);
}

/** Member color chips: chosen to keep >= 4.5:1 on the surface color. */
export const memberColors = ['#E8A33D', '#6AA8F7', '#4CC38A', '#F26B6B', '#C792EA', '#7FD1C7', '#F2A0C4', '#D4C46A'] as const;
