// A chat message's text (spec section 9): plain text only. URLs are shown as text, never linked
// or opened; each gets a small copy button. React escapes everything, so markup stays literal.
import { useState } from 'react';
import { Icon } from '../ui/icons';

const URL_RE = /\b(?:https?:\/\/|www\.)[^\s<>"]+/gi;
/** Trailing punctuation that usually ends the sentence, not the URL. */
const TRAILING = /[.,;:!?)\]}'"]+$/;

export type ChatPart = { url: false; text: string } | { url: true; text: string };

/** Splits text into plain parts and URL parts (exported for tests). */
export function splitUrls(text: string): ChatPart[] {
  const out: ChatPart[] = [];
  let last = 0;
  for (const m of text.matchAll(URL_RE)) {
    let url = m[0];
    const trail = url.match(TRAILING)?.[0] ?? '';
    url = url.slice(0, url.length - trail.length);
    if (!url) continue;
    const start = m.index ?? 0;
    if (start > last) out.push({ url: false, text: text.slice(last, start) });
    out.push({ url: true, text: url });
    last = start + url.length;
  }
  if (last < text.length) out.push({ url: false, text: text.slice(last) });
  return out;
}

function CopyUrl({ url }: { url: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className="mx-0.5 inline-flex translate-y-0.5 items-center rounded px-0.5 text-muted hover:bg-elevated hover:text-text"
      aria-label={copied ? 'Link copied' : 'Copy link'}
      title={copied ? 'Copied' : 'Copy link (links are never opened from chat)'}
      onClick={() => {
        void navigator.clipboard?.writeText(url).then(() => {
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        });
      }}
    >
      <Icon name={copied ? 'complete' : 'copy'} size="xs" />
    </button>
  );
}

export function ChatText({ text }: { text: string }) {
  return (
    <>
      {splitUrls(text).map((p, i) =>
        p.url ? (
          <span key={i} className="break-all" data-testid="chat-url">
            {p.text}
            <CopyUrl url={p.text} />
          </span>
        ) : (
          <span key={i}>{p.text}</span>
        ),
      )}
    </>
  );
}
