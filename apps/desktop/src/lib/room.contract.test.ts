// @vitest-environment node
// Contract between the UI and the Rust room commands (see the Rust test room_actions_contract).
// This test writes every room_action payload the UI can send to room-actions.fixture.json and
// fails if the committed file is out of date. The Rust test deserializes the same file with
// the real command argument type, so any field-name mismatch fails one of the two suites.

import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { ROOM_ACTION_EXAMPLES, roomActionArgs } from './room';

// Tests run with apps/desktop as the working directory.
const FIXTURE = resolve(process.cwd(), 'src/lib/room-actions.fixture.json');

describe('room_action contract', () => {
  it('the fixture holds exactly the payloads the UI sends', () => {
    const payloads = Object.values(ROOM_ACTION_EXAMPLES).map((a) => roomActionArgs(a));
    const text = `${JSON.stringify(payloads, null, 2)}\n`;
    if (process.env.UPDATE_FIXTURES === '1') writeFileSync(FIXTURE, text);
    let current = '';
    try {
      current = readFileSync(FIXTURE, 'utf8');
    } catch {
      /* missing: fails below */
    }
    expect(current.replace(/\r\n/g, '\n'), 'run: UPDATE_FIXTURES=1 pnpm test').toBe(text);
  });

  it('every field is camelCase (the Rust side refuses snake_case)', () => {
    for (const a of Object.values(ROOM_ACTION_EXAMPLES)) {
      for (const k of Object.keys(a)) expect(k).not.toMatch(/_/);
    }
  });
});
