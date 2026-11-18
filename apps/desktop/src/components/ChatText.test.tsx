// Spec section 9: URLs in chat are shown as text with a copy action, never opened or linked.
import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { ChatText, splitUrls } from './ChatText';

describe('chat URLs', () => {
  it('splits URLs out of the text, leaving sentence punctuation outside', () => {
    expect(splitUrls('see https://example.org/a?b=1, then www.test.net.')).toEqual([
      { url: false, text: 'see ' },
      { url: true, text: 'https://example.org/a?b=1' },
      { url: false, text: ', then ' },
      { url: true, text: 'www.test.net' },
      { url: false, text: '.' },
    ]);
    expect(splitUrls('no links here')).toEqual([{ url: false, text: 'no links here' }]);
  });

  it('never renders a link, only text with a copy button', () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    const { container } = render(
      <p>
        <ChatText text={'look <b>bold</b> javascript:alert(1) https://example.org/x'} />
      </p>,
    );
    expect(container.querySelector('a')).toBeNull();
    expect(container.querySelector('b')).toBeNull();
    expect(container.textContent).toContain('<b>bold</b>');
    expect(screen.getByTestId('chat-url').textContent).toBe('https://example.org/x');
    fireEvent.click(screen.getByRole('button', { name: 'Copy link' }));
    expect(writeText).toHaveBeenCalledWith('https://example.org/x');
  });
});
