import { useEffect, useRef, useState } from 'react';
import type { MockChatMessage } from '../mock/data';
import { memberColors } from '../theme/tokens';

const FADE_AFTER_MS = 8000;

/**
 * Fullscreen chat overlay (spec section 7): lower-left, short-lived lines that fade after
 * about 8 s, optional input on Enter. Kept in the lower-left and above the bottom controls so
 * it never covers centered subtitles.
 */
export function ChatOverlay({ messages, inputOpen, onCloseInput, onSend }: {
  messages: MockChatMessage[];
  inputOpen: boolean;
  onCloseInput: () => void;
  onSend: (text: string) => void;
}) {
  const [now, setNow] = useState(() => Date.now());
  const shownAt = useRef(new Map<string, number>());
  const [draft, setDraft] = useState('');

  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), 500);
    return () => window.clearInterval(t);
  }, []);

  const recent = messages.filter((m) => !m.divider).slice(-5);
  for (const m of recent) if (!shownAt.current.has(m.id)) shownAt.current.set(m.id, Date.now());

  return (
    <div className="pointer-events-none absolute bottom-24 left-4 w-[min(28rem,40%)] space-y-1" aria-live="polite">
      {recent.map((m) => {
        const age = now - (shownAt.current.get(m.id) ?? now);
        const visible = inputOpen || age < FADE_AFTER_MS;
        return (
          <div key={m.id} className="chat-overlay-line rounded-md bg-black/60 px-2 py-1 text-sm" style={{ opacity: visible ? 1 : 0 }}>
            <span className="font-semibold" style={{ color: memberColors[m.colorIndex % memberColors.length] }}>
              {m.senderName}:
            </span>{' '}
            {m.text}
          </div>
        );
      })}
      {inputOpen && (
        <input
          autoFocus
          value={draft}
          maxLength={2000}
          aria-label="Chat message"
          placeholder="Message (Enter to send, Esc to close)"
          className="pointer-events-auto w-full rounded-md border border-border bg-black/80 px-2 py-1.5 text-sm text-text focus:border-accent focus:outline-none"
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              const t = draft.trim();
              if (t) onSend(t);
              setDraft('');
              onCloseInput();
            } else if (e.key === 'Escape') {
              e.stopPropagation();
              onCloseInput();
            }
          }}
        />
      )}
    </div>
  );
}
