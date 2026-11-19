// Found in the Milestone 9 window check: in fullscreen the visible control bar covered the
// bottom subtitle line. Subtitles are lifted above the bar while it is shown.
import { describe, expect, it } from 'vitest';
import { SUB_MARGIN_DEFAULT, subMarginFor } from './VideoArea';

describe('subtitle margin under the control bar', () => {
  it('lifts subtitles by the bar height while the bar is shown', () => {
    // 1080 px fullscreen with a 64 px bar: 22 + 64 * 720 / 1080 = 64.67 -> 65.
    expect(subMarginFor(64, 1080, true)).toBe(65);
    // A smaller window: the same bar is a larger share of the height.
    expect(subMarginFor(64, 540, true)).toBe(107);
  });

  it('goes back to mpv\'s default when the bar hides, and never runs away', () => {
    expect(subMarginFor(64, 1080, false)).toBe(SUB_MARGIN_DEFAULT);
    expect(subMarginFor(64, 0, true)).toBe(SUB_MARGIN_DEFAULT);
    expect(subMarginFor(5000, 100, true)).toBe(300);
  });
});
