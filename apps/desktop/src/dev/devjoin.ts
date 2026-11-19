// Dev scenarios: join (or rejoin) the dev room. With the dev signaling server that is the room
// code; with manual invites (two-windows.ps1 -Handshake manual, as in release builds) the dev
// host hands out an invite through files and admits the reply (ADR 0014).
import { invoke } from '@tauri-apps/api/core';
import { room, type EntryOutcome } from '../lib/room';

export async function devJoin(name: string, code: string, password: string | null): Promise<EntryOutcome> {
  if ((await room.handshake()) === 'manual') return invoke<EntryOutcome>('dev_join_invite', { name });
  return room.join(name, code, password);
}
