import { describe, expect, it } from 'vitest';
import { redact } from './redact';

describe('diagnostics redaction', () => {
  it('removes credentials, keys, tokens, the app name and IPs', () => {
    const s = [
      '{"username": "u123", "credential": "s3cret", "apiKey": "k"}',
      'https://myapp.metered.live/api/v1/turn/credentials?apiKey=abcdef&x=1',
      'turns:user:pass@relay.example:443',
      'peer 203.0.113.7:51413 and 2001:db8::1:5 and fe80:0:0:0:0:0:0:1',
      'stream http://127.0.0.1:5555/0123456789abcdef0123456789abcdef/media',
      'bind 0.0.0.0, version 1.2.3, at 10:51:45',
    ].join('\n');
    const r = redact(s);
    for (const secret of ['u123', 's3cret', 'myapp', 'abcdef&', 'user:pass', '203.0.113.7', '2001:db8', 'fe80:0:0', '0123456789abcdef0123456789abcdef']) {
      expect(r, secret).not.toContain(secret);
    }
    expect(r).toContain('<app-name>.metered.live');
    expect(r).toContain('127.0.0.1');
    expect(r).toContain('version 1.2.3');
    expect(r).toContain('10:51:45');
  });
});
