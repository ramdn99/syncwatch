import { useEffect, useId, useState } from 'react';
import { inTauri } from '../lib/player';
import { TORRENT_NOTICE, torrents, type TorrentSettings } from '../lib/room';

/** Defaults and allowed ranges (mirrors apps/desktop/src-tauri/src/torrent.rs). */
export const TORRENT_DEFAULTS = { metaCapMb: 5, windowBeforeSecs: 1, windowAfterSecs: 5, startupKb: 1024 } as const;
export const TORRENT_RANGES = {
  metaCapMb: [1, 16],
  windowBeforeSecs: [0, 10],
  windowAfterSecs: [2, 60],
  startupKb: [256, 8192],
} as const;

/** Why a value is refused (null when it is valid). */
export function rangeError(v: number, [lo, hi]: readonly [number, number], unit: string): string | null {
  if (!Number.isFinite(v) || !Number.isInteger(v)) return 'Enter a whole number.';
  if (v < lo || v > hi) return `Allowed: ${lo} to ${hi} ${unit}.`;
  return null;
}

/**
 * A number setting: shows its default, offers "Reset to default", and saves only valid values
 * (an invalid entry shows why and is not saved).
 */
export function NumberSetting(props: {
  label: string;
  unit: string;
  value: number;
  def: number;
  range: readonly [number, number];
  help?: string;
  testId?: string;
  onSave: (v: number) => void;
}) {
  const id = useId();
  const [text, setText] = useState(String(props.value));
  useEffect(() => setText(String(props.value)), [props.value]);
  const n = Number(text);
  const err = text.trim() === '' ? 'Enter a number.' : rangeError(n, props.range, props.unit);
  const commit = () => {
    if (!err && n !== props.value) props.onSave(n);
  };
  return (
    <div className="space-y-1" data-testid={props.testId}>
      <div className="flex flex-wrap items-center gap-2">
        <label htmlFor={id} className="text-muted">
          {props.label}
        </label>
        <input
          id={id}
          type="number"
          inputMode="numeric"
          min={props.range[0]}
          max={props.range[1]}
          className={`w-24 rounded-md border bg-surface px-2 py-1 text-sm tabular ${err ? 'border-danger' : 'border-border'}`}
          value={text}
          aria-invalid={err != null}
          aria-describedby={`${id}-hint`}
          onChange={(e) => setText(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => e.key === 'Enter' && commit()}
        />
        <span className="text-muted">{props.unit}</span>
        <button
          type="button"
          className="rounded-md border border-border px-2 py-0.5 text-xs hover:bg-surface disabled:opacity-50"
          disabled={props.value === props.def && text === String(props.def)}
          onClick={() => {
            setText(String(props.def));
            props.onSave(props.def);
          }}
        >
          Reset to default
        </button>
      </div>
      <p id={`${id}-hint`} className={`text-xs ${err ? 'text-danger' : 'text-muted'}`} role={err ? 'alert' : undefined}>
        {err ?? `Default: ${props.def} ${props.unit}.`} {props.help}
      </p>
    </div>
  );
}

function useTorrentSettings() {
  const [s, setS] = useState<TorrentSettings | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  useEffect(() => {
    if (!inTauri()) return;
    void torrents
      .settings()
      .then(setS)
      .catch(() => {});
  }, []);
  const save = (next: TorrentSettings) => {
    setS(next);
    void torrents
      .saveSettings(next)
      .then(() => setMsg('Saved.'))
      .catch((e: unknown) => setMsg(String(e)));
  };
  return { s, setS, save, msg };
}

const field = 'w-28 rounded-md border border-border bg-surface px-2 py-1 text-sm';

/** Settings · Files: torrent cache, size cap, finished files, metadata cap, buffering window. */
export function FilesSettingsSection({ initial }: { initial?: TorrentSettings }) {
  const st = useTorrentSettings();
  const s = st.s ?? initial ?? null;
  if (!s) return null;
  const save = st.save;
  return (
    <section className="mt-6 space-y-4 rounded-xl border border-border bg-elevated p-5 text-sm" aria-label="Files" data-testid="settings-files">
      <h2 className="font-medium">Files</h2>
      <p className="text-xs text-warning">{TORRENT_NOTICE}</p>
      <label className="block">
        <span className="text-muted">Torrent cache folder (empty = default)</span>
        <input className="mt-1 w-full rounded-md border border-border bg-surface px-2 py-1" value={s.cacheDir ?? ''} onChange={(e) => st.setS({ ...s, cacheDir: e.target.value || null })} onBlur={() => save(s)} />
      </label>
      <label className="flex items-center gap-2">
        <span className="text-muted">Torrent cache size limit (GB)</span>
        <input type="number" min={1} className={field} value={s.capGb} onChange={(e) => save({ ...s, capGb: Math.max(1, Number(e.target.value) || 1) })} />
      </label>
      <label className="flex items-center gap-2">
        <input type="checkbox" checked={s.keepFinished} onChange={(e) => save({ ...s, keepFinished: e.target.checked })} />
        <span>Keep finished files after leaving a room (replayed later without joining the swarm again)</span>
      </label>
      <NumberSetting
        label="Largest torrent file list"
        unit="MB"
        value={s.metaCapMb}
        def={TORRENT_DEFAULTS.metaCapMb}
        range={TORRENT_RANGES.metaCapMb}
        help="Bigger .torrent files (or file lists offered by a host) are refused."
        testId="setting-meta-cap"
        onSave={(v) => save({ ...s, metaCapMb: v })}
      />
      <fieldset className="space-y-3 rounded-lg border border-border p-3">
        <legend className="px-1 text-muted">Buffering window</legend>
        <p className="text-xs text-muted">
          A torrent member counts as ready when the part around the room position is downloaded: this much before it (the player starts at the keyframe before) and this much after it.
          A larger window waits longer before playing but stalls less on a slow swarm.
        </p>
        <NumberSetting
          label="Before the position"
          unit="seconds"
          value={s.windowBeforeSecs}
          def={TORRENT_DEFAULTS.windowBeforeSecs}
          range={TORRENT_RANGES.windowBeforeSecs}
          testId="setting-window-before"
          onSave={(v) => save({ ...s, windowBeforeSecs: v })}
        />
        <NumberSetting
          label="After the position"
          unit="seconds"
          value={s.windowAfterSecs}
          def={TORRENT_DEFAULTS.windowAfterSecs}
          range={TORRENT_RANGES.windowAfterSecs}
          testId="setting-window-after"
          onSave={(v) => save({ ...s, windowAfterSecs: v })}
        />
        <NumberSetting
          label="Start-up range"
          unit="KB"
          value={s.startupKb}
          def={TORRENT_DEFAULTS.startupKb}
          range={TORRENT_RANGES.startupKb}
          help="What the player is expected to read to open a file (header and index). Measured: at most 512 KB for MKV and MP4."
          testId="setting-startup"
          onSave={(v) => save({ ...s, startupKb: v })}
        />
      </fieldset>
      {st.msg && <p className="text-xs text-muted">{st.msg}</p>}
    </section>
  );
}

/** Settings · Network: torrent port, bind address, DHT. */
export function NetworkSettingsSection({ initial }: { initial?: TorrentSettings }) {
  const st = useTorrentSettings();
  const s = st.s ?? initial ?? null;
  if (!s) return null;
  const save = st.save;
  return (
    <section className="mt-6 space-y-3 rounded-xl border border-border bg-elevated p-5 text-sm" aria-label="Network" data-testid="settings-network">
      <h2 className="font-medium">Network</h2>
      <label className="flex items-center gap-2">
        <span className="text-muted">Incoming torrent port (0 = random)</span>
        <input type="number" min={0} max={65535} className={field} value={s.port} onChange={(e) => save({ ...s, port: Math.min(65535, Math.max(0, Number(e.target.value) || 0)) })} />
      </label>
      <label className="flex items-center gap-2">
        <span className="text-muted">Bind address (empty = all)</span>
        <input className={field} value={s.bind} onChange={(e) => st.setS({ ...s, bind: e.target.value })} onBlur={() => save(s)} />
      </label>
      <label className="flex items-center gap-2">
        <input type="checkbox" checked={s.dht} onChange={(e) => save({ ...s, dht: e.target.checked })} />
        <span>Use DHT to find peers (only while a torrent is active)</span>
      </label>
      <p className="text-xs text-muted">
        When a file is complete, downloading and sharing stop and the torrent engine shuts down: no connections stay open. Other DHT nodes may still list your address for about 30
        minutes, but nobody can connect because every socket is closed.
      </p>
      {st.msg && <p className="text-xs text-muted">{st.msg}</p>}
    </section>
  );
}

/** Both torrent-related settings sections (kept for the existing Settings screen). */
export function TorrentSettingsSection() {
  return (
    <>
      <FilesSettingsSection />
      <NetworkSettingsSection />
    </>
  );
}
