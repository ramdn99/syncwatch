// Bridge to the Rust connection commands (crates/ice-config + src-tauri/src/connection.rs).

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { inTauri } from './player';

export type ProbeError =
  | 'dns' | 'refused' | 'timeout' | 'tls' | 'badCredentials' | 'quota' | 'capacity' | 'server' | 'protocol' | 'offline';

export interface ResultSummary {
  label: string;
  host: string;
  relay: boolean;
  ok: boolean;
  latencyMs: number | null;
  error: ProbeError | null;
}

export interface ConnectionStatus {
  configured: boolean;
  mode: 'apiKey' | 'iceServers' | null;
  hostnames: string[];
  verified: boolean;
  verifiedAtUnix: number | null;
  relayOnly: boolean;
  lastResults: ResultSummary[];
  warning: 'meteredUnreachable' | 'usingCachedServers' | 'storeUnavailable' | null;
  devBypass: boolean;
  canConnect: boolean;
  portable: boolean;
}

export interface UrlResult {
  label: string;
  host: string;
  kind: 'reflexive' | 'relay';
  ok: boolean;
  latencyMs: number | null;
  error: ProbeError | null;
  released: boolean;
  stunCode: number | null;
}

export interface TestReport {
  results: UrlResult[];
  verified: boolean;
  bytesSent: number;
  bytesReceived: number;
  allocations: number;
  allocationsReleased: number;
}

export type TestOutcome =
  | { outcome: 'inputError'; error: 'badAppName' | 'badKey' | 'badUrl' }
  | { outcome: 'parseError'; error: string; code: string }
  | { outcome: 'fetchError'; error: { kind: string; status?: number } }
  | { outcome: 'tested'; report: TestReport; saved: boolean }
  | { outcome: 'storeError' };

export const EMPTY_STATUS: ConnectionStatus = {
  configured: false,
  mode: null,
  hostnames: [],
  verified: false,
  verifiedAtUnix: null,
  relayOnly: false,
  lastResults: [],
  warning: null,
  devBypass: false,
  canConnect: false,
  portable: false,
};

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T | undefined> {
  if (!inTauri()) return undefined;
  return invoke<T>(cmd, args);
}

export const connection = {
  status: () => call<ConnectionStatus>('connection_status'),
  splitUrl: (url: string) => call<{ app: string; key: string }>('connection_split_url', { url }),
  testApiKey: (appName: string, key: string) => call<TestOutcome>('connection_test_api_key', { appName, key }),
  testIce: (text: string) => call<TestOutcome>('connection_test_ice', { text }),
  retest: () => call<TestOutcome | null>('connection_retest'),
  remove: () => call<ConnectionStatus>('connection_remove'),
  setRelayOnly: (on: boolean) => call<ConnectionStatus>('connection_set_relay_only', { on }),
  openHelp: (id: 'metered' | 'meteredDashboard') => call<void>('open_help_link', { id }),
};

export function onConnectionStatus(cb: (s: ConnectionStatus) => void): () => void {
  if (!inTauri()) return () => {};
  let un: (() => void) | undefined;
  let cancelled = false;
  void listen<ConnectionStatus>('connection-status', (e) => cb(e.payload)).then((u) => {
    if (cancelled) u();
    else un = u;
  });
  return () => {
    cancelled = true;
    un?.();
  };
}

/** Client-side mirror of the Rust credentials-URL check, for instant feedback only. */
export function looksLikeCredentialsUrl(text: string): boolean {
  return /^https:\/\/[a-z0-9_-]+\.metered\.live\/api\/v1\/turn\/credentials\?/i.test(text.trim());
}
