// "Copy diagnostics": credentials, keys, tokens and IP addresses are removed before anything
// reaches the clipboard (spec section 11). Loopback addresses stay (they identify nothing).

const RULES: [RegExp, string][] = [
  // JSON fields that carry secrets.
  [/"(credential|username|password|apiKey|api_key|token|secret)"\s*:\s*"[^"]*"/gi, '"$1": "<redacted>"'],
  // Query parameters that carry secrets.
  [/([?&](?:apiKey|api_key|key|token|password|credential|username)=)[^&\s"']+/gi, '$1<redacted>'],
  // user:password@ in URLs (turn:, turns:, http...).
  [/\b((?:turns?|stuns?|https?|wss?):\/*)[^@\s"'/]+@/gi, '$1<redacted>@'],
  // The Metered app name is part of the host name: a secret too.
  [/\b[\w-]+\.metered\.live\b/gi, '<app-name>.metered.live'],
  // 128-bit tokens (the loopback stream URL) and longer hex secrets.
  [/\b[0-9a-f]{32,}\b/gi, '<token>'],
  // IPv4 except loopback and 0.0.0.0 (IPv6 below).
  [/\b(?!127\.0\.0\.1\b)(?!0\.0\.0\.0\b)(?:25[0-5]|2[0-4]\d|1?\d?\d)(?:\.(?:25[0-5]|2[0-4]\d|1?\d?\d)){3}\b/g, '<ip>'],
];

/** IPv6: a "::" or at least five groups (clock times like 10:51:45 have three). */
const IPV6 = /(?<![\w:])(?:[0-9a-f]{0,4}:){2,7}[0-9a-f]{0,4}(?![\w:])/gi;

export function redact(text: string): string {
  const t = RULES.reduce((s, [re, to]) => s.replace(re, to), text);
  return t.replace(IPV6, (m) => (m.includes('::') || m.split(':').length >= 5 ? '<ip>' : m));
}
