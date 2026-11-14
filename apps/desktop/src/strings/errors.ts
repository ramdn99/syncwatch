// Error states (Milestone 8): one friendly message per state, each with the one-click action
// that helps. Used by the room banner, the Create/Join dialog and the chat input.

export type ErrorId =
  | 'noServer'
  | 'firewall'
  | 'unsupportedFile'
  | 'trackUnresolved'
  | 'noPeers'
  | 'diskFull'
  | 'chatMuted'
  | 'chatOff'
  | 'chatRateLimited';

export type ErrorAction = 'retry' | 'connectionSettings' | 'networkSettings' | 'filesSettings' | 'alwaysRelay' | 'chooseFile' | 'changeSource' | 'showTracks';

export interface ErrorState {
  title: string;
  text: string;
  /** The one-click action, with its button label; null when there is nothing to do but wait. */
  action: { id: ErrorAction; label: string } | null;
}

export const ERRORS: Record<ErrorId, ErrorState> = {
  noServer: {
    title: 'The room server cannot be reached',
    text: 'Check your internet connection. If it works, the server may be down for a moment, or a firewall or proxy blocks it.',
    action: { id: 'retry', label: 'Try again' },
  },
  firewall: {
    title: 'A member cannot connect',
    text: 'A firewall may block the direct connection. Allow SyncWatch in Windows Security (Firewall), or use the relay for everyone you connect to.',
    action: { id: 'alwaysRelay', label: 'Always use relay' },
  },
  unsupportedFile: {
    title: 'This video cannot be played',
    text: 'The file may be damaged or in a format the player does not support. Choose a different copy of the video.',
    action: { id: 'chooseFile', label: 'Choose a different file...' },
  },
  trackUnresolved: {
    title: "Your file's tracks differ from the host's",
    text: 'Pick the audio and subtitle tracks yourself; playback stays in sync.',
    action: { id: 'showTracks', label: 'Show tracks' },
  },
  noPeers: {
    title: 'Nobody is sharing this torrent yet',
    text: 'SyncWatch keeps trying. What is already downloaded keeps playing. Turning DHT on, or allowing incoming connections, can help.',
    action: { id: 'networkSettings', label: 'Network settings...' },
  },
  diskFull: {
    title: 'The disk is full',
    text: 'The torrent cannot be saved. Free some space, choose a cache folder on another drive, or lower the cache size limit.',
    action: { id: 'filesSettings', label: 'Files settings...' },
  },
  chatMuted: {
    title: 'You are muted in the chat',
    text: 'The host muted you. You can still read the chat.',
    action: null,
  },
  chatOff: {
    title: 'The chat is off',
    text: 'The host turned the chat off for this room; only the host can write.',
    action: null,
  },
  chatRateLimited: {
    title: 'Slow down',
    text: 'At most 5 messages in 5 seconds. Your message was not sent; send it again in a moment.',
    action: null,
  },
};

/** The Create/Join error a server outage or a blocked connection produces. */
export function isNoServer(error: string): boolean {
  return /could not reach the signaling server|connection to the signaling server .* was lost|failed to connect|network is unreachable/i.test(error);
}

/** Seconds a member's link may stay "connecting" before the firewall hint shows. */
export const FIREWALL_HINT_AFTER_MS = 25_000;

/** Shown once on the first run, next to the connection setup. */
export const FIREWALL_FIRST_RUN =
  'When you first create or join a room, Windows may ask whether SyncWatch may use the network. Choose Allow (private networks are enough at home). If you chose Cancel, members may fail to connect directly; the relay still works, and Settings > Network > Always use relay makes it the default.';
