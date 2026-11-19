import { useEffect, useState } from 'react';
import logoUrl from '../assets/logo-lockup.svg';
import { FilesSettingsSection, NetworkSettingsSection, NumberSetting } from '../components/TorrentSettings';
import { connection, type ConnectionStatus } from '../lib/connection';
import { inTauri } from '../lib/player';
import { redact } from '../lib/redact';
import { call, room, torrents } from '../lib/room';
import { settingsText } from '../strings/connection-help';
import { Icon } from '../ui/icons';
import { SettingsConnection } from './SettingsConnection';

/** Mirrors apps/desktop/src-tauri/src/prefs.rs. */
export interface AppPrefs {
  hwdec: boolean;
  subScalePct: number;
  preset: 'everyone' | 'onlyMe';
  gateTimeoutSecs: number;
  noLimitDefault: boolean;
  /** Manual invites (ADR 0014). */
  inviteTtlMins: number;
  seatKeepMins: number;
}
export const PREFS_DEFAULTS: AppPrefs = { hwdec: true, subScalePct: 100, preset: 'everyone', gateTimeoutSecs: 15, noLimitDefault: false, inviteTtlMins: 10, seatKeepMins: 5 };
export const prefs = {
  get: () => (inTauri() ? call<AppPrefs>('prefs_get') : Promise.resolve(PREFS_DEFAULTS)),
  set: (p: AppPrefs) => call<void>('prefs_set', { prefs: p }),
};

export const SECTIONS = ['Connection', 'Playback', 'Room defaults', 'Chat', 'Files', 'Network', 'About'] as const;
export type Section = (typeof SECTIONS)[number];

const card = 'mt-6 space-y-3 rounded-xl border border-border bg-elevated p-5 text-sm first:mt-0';
const btn = 'rounded-md border border-border bg-surface px-3 py-1.5 text-sm hover:border-accent disabled:opacity-50';

export function Settings({ status, onReplace, onBack, initial = 'Connection' }: { status: ConnectionStatus; onReplace: () => void; onBack: () => void; initial?: Section }) {
  const [section, setSection] = useState<Section>(initial);
  const [p, setP] = useState<AppPrefs | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  useEffect(() => {
    void prefs
      .get()
      .then(setP)
      .catch(() => setP(PREFS_DEFAULTS));
  }, []);
  const save = (next: AppPrefs) => {
    setP(next);
    void prefs
      .set(next)
      .then(() => setMsg('Saved.'))
      .catch((e: unknown) => setMsg(String(e)));
  };
  return (
    <div className="flex h-full overflow-hidden bg-bg">
      <nav className="w-52 shrink-0 border-r border-border bg-surface px-3 py-6" aria-label="Settings sections">
        <button type="button" className="mb-4 inline-flex items-center gap-1 text-sm text-muted hover:text-text" onClick={onBack}>
          <Icon name="back" />
          Back
        </button>
        <h1 className="mb-3 px-2 font-brand text-xl font-bold">Settings</h1>
        <ul className="space-y-0.5">
          {SECTIONS.map((s) => (
            <li key={s}>
              <button
                type="button"
                aria-current={section === s ? 'page' : undefined}
                className={`w-full rounded-md px-2 py-1.5 text-left text-sm ${section === s ? 'bg-elevated text-text' : 'text-muted hover:text-text'}`}
                onClick={() => setSection(s)}
              >
                {s}
              </button>
            </li>
          ))}
        </ul>
      </nav>
      <main className="min-w-0 flex-1 overflow-y-auto px-8 py-8" aria-label={section}>
        <div className="mx-auto max-w-2xl">
          <h2 className="mb-4 font-brand text-2xl font-bold">{section === 'Connection' ? settingsText.title : section}</h2>
          {section === 'Connection' && <SettingsConnection status={status} onReplace={onReplace} />}
          {section === 'Playback' && p && <PlaybackSection p={p} save={save} />}
          {section === 'Room defaults' && p && <RoomDefaultsSection p={p} save={save} />}
          {section === 'Chat' && <ChatSection />}
          {section === 'Files' && p && <FilesSection p={p} save={save} />}
          {section === 'Network' && <NetworkSection status={status} />}
          {section === 'About' && <AboutSection status={status} prefsNow={p} />}
          {msg && section !== 'Connection' && section !== 'About' && <p className="mt-3 text-xs text-muted">{msg}</p>}
        </div>
      </main>
    </div>
  );
}

