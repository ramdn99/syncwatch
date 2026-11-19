// User-facing text for manual invites (ADR 0014). One place, so the user guide and the app
// say the same thing.
export const INVITE_TEXT = {
  joinIntro:
    'Paste the invite the host sent you. SyncWatch then makes a reply for you to send back. No connection to the host is made until the host lets you in.',
  createIntro:
    'After the room is created, use Invite a member (in the room) for each person: send them the invite, paste their reply back, compare four words, then admit them.',
  sendReply:
    'Send this reply back to the host (any chat app is fine). The host pastes it and lets you in.',
  compareMember: 'Read these four words to the host. They must match what the host sees next to your name.',
  compareHost: 'Ask them to read their four words. Admit only if they match exactly.',
  waitingForAdmit: 'Waiting for the host to let you in...',
  passwordAfterAdmit: 'The host let you in. Now enter the room password.',
  joinFailed: 'Could not join the room.',
  hostPanelIntro:
    'Each invite is for one person and works once. Send it by any chat app, then paste the reply you get back.',
  pasteReply: 'Paste a reply',
  noInvites: 'No invites yet.',
  reinviteHint: 'Re-invite: an invite only this member can use, to get back into their kept seat.',
  status: {
    preparing: 'Preparing...',
    waiting: 'Waiting for a reply',
    replied: 'Wants to join',
    admitted: 'Connecting...',
    joined: 'Joined',
    refused: 'Refused',
    expired: 'Expired',
    failed: 'Could not connect',
  },
} as const;
