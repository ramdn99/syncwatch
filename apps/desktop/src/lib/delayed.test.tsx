import { act, renderHook } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { useDelayedTrue } from './delayed';

afterEach(() => vi.useRealTimers());

describe('useDelayedTrue (gate banner delay)', () => {
  it('a short gate pass never shows; a longer one shows after the delay and hides at once', () => {
    vi.useFakeTimers();
    const { result, rerender } = renderHook(({ f }) => useDelayedTrue(f, 600), { initialProps: { f: false } });
    rerender({ f: true });
    act(() => void vi.advanceTimersByTime(400));
    expect(result.current).toBe(false);
    rerender({ f: false });
    act(() => void vi.advanceTimersByTime(1000));
    expect(result.current).toBe(false);
    rerender({ f: true });
    act(() => void vi.advanceTimersByTime(600));
    expect(result.current).toBe(true);
    rerender({ f: false });
    expect(result.current).toBe(false);
  });
});