function PlaybackSection({ p, save }: { p: AppPrefs; save: (p: AppPrefs) => void }) {
  return (
    <>
      <section className={card} aria-label="Video">
        <label className="flex items-start gap-3">
          <input type="checkbox" className="mt-1" checked={p.hwdec} onChange={(e) => save({ ...p, hwdec: e.target.checked })} />
          <span>
            <span className="font-medium">Hardware decoding</span>
            <span className="block text-muted">Uses the graphics card to decode video, with a safe fallback to software. Turn it off if the picture shows artifacts. Applies the next time SyncWatch starts.</span>
          </span>
        </label>
      </section>
      <section className={card} aria-label="Subtitles">
        <NumberSetting label="Subtitle size" unit="%" value={p.subScalePct} def={100} range={[50, 200]} help="For text subtitles (SRT, ASS). Image subtitles (PGS, VobSub) keep their own size. Applies the next time SyncWatch starts." testId="setting-sub-scale" onSave={(v) => save({ ...p, subScalePct: v })} />
        <p className="text-xs text-muted">Subtitles in languages your system has no font for use the bundled Noto Sans, so they never show as boxes.</p>
      </section>
      <section className={card} aria-label="Buffering (mpv cache)">
        <h3 className="font-medium">Buffering</h3>
        <ul className="list-disc space-y-1 pl-5 text-muted">
          <li>When data runs out, playback pauses cleanly instead of stuttering.</li>
          <li>It resumes once 2 seconds are buffered again, so a download that keeps up with playback does not pause and resume over and over.</li>
          <li>Playback never waits before the first frame.</li>
          <li>Local files read 20 seconds ahead; torrent streams read ahead as far as the download allows.</li>
        </ul>
        <p className="text-xs text-muted">These values are fixed: playback from data that is already on disk never pauses for buffering.</p>
      </section>
    </>
  );
}

function RoomDefaultsSection({ p, save }: { p: AppPrefs; save: (p: AppPrefs) => void }) {
  return (
    <section className={card} aria-label="New rooms">
      <fieldset className="space-y-2">
        <legend className="font-medium">Who controls playback in a new room</legend>
        {(
          [
            ['everyone', 'Everyone', 'Members can play, pause, seek and change the speed and the source.'],
            ['onlyMe', 'Only me', 'Only you control playback; members can still choose their own file and chat.'],
          ] as const
        ).map(([v, label, hint]) => (
          <label key={v} className="flex items-start gap-3">
            <input type="radio" name="preset" className="mt-1" checked={p.preset === v} onChange={() => save({ ...p, preset: v })} />
            <span>
              <span className="font-medium">{label}</span>
              <span className="block text-muted">{hint}</span>
            </span>
          </label>
        ))}
        <p className="text-xs text-muted">You can change it for each member in the room (Permissions...).</p>
      </fieldset>
      <NumberSetting
        label="Wait for members"
        unit="seconds"
        value={p.gateTimeoutSecs}
        def={15}
        range={[5, 120]}
        help="After a seek or a pause for buffering, the room waits this long for everyone, then plays without those still loading (you can always choose Resume without them)."
        testId="setting-gate-timeout"
        onSave={(v) => save({ ...p, gateTimeoutSecs: v })}
      />
      <NumberSetting
        label="Invites stay valid for"
        unit="minutes"
        value={p.inviteTtlMins}
        def={10}
        range={[1, 1440]}
        help="After this time an invite can no longer be used; make a new one. Shorter is safer if you post invites where others can see them."
        testId="setting-invite-ttl"
        onSave={(v) => save({ ...p, inviteTtlMins: v })}
      />
      <NumberSetting
        label="Keep a dropped member's seat for"
        unit="minutes"
        value={p.seatKeepMins}
        def={5}
        range={[1, 60]}
        help="If someone's connection is lost, use Re-invite in their menu: they get their seat back within this time."
        testId="setting-seat-keep"
        onSave={(v) => save({ ...p, seatKeepMins: v })}
      />
    </section>
  );
}

/** Chat limits (mirror crates/protocol/src/limits.rs and crates/sync-core/src/chat.rs). */
export const CHAT_LIMITS = [
  ['Message length', 'up to 500 characters (4 KB)'],
  ['Sending rate', 'up to 5 messages in 5 seconds'],
  ['Slow mode', 'set by the host for each room, up to 5 minutes between messages'],
  ['History', 'the last 2000 messages of the room; late joiners see the last 200'],
  ['After the room', 'nothing is kept: the chat ends with the room unless you export it'],
] as const;

function ChatSection() {
  const [msg, setMsg] = useState<string | null>(null);
  const [inRoom, setInRoom] = useState(false);
  useEffect(() => {
    void room
      .state()
      .then((s) => setInRoom(s.active))
      .catch(() => {});
  }, []);
  const exportChat = async () => {
    const { save } = await import('@tauri-apps/plugin-dialog');
    const path = await save({ defaultPath: 'SyncWatch chat.txt', filters: [{ name: 'Text', extensions: ['txt'] }] });
    if (!path) return;
    try {
      const n = await call<number>('room_chat_export', { path });
      setMsg(`Exported ${n} message${n === 1 ? '' : 's'}.`);
    } catch (e) {
      setMsg(String(e));
    }
  };
  return (
    <section className={card} aria-label="Chat">
      <dl className="grid grid-cols-[10rem_1fr] gap-x-3 gap-y-1">
        {CHAT_LIMITS.map(([k, v]) => (
          <div key={k} className="contents">
            <dt className="text-muted">{k}</dt>
            <dd>{v}</dd>
          </div>
        ))}
      </dl>
      <button type="button" className={btn} disabled={!inRoom} onClick={() => void exportChat()}>
        Export this room's chat...
      </button>
      <p className="text-xs text-muted">{inRoom ? 'Saves the chat of the room you are in as a text file.' : 'Available while you are in a room.'}</p>
      {msg && <p className="text-xs text-muted">{msg}</p>}
    </section>
  );
}

