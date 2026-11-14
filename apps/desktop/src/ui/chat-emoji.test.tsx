// Chat messages typed by people are user content: rendered exactly as typed, emoji included
// (the no-emoji rule covers only the app's own text).
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { ChatPanel } from '../components/ChatPanel';

afterEach(cleanup);

it('renders a typed message with emoji unchanged', () => {
  const cp = (...c: number[]) => String.fromCodePoint(...c);
  // Party popper, thumbs up + skin tone, heart + VS16, a family ZWJ sequence.
  const text = `Look at this ${cp(0x1f389, 0x1f44d, 0x1f3fd)} ${cp(0x2764, 0xfe0f)} and ${cp(0x1f468, 0x200d, 0x1f469, 0x200d, 0x1f467)}`;
  render(
    <ChatPanel
      messages={[{ id: 'c1', senderId: 'm1', senderName: 'Lina', isHost: false, colorIndex: 1, text, wallTime: '21:09', mediaTimeMs: null }]}
      onCollapse={() => {}}
      onChipClick={() => {}}
      onSend={() => {}}
      disabledReason={null}
    />,
  );
  expect(screen.getByText((_, el) => el?.tagName === 'P' && el.textContent?.includes(text) === true)).toBeTruthy();
});
