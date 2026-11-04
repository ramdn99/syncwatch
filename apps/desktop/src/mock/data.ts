// Mock room data for Milestone 1 (no networking yet).

export type ConnKind = 'direct' | 'relayed' | 'reconnecting';
export type MediaStatus = 'ready' | 'verifying' | 'missing' | 'mismatch' | 'buffering';

export interface MockMember {
  id: string;
  name: string;
  isHost: boolean;
  isYou: boolean;
  conn: ConnKind;
  latencyMs: number | null;
  media: MediaStatus;
  tracksInSync: boolean;
  muted: boolean;
  colorIndex: number;
}

export interface MockChatMessage {
  id: string;
  senderId: string;
  senderName: string;
  isHost: boolean;
  colorIndex: number;
  text: string;
  wallTime: string;
  mediaTimeMs: number | null;
  /** Divider row instead of a message. */
  divider?: string;
}

export const mockMembers: MockMember[] = [
  { id: 'm1', name: 'Alex', isHost: true, isYou: true, conn: 'direct', latencyMs: null, media: 'ready', tracksInSync: true, muted: false, colorIndex: 0 },
  { id: 'm2', name: 'Lina', isHost: false, isYou: false, conn: 'direct', latencyMs: 24, media: 'ready', tracksInSync: true, muted: false, colorIndex: 1 },
  { id: 'm3', name: 'Omar', isHost: false, isYou: false, conn: 'relayed', latencyMs: 88, media: 'verifying', tracksInSync: true, muted: true, colorIndex: 2 },
  { id: 'm4', name: 'Sara', isHost: false, isYou: false, conn: 'reconnecting', latencyMs: null, media: 'missing', tracksInSync: false, muted: false, colorIndex: 3 },
];

export const mockChat: MockChatMessage[] = [
  { id: 'd1', senderId: '', senderName: '', isHost: false, colorIndex: 0, text: '', wallTime: '20:58', mediaTimeMs: null, divider: 'Now playing: test.mkv' },
  { id: 'c1', senderId: 'm2', senderName: 'Lina', isHost: false, colorIndex: 1, text: 'Ready when you are!', wallTime: '21:00', mediaTimeMs: 0 },
  { id: 'c2', senderId: 'm1', senderName: 'Alex', isHost: true, colorIndex: 0, text: 'Starting now. Japanese audio, English subs.', wallTime: '21:00', mediaTimeMs: 4_000 },
  { id: 'c3', senderId: 'm3', senderName: 'Omar', isHost: false, colorIndex: 2, text: 'that opening shot though', wallTime: '21:02', mediaTimeMs: 95_000 },
  { id: 'c4', senderId: 'm2', senderName: 'Lina', isHost: false, colorIndex: 1, text: 'Look at the background here https://example.invalid/not-a-link', wallTime: '21:09', mediaTimeMs: 512_000 },
  { id: 'c5', senderId: 'm1', senderName: 'Alex', isHost: true, colorIndex: 0, text: '<b>not bold</b> — plain text only', wallTime: '21:15', mediaTimeMs: 870_000 },
];
