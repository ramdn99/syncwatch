import { describe, expect, it } from 'vitest';
import { computeColumns, DEFAULT_PREFS, dragWidth, LIMITS, toPhysical } from './layout';

describe('computeColumns', () => {
  it('keeps side widths and gives the rest to the video', () => {
    const c = computeColumns(1440, DEFAULT_PREFS, false);
    expect(c.left).toBe(320);
    expect(c.right).toBe(320);
    expect(c.center).toBe(1440 - 640 - 2 * LIMITS.resizer);
  });

  it('shrinks the video first, then the side columns, never below their minimum', () => {
    const wide = { ...DEFAULT_PREFS, leftWidth: 500, rightWidth: 500 };
    const c = computeColumns(1500, wide, false);
    expect(c.center).toBe(1500 - 1000 - 8);
    const narrow = computeColumns(960, wide, false);
    expect(narrow.center).toBe(LIMITS.centerMin);
    expect(narrow.left).toBeGreaterThanOrEqual(LIMITS.left.min);
    expect(narrow.right).toBeGreaterThanOrEqual(LIMITS.right.min);
    expect(narrow.left + narrow.right + narrow.center + 8).toBe(960);
  });

  it('collapsed columns take no space and fullscreen hides both', () => {
    const c = computeColumns(1200, { ...DEFAULT_PREFS, leftCollapsed: true }, false);
    expect(c.left).toBe(0);
    expect(c.center).toBe(1200 - 320 - LIMITS.resizer);
    expect(computeColumns(1920, DEFAULT_PREFS, true)).toEqual({ left: 0, right: 0, center: 1920 });
  });
});

describe('dragWidth', () => {
  it('clamps to the column limits and the center minimum', () => {
    expect(dragWidth('left', 320, 1000, 1440, 320)).toBe(Math.min(LIMITS.left.max, 1440 - 320 - LIMITS.centerMin - 8));
    expect(dragWidth('left', 320, -1000, 1440, 320)).toBe(LIMITS.left.min);
    // Right resizer: dragging left widens the right column.
    expect(dragWidth('right', 320, -50, 1440, 320)).toBe(370);
  });
});

describe('toPhysical', () => {
  it('rounds edges so adjacent rects never gap or overlap', () => {
    const a = toPhysical({ left: 0, top: 40, right: 320.4, bottom: 800 }, 1.25);
    const b = toPhysical({ left: 320.4, top: 40, right: 1000, bottom: 800 }, 1.25);
    expect(a.x + a.w).toBe(b.x);
    expect(b).toEqual({ x: 401, y: 50, w: 849, h: 950 });
  });
});
