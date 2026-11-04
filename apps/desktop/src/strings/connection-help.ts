// ALL connection help and error text (spec section 5). Metered's menu names appear ONLY in
// this file (enforced by connection-help.test.ts). Lines that have not been verified against
// Metered's real behavior are marked TODO-VERIFY.

/** Rich text: plain strings and bold UI names. */
export type Rich = Array<string | { b: string }>;

export const METERED = {
  site: 'metered.ca',
  /** Shown as a format hint only. Placeholders, never real values. */
  credentialsUrlFormat: 'https://<app-name>.metered.live/api/v1/turn/credentials?apiKey=<API_KEY>',
} as const;

export const setupText = {
  title: 'Set up your connection',
  intro:
    'SyncWatch connects you to friends directly. To get through firewalls it needs your own free Metered account. You only do this once.',
  tabs: { apiKey: 'API key', iceServers: 'ICE servers' },
  tabHints: { apiKey: 'Recommended', iceServers: 'Advanced' },
  openSite: 'Open metered.ca',
  testButton: 'Test connection',
  testing: 'Testing your connection...',
  saved: 'Connected. Your setup is saved.',
  notSaved: 'Not saved: no relay connection worked yet.',
  devBypass: 'Developer mode: Create and Join are allowed without a verified connection.',
  note: [
    'Your free Metered account is used only to connect through firewalls. Your video never goes through it.',
    'Check your plan quota on Metered’s site.',
  ],
  // TODO-VERIFY: not confirmed that the free plan covers typical use.
  freePlanEnough: 'The free plan should be enough for watching together.',
  apiKey: {
    heading: 'Get your API key',
    steps: [
      ['Create a free account at ', { b: 'metered.ca' }, '.'],
      ['Create an app and open it.'],
      ['In the left section open ', { b: 'TURN Server' }, '.'],
      ['Scroll down to ', { b: 'TURN Credentials' }, ' and press ', { b: 'Show API Key' }, '.'],
      ['Copy the key (your app name is the part before ', { b: '.metered.live' }, ') and paste both here.'],
      ['Press ', { b: 'Test connection' }, '.'],
    ] satisfies Rich[],
    appLabel: 'App name',
    appPlaceholder: 'the part before .metered.live',
    keyLabel: 'API key',
    keyPlaceholder: 'paste the key from Show API Key',
    urlLabel: 'Or paste the whole credentials URL',
    urlPlaceholder: METERED.credentialsUrlFormat,
    urlSplit: 'Got it: app name and key filled in.',
  },
  iceServers: {
    heading: 'Copy the ICE servers array',
    steps: [
      ['Create a free account at ', { b: 'metered.ca' }, '.'],
      ['Create an app and open it.'],
      ['In the left section open ', { b: 'TURN Server' }, '.'],
      ['Scroll down to ', { b: 'TURN Credentials' }, ' and press ', { b: 'Show ICE Servers Array' }, '.'],
      ['Copy the whole array and paste it here.'],
      ['Press ', { b: 'Test connection' }, '.'],
    ] satisfies Rich[],
    pasteLabel: 'ICE servers',
    pastePlaceholder: 'Paste the array exactly as Metered shows it (JavaScript is fine; it is read as data, never run).',
  },
};

/** Errors before any network access. */
export const inputErrors: Record<string, Rich> = {
  badAppName: ['The app name looks wrong. It is the part before ', { b: '.metered.live' }, ' (step 5).'],
  badKey: ['The API key looks wrong. Go back to step 4 and press ', { b: 'Show API Key' }, ' again.'],
  badUrl: ['That is not a Metered credentials URL. It looks like ', METERED.credentialsUrlFormat, '.'],
};

