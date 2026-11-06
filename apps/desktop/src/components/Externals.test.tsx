import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { Externals } from './HostControls';

afterEach(cleanup);

const ext = (over: Partial<{ received: number | null; failed: boolean; withheld: boolean }>) => ({
  id: 1,
  kind: 'sub' as const,
  name: 'fansub.ass',
  size: 50_000,
  received: 0,
  failed: false,
  withheld: false,
  ...over,
});

it('a withheld file says so instead of showing a stalled download', () => {
  render(<Externals room={{ externals: [ext({ withheld: true, received: null })], declined: [] }} isHost={false} onAction={() => {}} />);
  expect(screen.getByTestId('external-withheld').textContent).toBe('The host did not send this file to you');
  expect(screen.queryByText('0%')).toBeNull();
  expect(screen.queryByRole('button', { name: 'Cancel' })).toBeNull();
});

it('a normal download shows its progress and no withheld text', () => {
  render(<Externals room={{ externals: [ext({ received: 25_000 })], declined: [] }} isHost={false} onAction={() => {}} />);
  expect(screen.getByText('50%')).toBeTruthy();
  expect(screen.queryByTestId('external-withheld')).toBeNull();
});
