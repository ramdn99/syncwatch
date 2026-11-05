import { describe, expect, it } from 'vitest';
import { endText, formatCode } from './room';

describe('room helpers', () => {
  it('formats room codes as XXXX-XXXX', () => {
    expect(formatCode('K7QM4XRP')).toBe('K7QM-4XRP');
    expect(formatCode(null)).toBe('');
  });

  it('explains every way a room can end, and stays quiet for a normal leave', () => {
    expect(endText({ kind: 'left' })).toBeNull();
    expect(endText({ kind: 'kicked', banned: true })).toMatch(/banned/);
    expect(endText({ kind: 'refused', reason: 'locked' })).toMatch(/locked/);
    expect(endText({ kind: 'refused', reason: 'full' })).toMatch(/full/);
    expect(endText({ kind: 'authRejected', reason: 'TooManyAttempts' })).toMatch(/Too many/);
    expect(endText({ kind: 'hostNotVerified' })).toMatch(/could not be verified/);
    expect(endText({ kind: 'noSuchRoom' })).toMatch(/no room/);
    expect(endText({ kind: 'error', message: 'x' })).toBe('x');
  });
});