/** Errors while fetching credentials from Metered (Tab A). */
export const fetchErrors: Record<string, Rich> = {
  badKey: ['Metered rejected this API key. Go back to step 4, press ', { b: 'Show API Key' }, ' and copy the key again.'],
  // TODO-VERIFY: Metered's quota-exhausted response is unknown, so this is a generic error plus a hint.
  server: ['Metered returned an error. Try again in a moment. If it keeps happening, your plan limit may be reached: check your quota on metered.ca.'],
  dns: ['Could not find Metered’s server. Check your internet connection and the app name (step 5).'],
  refused: ['Metered refused the connection. A firewall may be blocking it; try again later or use the ICE servers tab.'],
  timeout: ['Metered did not answer in time. Check your internet connection and try again.'],
  tls: ['The secure connection to Metered failed. Check the date and time on this PC, or a proxy that inspects HTTPS.'],
  offline: ['You seem to be offline. Check your internet connection.'],
  badResponse: ['Metered sent an unexpected answer. Try again, or use the ICE servers tab (step 4: ', { b: 'Show ICE Servers Array' }, ').'],
  badRedirect: ['Metered redirected to an address SyncWatch does not trust, so your key was not sent there.'],
  input: ['The app name or key looks wrong (steps 4 and 5).'],
};

/** Errors reading the pasted ICE servers (Tab B). Each points back to the right step. */
export const parseErrors: Record<string, Rich> = {
  empty: ['Paste the ICE servers array first (step 5).'],
  tooLarge: ['That is much more than an ICE servers array. Copy only the array (step 5).'],
  noArray: ['No ICE servers array found. Go back to step 4, press ', { b: 'Show ICE Servers Array' }, ' and copy the whole array.'],
  syntax: ['The pasted text could not be read. Copy the whole array again (step 5), including the brackets.'],
  noServers: ['The array is empty. Copy it again from ', { b: 'Show ICE Servers Array' }, ' (step 4).'],
  tooMany: ['Too many servers in the array (at most 16).'],
  badUrl: ['One of the server addresses looks wrong. Copy the array again (step 5).'],
  badScheme: ['Only stun:, stuns:, turn: and turns: addresses are allowed. Copy the array again (step 5).'],
  missingCredential: ['A TURN server is missing its username or credential. Copy the whole array (step 5), not just the addresses.'],
  tooLong: ['A value in the array is too long. Copy the array again (step 5).'],
};

/** Per-URL connection test results. */
export const probeErrors: Record<string, string> = {
  dns: 'server name not found',
  refused: 'connection refused',
  timeout: 'no answer (port blocked?)',
  tls: 'secure connection failed',
  badCredentials: 'username or credential rejected',
  // TODO-VERIFY: assumed to correspond to Metered's quota limit.
  quota: 'allocation quota reached (your plan limit may be reached)',
  capacity: 'server busy',
  server: 'server error',
  protocol: 'unexpected answer',
  offline: 'no network',
};

export const resultText = {
  relay: 'relay',
  stun: 'reachability',
  ok: 'OK',
  fail: 'Failed',
  verified: 'At least one relay works. You can connect through firewalls.',
  notVerified: 'No relay worked, so SyncWatch cannot connect through firewalls yet.',
  released: 'Test allocations were released right away and used no quota.',
};

export const statusText = {
  banner: 'Set up your connection',
  bannerBody: 'Create and Join need a working connection setup.',
  bannerButton: 'Set up now',
  verified: 'Connection verified',
  notVerified: 'Not verified',
  notConfigured: 'Not set up',
  warning: {
    meteredUnreachable: 'Metered could not be reached just now. Your last verified setup is still used.',
    usingCachedServers: 'Fresh connection details could not be fetched; the last good ones are used.',
    storeUnavailable: 'The Windows credential store is not available, so your setup could not be saved.',
  } as Record<string, string>,
};

export const settingsText = {
  title: 'Connection',
  hostnames: 'Servers',
  test: 'Test',
  replace: 'Replace',
  remove: 'Remove',
  removeConfirm: 'Remove the saved connection setup?',
  removeYes: 'Remove',
  removeNo: 'Keep',
  help: 'Show setup help',
  saved: 'Saved securely in Windows Credential Manager.',
  savedPortable: 'Saved encrypted for this Windows user on this PC (portable mode).',
  masked: '•••••••• (hidden)',
  relayOnly: 'Always use relay (hide my IP address)',
  relayOnlyHint: 'All traffic goes through your Metered relay, so other members never see your IP address. Uses more of your quota.',
  torrentNote: 'Torrent downloads are separate: other people in the torrent swarm see your IP address regardless of this setting.',
};
