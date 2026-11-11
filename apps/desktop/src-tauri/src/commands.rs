//! Tauri app setup, commands and player event forwarding.

#![cfg(windows)]

use crate::native::loader::{NativeLibs, VulkanSource};
use crate::native::video_surface::{Rect, VideoSurface};
use crate::paths::AppPaths;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use syncwatch_player::mpv::{MpvConfig, MpvPlayer};
use syncwatch_player::{PlaybackState, Player, PlayerEvent, Track, TrackKind, TrackSelection};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

pub struct AppState {
    pub(crate) player: Arc<MpvPlayer>,
    /// `app_ready` runs once (React StrictMode calls effects twice in development).
    ready: std::sync::atomic::AtomicBool,
    surface: Arc<VideoSurface>,
    vulkan: String,
}

impl AppState {
    /// Whether the mpv video window is currently shown (debug checks).
    pub fn video_visible(&self) -> bool {
        self.surface.is_video_visible()
    }
}

/// Friendly, user-facing error string for commands.
type CmdResult<T> = Result<T, String>;

fn friendly(e: syncwatch_player::PlayerError) -> String {
    tracing::warn!(error = %e, "player command failed");
    format!("The player could not do that: {e}")
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutRects {
    /// Center column in physical client px.
    center: Rect,
    /// Visible side columns in physical client px (dev probe only).
    #[serde(default)]
    columns: Vec<Rect>,
    #[serde(default)]
    fullscreen: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    state: PlaybackState,
    tracks: Vec<Track>,
    vulkan: String,
    dev: bool,
}

fn on_main(app: &AppHandle, f: impl FnOnce() + Send + 'static) {
    if let Err(e) = app.run_on_main_thread(f) {
        tracing::warn!(error = %e, "could not schedule on the UI thread");
    }
}

#[tauri::command]
fn app_ready(app: AppHandle, window: WebviewWindow, state: State<'_, AppState>) {
    if state.ready.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let _ = window.show();
    let surface = Arc::clone(&state.surface);
    on_main(&app, move || surface.set_wanted(true));
    // Dev convenience: auto-open a file (e.g. D:\media\test.mkv) from the environment.
    if cfg!(debug_assertions)
        && let Ok(media) = std::env::var("SYNCWATCH_DEV_MEDIA")
        && !media.is_empty()
    {
        tracing::info!("dev: auto-loading SYNCWATCH_DEV_MEDIA");
        if let Err(e) = state.player.load(&media) {
            tracing::warn!(error = %e, "dev media load failed");
        }
    }
}

#[tauri::command]
fn set_layout(app: AppHandle, state: State<'_, AppState>, rects: LayoutRects) {
    let surface = Arc::clone(&state.surface);
    on_main(&app, move || {
        surface.set_layout(rects.center, rects.columns, rects.fullscreen)
    });
}

#[tauri::command]
fn set_fullscreen(window: WebviewWindow, on: bool) -> CmdResult<()> {
    window
        .set_fullscreen(on)
        .map_err(|e| format!("Could not change fullscreen: {e}"))
}

#[tauri::command]
fn snapshot(state: State<'_, AppState>) -> Snapshot {
    Snapshot {
        state: state.player.state(),
        tracks: state.player.tracks(),
        vulkan: state.vulkan.clone(),
        dev: cfg!(debug_assertions),
    }
}

#[tauri::command]
fn player_open(state: State<'_, AppState>, path: String) -> CmdResult<()> {
    let p = PathBuf::from(&path);
    if !p.is_file() {
        return Err("That file could not be found.".into());
    }
    state.player.load(&path).map_err(friendly)
}

#[tauri::command]
fn player_set_paused(state: State<'_, AppState>, paused: bool) -> CmdResult<()> {
    state.player.set_paused(paused).map_err(friendly)
}

#[tauri::command]
fn player_seek(state: State<'_, AppState>, seconds: f64) -> CmdResult<()> {
    state.player.seek(seconds).map_err(friendly)
}

#[tauri::command]
fn player_seek_relative(state: State<'_, AppState>, delta: f64) -> CmdResult<()> {
    state.player.seek_relative(delta).map_err(friendly)
}

#[tauri::command]
fn player_set_volume(state: State<'_, AppState>, volume: f64) -> CmdResult<()> {
    state.player.set_volume(volume).map_err(friendly)
}

#[tauri::command]
fn player_set_mute(state: State<'_, AppState>, mute: bool) -> CmdResult<()> {
    state.player.set_mute(mute).map_err(friendly)
}

#[tauri::command]
fn player_select_track(
    state: State<'_, AppState>,
    kind: TrackKind,
    selection: TrackSelection,
) -> CmdResult<()> {
    state.player.select_track(kind, selection).map_err(friendly)
}

#[tauri::command]
fn player_add_external(state: State<'_, AppState>, kind: TrackKind, path: String) -> CmdResult<()> {
    let p = PathBuf::from(&path);
    if !p.is_file() {
        return Err("That file could not be found.".into());
    }
    state.player.add_external(kind, &p, true).map_err(friendly)
}

fn forward_events(app: AppHandle, rx: std::sync::mpsc::Receiver<PlayerEvent>) {
    let _ = std::thread::Builder::new()
        .name("player-forward".into())
        .spawn(move || {
            while let Ok(ev) = rx.recv() {
                if let PlayerEvent::FileLoaded { tracks } = &ev {
                    tracing::info!(tracks = tracks.len(), "file loaded");
                }
                let shutdown = matches!(ev, PlayerEvent::Shutdown);
                // The active room (sync-core) observes the real player.
                if let Some(rooms) = app.try_state::<crate::room::RoomManager>() {
                    rooms.player_event(&ev);
                }
                let _ = app.emit("player-event", &ev);
                if shutdown {
                    break;
                }
            }
        });
}

pub fn run(paths: AppPaths, libs: NativeLibs) {
    let vulkan = match &libs.vulkan {
        VulkanSource::System => "system".to_owned(),
        VulkanSource::Bundled(_) => "bundled".to_owned(),
    };
    let fonts_dir = paths.exe_dir.join("fonts");
    let probe = (std::env::var("SYNCWATCH_DEV_LAYOUT_PROBE").as_deref() == Ok("1")).then(|| {
        let dir = paths.root.join("dev");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("layout-probe.json")
    });

    let conn_paths = paths.clone();
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            syncwatch_ice_config::install_crypto_provider();
            app.manage(crate::connection::ConnectionManager::new(&conn_paths));
            crate::connection::spawn_background_retest(app.handle().clone());
            tracing::info!(signaling_url = %syncwatch_net::signaling::configured_url(), "signaling server for this instance");
            let (bridge, events) = crate::bridge::Bridge::start(app.handle())?;
            app.manage(Arc::clone(&bridge));
            app.manage(crate::room::RoomManager::new(&conn_paths));
            // The torrent engine actor (idle: no session, no sockets until a torrent starts).
            let tp = conn_paths.clone();
            app.manage(tauri::async_runtime::block_on(async move {
                crate::torrent::TorrentService::start(&tp)
            }));
            if let Some(events) = crate::netlab::maybe_start(app.handle(), bridge, events) {
                crate::room::RoomManager::forward_transport(app.handle().clone(), events);
                crate::room::dev_autoroom(app.handle());
            }
            let window = app
                .get_webview_window("main")
                .ok_or("main window missing from tauri.conf.json")?;
            let main_hwnd = window.hwnd()?;
            crate::native::title_bar::apply(main_hwnd);
            let surface = VideoSurface::attach(main_hwnd, probe.clone())?;

            let mut cfg = MpvConfig::new(surface.video_hwnd() as i64);
            if fonts_dir.is_dir() {
                cfg.sub_fonts_dir = Some(fonts_dir.clone());
            }
            let (player, rx) = MpvPlayer::new(cfg).map_err(|e| e.to_string())?;
            forward_events(app.handle().clone(), rx);
            app.manage(AppState {
                player: Arc::new(player),
                ready: std::sync::atomic::AtomicBool::new(false),
                surface: Arc::clone(&surface),
                vulkan: vulkan.clone(),
            });

            // Safety net: show the window even if the UI never reports its first paint.
            let w = window.clone();
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(4));
                if !w.is_visible().unwrap_or(true) {
                    tracing::warn!("UI did not report first paint in time; showing window");
                    let _ = w.show();
                    on_main(&handle, move || surface.set_wanted(true));
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_ready,
            set_layout,
            set_fullscreen,
            snapshot,
            player_open,
            player_set_paused,
            player_seek,
            player_seek_relative,
            player_set_volume,
            player_set_mute,
            player_select_track,
            player_add_external,
            crate::connection::connection_status,
            crate::connection::connection_split_url,
            crate::connection::connection_test_api_key,
            crate::connection::connection_test_ice,
            crate::connection::connection_retest,
            crate::connection::connection_remove,
            crate::connection::connection_set_relay_only,
            crate::connection::open_help_link,
            crate::media::magnet_inspect,
            crate::media::prepare_change_media,
            crate::torrent::torrent_metadata,
            crate::torrent::torrent_settings_get,
            crate::torrent::torrent_settings_set,
            crate::room::room_set_torrent_media,
            crate::room::room_create,
            crate::room::room_join,
            crate::room::room_state,
            crate::room::room_leave,
            crate::room::room_password,
            crate::room::room_action,
            crate::room::room_choose_file,
            crate::room::room_set_local_media,
            crate::room::room_select_track,
            crate::room::room_add_external,
            crate::room::dev_scenario,
            crate::room::dev_scenario_report,
            crate::room::dev_window,
            crate::room::dev_reconnect,
            crate::room::dev_scenario_share,
            crate::room::dev_scenario_shared,
            crate::bridge::bridge_attach,
            crate::bridge::bridge_in,
        ])
        .run(tauri::generate_context!());

    if let Err(e) = result {
        tracing::error!(error = %e, "tauri runtime error");
        crate::native::loader::error_dialog(&format!("SyncWatch could not start: {e}"));
    }
}
