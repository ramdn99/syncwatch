// Pure layout math for the three-column room view (spec section 7).

export interface LayoutPrefs {
  leftWidth: number;
  rightWidth: number;
  leftCollapsed: boolean;
  rightCollapsed: boolean;
  membersCollapsed: boolean;
  hostCollapsed: boolean;
}

export const LIMITS = {
  left: { min: 240, max: 520 },
  right: { min: 260, max: 520 },
  centerMin: 420,
  resizer: 4,
} as const;

export const DEFAULT_PREFS: LayoutPrefs = {
  leftWidth: 320,
  rightWidth: 320,
  leftCollapsed: false,
  rightCollapsed: false,
  membersCollapsed: false,
  hostCollapsed: false,
};

export interface Columns {
  left: number;
  right: number;
  center: number;
}

export const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

/**
 * Effective column widths for a window width. The video (center) shrinks first, down to
 * LIMITS.centerMin; only then do the side columns give way (proportionally, never below
 * their minimum). Fullscreen hides both side columns.
 */
export function computeColumns(total: number, p: LayoutPrefs, fullscreen: boolean): Columns {
  if (fullscreen) return { left: 0, right: 0, center: Math.max(0, total) };
  let left = p.leftCollapsed ? 0 : clamp(p.leftWidth, LIMITS.left.min, LIMITS.left.max);
  let right = p.rightCollapsed ? 0 : clamp(p.rightWidth, LIMITS.right.min, LIMITS.right.max);
  const resizers = (left ? LIMITS.resizer : 0) + (right ? LIMITS.resizer : 0);
  let center = total - left - right - resizers;
  if (center < LIMITS.centerMin) {
    let deficit = LIMITS.centerMin - center;
    const slackL = left ? left - LIMITS.left.min : 0;
    const slackR = right ? right - LIMITS.right.min : 0;
    const slack = slackL + slackR;
    if (slack > 0) {
      const take = Math.min(deficit, slack);
      const takeL = Math.round((take * slackL) / slack);
      left -= takeL;
      right -= take - takeL;
      deficit -= take;
    }
    center = total - left - right - resizers;
  }
  return { left, right, center: Math.max(0, center) };
}

/** New width while dragging a resizer, respecting both column limits and the center minimum. */
export function dragWidth(
  side: 'left' | 'right',
  startWidth: number,
  deltaX: number,
  total: number,
  otherWidth: number,
): number {
  const lim = side === 'left' ? LIMITS.left : LIMITS.right;
  const raw = side === 'left' ? startWidth + deltaX : startWidth - deltaX;
  const maxByCenter = total - otherWidth - LIMITS.centerMin - 2 * LIMITS.resizer;
  return clamp(Math.round(raw), lim.min, Math.max(lim.min, Math.min(lim.max, maxByCenter)));
}

/** CSS-pixel rect -> physical-pixel rect, rounding EDGES (not sizes) to avoid 1 px drift. */
export function toPhysical(r: { left: number; top: number; right: number; bottom: number }, dpr: number) {
  const x = Math.round(r.left * dpr);
  const y = Math.round(r.top * dpr);
  return { x, y, w: Math.round(r.right * dpr) - x, h: Math.round(r.bottom * dpr) - y };
}

const KEY = 'syncwatch.layout.v1';

export function loadPrefs(): LayoutPrefs {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return { ...DEFAULT_PREFS };
    const v = JSON.parse(raw) as Partial<LayoutPrefs>;
    return {
      leftWidth: typeof v.leftWidth === 'number' ? clamp(v.leftWidth, LIMITS.left.min, LIMITS.left.max) : DEFAULT_PREFS.leftWidth,
      rightWidth: typeof v.rightWidth === 'number' ? clamp(v.rightWidth, LIMITS.right.min, LIMITS.right.max) : DEFAULT_PREFS.rightWidth,
      leftCollapsed: v.leftCollapsed === true,
      rightCollapsed: v.rightCollapsed === true,
      membersCollapsed: v.membersCollapsed === true,
      hostCollapsed: v.hostCollapsed === true,
    };
  } catch {
    return { ...DEFAULT_PREFS };
  }
}

export function savePrefs(p: LayoutPrefs): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(p));
  } catch {
    /* storage unavailable: layout simply is not remembered */
  }
}
