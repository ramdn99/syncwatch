// Spec section 7: the fullscreen chat overlay is lower-left and never covers subtitles.
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { ChatOverlay, overlayBottomPct } from './ChatOverlay';

afterEach(cleanup);

/** Height share of N text subtitle lines at mpv's default size (55 px of 720, 22 px margin). */
const subtitleBand = (lines: number, scalePct: number) => ((22 + lines * 55) / 720) * (scalePct / 100) * 100;

describe('fullscreen chat overlay', () => {
  it('sits above three subtitle lines at every subtitle size', () => {
    for (const s of [50, 100, 150, 200]) {
      expect(overlayBottomPct(s), `size ${s}%`).toBeGreaterThan(subtitleBand(3, s));
      expect(overlayBottomPct(s)).toBeLessThanOrEqual(60);
    }
    expect(overlayBottomPct()).toBe(29);
  });

  it('uses that position on screen', () => {
    render(<ChatOverlay messages={[]} inputOpen={false} onCloseInput={() => {}} onSend={() => {}} subScalePct={150} />);
    expect(screen.getByTestId('chat-overlay').style.bottom).toBe(`${overlayBottomPct(150)}%`);
  });
});