function FilesSection({ p, save }: { p: AppPrefs; save: (p: AppPrefs) => void }) {
  return (
    <>
      <section className={card} aria-label="Added subtitle and audio files">
        <h3 className="font-medium">Subtitle and audio files the host adds</h3>
        <dl className="grid grid-cols-[10rem_1fr] gap-x-3 gap-y-1">
          <dt className="text-muted">Subtitle files</dt>
          <dd>up to 20 MB</dd>
          <dt className="text-muted">Audio files</dt>
          <dd>up to 1 GB</dd>
        </dl>
        <label className="flex items-start gap-3">
          <input type="checkbox" className="mt-1" checked={p.noLimitDefault} onChange={(e) => save({ ...p, noLimitDefault: e.target.checked })} />
          <span>
            <span className="font-medium">No limit</span>
            <span className="block text-muted">New rooms start with "No size limit for added files" turned on. Members are asked before a large file is sent to them.</span>
          </span>
        </label>
      </section>
      <FilesSettingsSection />
    </>
  );
}

function NetworkSection({ status }: { status: ConnectionStatus }) {
  const [relayOnly, setRelayOnly] = useState(status.relayOnly);
  return (
    <>
      <section className={card} aria-label="Relay">
        <label className="flex items-start gap-3">
          <input
            type="checkbox"
            className="mt-1"
            checked={relayOnly}
            onChange={(e) => {
              setRelayOnly(e.target.checked);
              void connection.setRelayOnly(e.target.checked);
            }}
          />
          <span>
            <span className="font-medium">{settingsText.relayOnly}</span>
            <span className="block text-muted">{settingsText.relayOnlyHint}</span>
          </span>
        </label>
        <p className="text-xs text-muted">{settingsText.torrentNote}</p>
      </section>
      <NetworkSettingsSection />
    </>
  );
}

const GPL_NOTICE =
  'SyncWatch is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, version 3. It is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for more details.';

export const THIRD_PARTY = [
  ['mpv (libmpv)', 'GPL-2.0-or-later'],
  ['Tauri, WebView2 runtime', 'MIT or Apache-2.0; Microsoft'],
  ['librqbit', 'Apache-2.0'],
  ['Vulkan loader', 'Apache-2.0 and MIT'],
  ['Lucide icons', 'ISC (Feather-derived icons: MIT)'],
  ['Inter, Space Grotesk, Noto Sans', 'SIL Open Font License 1.1'],
] as const;

function AboutSection({ status, prefsNow }: { status: ConnectionStatus; prefsNow: AppPrefs | null }) {
  const [version, setVersion] = useState<string>('');
  const [copied, setCopied] = useState<string | null>(null);
  useEffect(() => {
    if (!inTauri()) return;
    void import('@tauri-apps/api/app')
      .then(({ getVersion }) => getVersion())
      .then(setVersion)
      .catch(() => {});
  }, []);
  const copyDiagnostics = async () => {
    const [rs, ts] = await Promise.all([room.state().catch(() => null), inTauri() ? torrents.settings().catch(() => null) : Promise.resolve(null)]);
    const report = {
      app: { version, userAgent: navigator.userAgent },
      connection: { configured: status.configured, verified: status.verified, mode: status.mode, relayOnly: status.relayOnly, warning: status.warning ?? null },
      prefs: prefsNow,
      torrent: ts,
      room: rs ? { active: rs.active, phase: rs.phase, isHost: rs.isHost, members: rs.members.length, diag: rs.diag } : null,
    };
    const text = redact(JSON.stringify(report, null, 2));
    await navigator.clipboard?.writeText(text);
    setCopied('Diagnostics copied (credentials, keys, tokens and IP addresses removed).');
  };
  return (
    <section className={card} aria-label="About SyncWatch">
      <img src={logoUrl} alt="SyncWatch" className="h-10" />
      <p>
        Version <span className="tabular">{version || '0.1.0'}</span>
      </p>
      <p className="text-xs text-muted">{GPL_NOTICE}</p>
      <h3 className="pt-2 font-medium">Third-party software</h3>
      <dl className="grid grid-cols-[14rem_1fr] gap-x-3 gap-y-1">
        {THIRD_PARTY.map(([k, v]) => (
          <div key={k} className="contents">
            <dt className="text-muted">{k}</dt>
            <dd>{v}</dd>
          </div>
        ))}
      </dl>
      <p className="text-xs text-muted">The full license texts ship with SyncWatch in the licenses folder.</p>
      <button type="button" className={btn} onClick={() => void copyDiagnostics()}>
        Copy diagnostics
      </button>
      {copied && (
        <p className="text-xs text-muted" role="status">
          {copied}
        </p>
      )}
    </section>
  );
}
