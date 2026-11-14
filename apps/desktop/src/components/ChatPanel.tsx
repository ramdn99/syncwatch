import { forwardRef, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { chipText, nearCurrent, spoilerUntil, type ChatLine } from '../lib/chat';
import type { MockChatMessage } from '../mock/data';
import { memberColors } from '../theme/tokens';

export interface ChatPanelProps {
  /** Room chat lines; plain mock messages (outside a room) get chips from mediaTimeMs. */
  messages: (ChatLine | MockChatMessage)[];
  /** Current playhead in ms (filters and spoiler guard); null without media. */
  positionMs?: number | null;
  onCollapse: () => void;
  /** Seek target in seconds (the lead-in is already applied). */
  onChipClick: (seconds: number) => void;
  onSend: (text: string) => void;
  /** Host only: delete a message. */
  onDelete?: (id: string) => void;
  /** Shown instead of the input when this person cannot chat. */
  disabledReason?: string | null;
}

const asLine = (m: ChatLine | MockChatMessage): ChatLine =>
  'chip' in m
    ? m
    : {
        ...m,
        chip: m.mediaTimeMs != null ? chipText(m.mediaTimeMs, null) : null,
        seekMs: m.mediaTimeMs != null ? Math.max(0, m.mediaTimeMs - 2000) : null,
        stale: false,
        currentMs: m.mediaTimeMs,
      };

/** LEFT column: chat, full height. Plain text only; URLs are never linked. */
export const ChatPanel = forwardRef<HTMLTextAreaElement, ChatPanelProps>(function ChatPanel(
  { messages, positionMs = null, onCollapse, onChipClick, onSend, onDelete, disabledReason },
  inputRef,
) {
  const [draft, setDraft] = useState('');
  const [nearOnly, setNearOnly] = useState(false);
  const [spoilerGuard, setSpoilerGuard] = useState(false);
  const [atBottom, setAtBottom] = useState(true);
  const [unread, setUnread] = useState(0);
  const listRef = useRef<HTMLOListElement>(null);
  const lastCount = useRef(messages.length);

  const all = messages.map(asLine);
  const lines = nearOnly ? nearCurrent(all, positionMs) : all;

  // Stick to the bottom unless the reader scrolled up; then count unread messages.
  useLayoutEffect(() => {
    const added = messages.length - lastCount.current;
    lastCount.current = messages.length;
    if (atBottom && listRef.current) listRef.current.scrollTop = listRef.current.scrollHeight;
    else if (added > 0) setUnread((u) => u + added);
  }, [messages.length, atBottom]);
  useEffect(() => {
    if (atBottom) setUnread(0);
  }, [atBottom]);

  const send = () => {
    const t = draft.trim();
    if (!t || disabledReason) return;
    onSend(t);
    setDraft('');
  };
  return (
    <aside className="flex h-full min-h-0 flex-col bg-surface" aria-label="Chat">
      <header className="flex items-center justify-between border-b border-border px-3 py-2">
        <h2 className="font-brand text-sm font-bold tracking-wide">Chat</h2>
        <div className="flex items-center gap-1 text-[11px] text-muted">
          <button
            type="button"
            aria-pressed={nearOnly}
            title="Only messages within 2 minutes of the current time"
            className={`rounded px-1.5 py-0.5 ${nearOnly ? 'bg-accent text-on-accent' : 'hover:text-text'}`}
            onClick={() => setNearOnly((v) => !v)}
          >
            Near now
          </button>
          <button
            type="button"
            aria-pressed={spoilerGuard}
            title="Hide messages about scenes you have not reached yet"
            className={`rounded px-1.5 py-0.5 ${spoilerGuard ? 'bg-accent text-on-accent' : 'hover:text-text'}`}
            onClick={() => setSpoilerGuard((v) => !v)}
          >
            Spoiler guard
          </button>
          <button type="button" className="rounded px-2 text-muted hover:text-text" aria-label="Collapse chat ([)" title="Collapse chat ([)" onClick={onCollapse}>
            «
          </button>
        </div>
      </header>
      <div className="relative min-h-0 flex-1">
        <ol
          ref={listRef}
          className="h-full space-y-2 overflow-y-auto px-3 py-3 select-text"
          onScroll={(e) => {
            const el = e.currentTarget;
            setAtBottom(el.scrollHeight - el.scrollTop - el.clientHeight < 24);
          }}
        >
          {lines.map((m) => {
            if (m.divider) {
              return (
                <li key={m.id} className="my-2 flex items-center gap-2 text-xs text-muted">
                  <span className="h-px flex-1 bg-border" />
                  {m.divider}
                  <span className="h-px flex-1 bg-border" />
                </li>
              );
            }
            const hiddenUntil = spoilerGuard ? spoilerUntil(m, positionMs) : null;
            return (
              <li key={m.id} className="group text-sm leading-snug" data-testid="chat-line">
                <div className="flex items-baseline gap-2">
                  <span className="font-semibold" style={{ color: memberColors[m.colorIndex % memberColors.length] }}>
                    {m.senderName}
                  </span>
                  {m.isHost && <span className="rounded bg-accent px-1 text-[10px] font-bold text-on-accent">HOST</span>}
                  {m.chip == null ? (
                    <span className="text-[11px] text-muted" data-testid="chip-none">
                      no timestamp
                    </span>
                  ) : m.seekMs != null ? (
                    <button
                      type="button"
                      className="tabular rounded bg-elevated px-1.5 text-xs text-info hover:bg-border"
                      title="Jump here (2 s lead-in)"
                      data-testid="chip"
                      onClick={() => onChipClick(m.seekMs! / 1000)}
                    >
                      {m.chip}
                    </button>
                  ) : (
                    <span
                      className="tabular rounded bg-elevated px-1.5 text-xs text-muted"
                      title={m.stale ? 'From an earlier video' : 'You cannot seek in this room'}
                      data-testid="chip"
                    >
                      {m.chip}
                    </span>
                  )}
                  <span className="tabular ml-auto text-xs text-muted">{m.wallTime}</span>
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
                {hiddenUntil != null ? (
                  <p className="text-xs text-muted italic">Hidden until {chipText(hiddenUntil, null)}</p>
                ) : (
                  <p className="break-words whitespace-pre-wrap text-text">{m.text}</p>
                )}
              </li>
            );
          })}
        </ol>
        {!atBottom && (
          <button
            type="button"
            className="absolute bottom-2 left-1/2 -translate-x-1/2 rounded-full bg-accent px-3 py-1 text-xs font-semibold text-on-accent shadow"
            onClick={() => {
              if (listRef.current) listRef.current.scrollTop = listRef.current.scrollHeight;
            }}
          >
            Jump to latest{unread > 0 && ` (${unread})`}
          </button>
        )}
      </div>
      <div className="border-t border-border p-2">
        {/* Normal state, not an error: muted or chat off disables the input with the reason. */}
        {disabledReason && (
          <p className="mb-1 text-xs text-muted" role="status" data-testid="chat-input-status">
            {disabledReason}
          </p>
        )}
        <div className="flex gap-2">
          <textarea
            ref={inputRef}
            rows={2}
            value={disabledReason ? '' : draft}
            maxLength={2000}
            disabled={disabledReason != null}
            placeholder={disabledReason ?? 'Message (Enter to send)'}
            aria-label="Chat message"
            className="w-full resize-none rounded-md border border-border bg-elevated px-2 py-1.5 text-sm text-text placeholder:text-muted focus:border-accent focus:outline-none disabled:cursor-not-allowed disabled:opacity-60"
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !e.shiftKey) {
                e.preventDefault();
                send();
              }
              if (e.key === 'Escape') (e.target as HTMLTextAreaElement).blur();
            }}
          />
          <button
            type="button"
            disabled={disabledReason != null || draft.trim() === ''}
            aria-label="Send message"
            className="self-end rounded-md bg-accent px-3 py-1.5 text-sm font-semibold text-on-accent disabled:cursor-not-allowed disabled:opacity-40"
            onClick={send}
          >
            Send
          </button>
        </div>
      </div>    </aside>
  );
});
