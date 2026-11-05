import { forwardRef, useState } from 'react';
import { formatTime } from '../lib/time';
import type { MockChatMessage } from '../mock/data';
import { memberColors } from '../theme/tokens';

export interface ChatPanelProps {
  messages: MockChatMessage[];
  onCollapse: () => void;
  onChipClick: (seconds: number) => void;
  onSend: (text: string) => void;
  /** Host only: delete a message. */
  onDelete?: (id: string) => void;
  /** Shown instead of the input when this person cannot chat. */
  disabledReason?: string | null;
}

/** LEFT column: chat, full height. Plain text only; URLs are never linked. */
export const ChatPanel = forwardRef<HTMLTextAreaElement, ChatPanelProps>(function ChatPanel(
  { messages, onCollapse, onChipClick, onSend, onDelete, disabledReason },
  inputRef,
) {
  const [draft, setDraft] = useState('');
  const send = () => {
    const t = draft.trim();
    if (!t) return;
    onSend(t);
    setDraft('');
  };
  return (
    <aside className="flex h-full min-h-0 flex-col bg-surface" aria-label="Chat">
      <header className="flex items-center justify-between border-b border-border px-3 py-2">
        <h2 className="font-brand text-sm font-bold tracking-wide">Chat</h2>
        <button type="button" className="rounded px-2 text-muted hover:text-text" aria-label="Collapse chat ([)" title="Collapse chat ([)" onClick={onCollapse}>
          «
        </button>
      </header>
      <ol className="min-h-0 flex-1 space-y-2 overflow-y-auto px-3 py-3 select-text">
        {messages.map((m) =>
          m.divider ? (
            <li key={m.id} className="my-2 flex items-center gap-2 text-xs text-muted">
              <span className="h-px flex-1 bg-border" />
              {m.divider}
              <span className="h-px flex-1 bg-border" />
            </li>
          ) : (
            <li key={m.id} className="group text-sm leading-snug">
              <div className="flex items-baseline gap-2">
                <span className="font-semibold" style={{ color: memberColors[m.colorIndex % memberColors.length] }}>
                  {m.senderName}
                </span>
                {m.isHost && <span className="rounded bg-accent px-1 text-[10px] font-bold text-on-accent">HOST</span>}
                {m.mediaTimeMs != null && (
                  <button
                    type="button"
                    className="tabular rounded bg-elevated px-1.5 text-xs text-info hover:bg-border"
                    title="Jump here (2 s lead-in)"
                    onClick={() => onChipClick(Math.max(0, m.mediaTimeMs! / 1000 - 2))}
                  >
                    {formatTime(m.mediaTimeMs / 1000)}
                  </button>
                )}
                <span className="tabular ml-auto text-xs text-disabled">{m.wallTime}</span>
                {onDelete && (
                  <button
                    type="button"
                    className="invisible rounded px-1 text-xs text-danger group-hover:visible focus:visible"
                    aria-label={`Delete message from ${m.senderName}`}
                    onClick={() => onDelete(m.id)}
                  >
                    Delete
                  </button>
                )}
              </div>
              {/* React escapes text: markup in messages is shown literally, never rendered. */}
              <p className="break-words whitespace-pre-wrap text-text">{m.text}</p>
            </li>
          ),
        )}
      </ol>
      <div className="border-t border-border p-2">
        {disabledReason ? (
          <p className="rounded-md bg-elevated px-2 py-2 text-xs text-muted">{disabledReason}</p>
        ) : (
        <textarea
          ref={inputRef}
          rows={2}
          value={draft}
          maxLength={2000}
          placeholder="Message (Enter to send)"
          aria-label="Chat message"
          className="w-full resize-none rounded-md border border-border bg-elevated px-2 py-1.5 text-sm text-text placeholder:text-disabled focus:border-accent focus:outline-none"
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey) {
              e.preventDefault();
              send();
            }
            if (e.key === 'Escape') (e.target as HTMLTextAreaElement).blur();
          }}
        />
        )}
      </div>
    </aside>
  );
});
