mod capture;
mod clipboard;
mod cursor;
mod editor;
mod hotkeys;
mod migrate;
mod ocr;
mod overlay;
mod preview;
mod scroll;
mod store;

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, RwLock, RwLockWriteGuard};
use std::time::Duration;

use image::RgbaImage;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WindowEvent};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use capture::{MonitorInfo, WindowInfo};
use hotkeys::{HotkeyStatus, MECHANISM_HOOK, MECHANISM_NONE, MECHANISM_PLUGIN};
use store::{AppState, CaptureRecord, Settings};

const MAIN_LABEL: &str = "main";
/// The compositor needs a beat to actually drop a hidden window out of the
/// framebuffer; grabbing immediately still catches santi.sharex in the shot.
const HIDE_SETTLE: Duration = Duration::from_millis(120);
/// A `capture://error` emitted from `setup` arrives before the webview has
/// attached its listener and is simply lost, so the few failures the user has to
/// be told about wait for the page to mount.
const STARTUP_NOTICE_DELAY: Duration = Duration::from_millis(1200);

/// Whether the tray icon exists. Only ever false when `build_tray` failed, which
/// makes the main window the app's sole affordance — see `setup` and
/// `CloseRequested`.
static TRAY_ALIVE: AtomicBool = AtomicBool::new(true);

/// Tracks whether *we* hid the main window for the capture in flight, so a
/// region selection spanning several commands still restores it exactly once.
#[derive(Default)]
struct CaptureFlags {
    main_hidden: AtomicBool,
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// A capture panicking mid-write would poison the state mutexes and take every
/// later command down with it. The data behind them is plain owned values, so
/// recovering the inner value is always sound here.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The `RwLock` twin of [`lock`], for the hook's binding table. Same reasoning:
/// poison must not be able to strand the hotkeys.
pub(crate) fn write_lock<T>(rwlock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    rwlock
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Runs blocking work (screen grabs, PNG encoding, window creation) off the main
/// thread. `WebviewWindowBuilder::build` in particular deadlocks if it is driven
/// from the thread that owns the event loop.
pub(crate) async fn run_blocking<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("capture task failed: {e}"))?
}

pub(crate) fn hide_main_for_capture(app: &AppHandle) {
    let wanted = {
        let state = app.state::<AppState>();
        let settings = lock(&state.settings);
        settings.hide_window_on_capture
    };
    if !wanted {
        return;
    }

    let Some(window) = app.get_webview_window(MAIN_LABEL) else {
        return;
    };
    if !window.is_visible().unwrap_or(false) {
        return;
    }
    if window.hide().is_ok() {
        app.state::<CaptureFlags>()
            .main_hidden
            .store(true, Ordering::SeqCst);
        std::thread::sleep(HIDE_SETTLE);
    }
}

pub(crate) fn restore_main_after_capture(app: &AppHandle) {
    if !app
        .state::<CaptureFlags>()
        .main_hidden
        .swap(false, Ordering::SeqCst)
    {
        return;
    }
    if let Some(window) = app.get_webview_window(MAIN_LABEL) {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Takes a preview from the *previous* capture off screen before the next grab.
///
/// It is always-on-top, so left up it lands in the shot — and in the region
/// overlay's freeze frame, where it would then be selectable. Only pays the
/// settle delay when there was actually something on screen to take down.
pub(crate) fn hide_preview_for_capture(app: &AppHandle) {
    let was_visible = app
        .get_webview_window(preview::PREVIEW_LABEL)
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false);

    preview::hide(app);
    if was_visible {
        std::thread::sleep(HIDE_SETTLE);
    }
}

fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_LABEL) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Surfaces a startup failure on the channel the UI already listens on, late
/// enough that the listener is there to hear it.
fn report_startup_error(app: &AppHandle, message: String) {
    eprintln!("santi.sharex: {message}");
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        std::thread::sleep(STARTUP_NOTICE_DELAY);
        let _ = app.emit("capture://error", message);
    });
}

fn find_record(app: &AppHandle, id: &str) -> Result<CaptureRecord, String> {
    store::capture_by_id(app, id)
}

/// Two paths, tried in order (M2.7 §1).
///
/// The plugin goes first because it is one call. When it fails, the failure is
/// not usually about this image: it holds a single `arboard::Clipboard` for the
/// whole process lifetime, so a stale handle or a lost race with whatever else
/// touched the clipboard is a hard error with no retry behind it. The native
/// path in [`clipboard`] opens the clipboard fresh, retries while another app
/// has it, and writes `CF_DIBV5` plus `"PNG"` itself.
///
/// Every failure is logged to stderr as well as returned: the caller's only
/// channel is a `capture://error` toast, which is easy to miss and easy to
/// truncate, and these errors are the ones worth reading in full.
pub(crate) fn copy_rgba_to_clipboard(app: &AppHandle, img: &RgbaImage) -> Result<(), String> {
    let (w, h) = img.dimensions();
    let image = tauri::image::Image::new(img.as_raw(), w, h);

    let plugin_error = match app.clipboard().write_image(&image) {
        Ok(()) => return Ok(()),
        Err(e) => e.to_string(),
    };
    eprintln!(
        "santi.sharex: clipboard plugin write failed ({plugin_error}); trying the native path"
    );

    match clipboard::write_image(img) {
        Ok(()) => Ok(()),
        Err(native_error) => {
            let message = format!(
                "Could not copy to the clipboard — plugin: {plugin_error}; native fallback: {native_error}"
            );
            eprintln!("santi.sharex: {message}");
            Err(message)
        }
    }
}

/// Thumbnails are ours, so they always go. The capture itself only goes when the
/// caller asked for it.
fn remove_files(record: &CaptureRecord, delete_capture_file: bool) {
    if !record.thumb.is_empty() {
        let _ = fs::remove_file(&record.thumb);
    }
    if delete_capture_file && record.saved && !record.path.is_empty() {
        let _ = fs::remove_file(&record.path);
    }
}

// ---------------------------------------------------------------------------
// the one capture pipeline
// ---------------------------------------------------------------------------

pub(crate) fn finalize(
    app: &AppHandle,
    img: RgbaImage,
    kind: &str,
) -> Result<CaptureRecord, String> {
    let settings = {
        let state = app.state::<AppState>();
        let guard = lock(&state.settings);
        guard.clone()
    };

    let (width, height) = img.dimensions();
    let id = store::next_id();
    let target = store::resolve_filename(
        &settings.filename_pattern,
        kind,
        Path::new(&settings.save_dir),
    );
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("{kind}.png"));

    let mut path = String::new();
    let mut size_bytes: u64 = 0;
    let mut saved = false;

    if settings.save_to_disk {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        }
        let bytes = capture::encode_png(&img)?;
        fs::write(&target, &bytes)
            .map_err(|e| format!("could not write {}: {e}", target.display()))?;
        size_bytes = bytes.len() as u64;
        path = target.to_string_lossy().into_owned();
        saved = true;
    }

    let thumb_path = store::thumb_path(app, &id);
    let thumb_bytes = capture::encode_png_fast(&capture::thumbnail(&img))?;
    fs::write(&thumb_path, &thumb_bytes)
        .map_err(|e| format!("could not write the thumbnail: {e}"))?;

    let mut copied = false;
    if settings.copy_to_clipboard {
        match copy_rgba_to_clipboard(app, &img) {
            Ok(()) => copied = true,
            // A failed clipboard write must not lose an otherwise good capture.
            Err(e) => {
                let _ = app.emit("capture://error", e);
            }
        }
    }

    let record = CaptureRecord {
        id,
        name,
        path,
        thumb: thumb_path.to_string_lossy().into_owned(),
        width,
        height,
        kind: kind.to_string(),
        created_at: chrono::Utc::now().timestamp_millis(),
        size_bytes,
        saved,
        copied,
    };

    {
        let state = app.state::<AppState>();
        let mut history = lock(&state.history);
        history.insert(0, record.clone());
        store::persist_history(app, &mut history)?;
    }

    let _ = app.emit("capture://new", record.clone());

    // M2.9 §3. Fire-and-forget by construction: `preview::show` hands the work
    // to its own thread, so a preview that cannot be placed or shown can never
    // delay — or fail — the capture behind it.
    if settings.show_capture_preview {
        preview::show(app, &record);
    }

    if settings.open_folder_after {
        if saved {
            let _ = tauri_plugin_opener::reveal_item_in_dir(&record.path);
        } else {
            let _ = tauri_plugin_opener::open_path(&settings.save_dir, None::<&str>);
        }
    }

    // Last, after the emit: the history and every other post-capture behaviour
    // must land whether or not the editor opens. "edit" records come from the
    // editor itself — re-targeting it at its own output would fight with the
    // document already loaded there.
    if settings.open_editor_after && kind != "edit" {
        if let Err(e) = editor::open_editor_blocking(app, &record.id) {
            let _ = app.emit("capture://error", e);
        }
    }

    Ok(record)
}

fn grab<F>(app: &AppHandle, kind: &str, grabber: F) -> Result<CaptureRecord, String>
where
    F: FnOnce() -> Result<capture::Shot, String>,
{
    hide_preview_for_capture(app);
    hide_main_for_capture(app);
    let shot = grabber();
    restore_main_after_capture(app);

    let mut shot = shot?;
    // Drawn after the grab rather than captured with it: the framebuffer never
    // contains the cursor, so this is the only way it can appear at all.
    if cursor_wanted(app) {
        cursor::overlay_onto(&mut shot);
    }
    finalize(app, shot.image, kind)
}

/// Whether the cursor should be drawn into a capture. Read once per capture so a
/// mid-capture settings change cannot produce a half-applied result.
pub(crate) fn cursor_wanted(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    let wanted = lock(&state.settings).capture_cursor;
    wanted
}

// ---------------------------------------------------------------------------
// commands: settings
// ---------------------------------------------------------------------------

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> Settings {
    lock(&state.settings).clone()
}

#[tauri::command]
fn save_settings(app: AppHandle, mut settings: Settings) -> Result<Settings, String> {
    // A theme this build has no CSS block for would render an untokenised page,
    // so it never reaches disk or the frontend (M2.6 §5).
    settings.theme = store::normalize_theme(&settings.theme);
    // Same reasoning one field down: the overlay divides by this, so a value
    // outside 2x-20x never reaches disk or the frontend (M2.9 §1).
    settings.loupe_zoom = store::normalize_loupe_zoom(settings.loupe_zoom);

    store::persist_settings(&app, &settings)?;
    {
        let state = app.state::<AppState>();
        *lock(&state.settings) = settings.clone();
    }

    register_hotkeys(&app, &settings);
    sync_autostart(&app, settings.launch_at_login);
    // Turning the preview off has to take down the one already on screen, or
    // the setting reads as ignored until it happens to time out.
    if !settings.show_capture_preview {
        preview::hide(&app);
    }
    let _ = app.emit("settings://changed", settings.clone());
    Ok(settings)
}

/// Point the real Windows autostart registration at whatever the setting says.
///
/// Driven from the setting rather than the other way round, and applied on load
/// as well as on save, so an install that was never registered (or a
/// registration left behind by an uninstall that missed it) converges on the
/// next launch instead of drifting forever.
pub(crate) fn sync_autostart(app: &AppHandle, wanted: bool) {
    use tauri_plugin_autostart::ManagerExt;

    let manager = app.autolaunch();
    let current = manager.is_enabled().unwrap_or(false);
    if current == wanted {
        return;
    }
    let outcome = if wanted {
        manager.enable()
    } else {
        manager.disable()
    };
    if let Err(e) = outcome {
        // Not fatal: the app runs fine unregistered, and saying so is better
        // than a silently ignored toggle.
        let verb = if wanted { "enable" } else { "disable" };
        eprintln!("santi.sharex: could not {verb} launch at login: {e}");
        let _ = app.emit(
            "capture://error",
            format!("Could not {verb} launch at login: {e}"),
        );
    }
}

// ---------------------------------------------------------------------------
// commands: history
// ---------------------------------------------------------------------------

#[tauri::command]
fn get_history(state: State<'_, AppState>) -> Vec<CaptureRecord> {
    lock(&state.history).clone()
}

#[tauri::command]
async fn delete_capture(
    app: AppHandle,
    id: String,
    delete_file: bool,
) -> Result<Vec<CaptureRecord>, String> {
    run_blocking(move || {
        let (removed, remaining) = {
            let state = app.state::<AppState>();
            let mut history = lock(&state.history);
            let index = history.iter().position(|r| r.id == id);
            let removed = index.map(|i| history.remove(i));
            store::persist_history(&app, &mut history)?;
            (removed, history.clone())
        };

        if let Some(record) = removed {
            remove_files(&record, delete_file);
        }
        Ok(remaining)
    })
    .await
}

#[tauri::command]
async fn clear_history(app: AppHandle, delete_files: bool) -> Result<Vec<CaptureRecord>, String> {
    run_blocking(move || {
        let dropped = {
            let state = app.state::<AppState>();
            let mut history = lock(&state.history);
            let dropped = std::mem::take(&mut *history);
            store::persist_history(&app, &mut history)?;
            dropped
        };

        for record in &dropped {
            remove_files(record, delete_files);
        }
        Ok(Vec::new())
    })
    .await
}

#[tauri::command]
async fn copy_capture(app: AppHandle, id: String) -> Result<(), String> {
    run_blocking(move || {
        let record = find_record(&app, &id)?;
        if record.path.is_empty() || !Path::new(&record.path).exists() {
            return Err("that capture is no longer on disk".to_string());
        }
        let img = image::open(&record.path)
            .map_err(|e| format!("could not reopen {}: {e}", record.name))?
            .to_rgba8();
        copy_rgba_to_clipboard(&app, &img)
    })
    .await
}

#[tauri::command]
fn reveal_capture(app: AppHandle, id: String) -> Result<(), String> {
    let record = find_record(&app, &id)?;
    if record.path.is_empty() {
        return Err("that capture was never saved to disk".to_string());
    }
    tauri_plugin_opener::reveal_item_in_dir(&record.path).map_err(|e| e.to_string())
}

#[tauri::command]
fn open_capture(app: AppHandle, id: String) -> Result<(), String> {
    let record = find_record(&app, &id)?;
    if record.path.is_empty() {
        return Err("that capture was never saved to disk".to_string());
    }
    tauri_plugin_opener::open_path(&record.path, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
fn open_save_dir(app: AppHandle) -> Result<(), String> {
    let dir = {
        let state = app.state::<AppState>();
        let settings = lock(&state.settings);
        settings.save_dir.clone()
    };
    fs::create_dir_all(&dir).map_err(|e| format!("could not create {dir}: {e}"))?;
    tauri_plugin_opener::open_path(&dir, None::<&str>).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// commands: enumeration
// ---------------------------------------------------------------------------

#[tauri::command]
async fn list_monitors() -> Result<Vec<MonitorInfo>, String> {
    run_blocking(capture::list_monitors).await
}

#[tauri::command]
async fn list_windows() -> Result<Vec<WindowInfo>, String> {
    run_blocking(capture::list_windows).await
}

// ---------------------------------------------------------------------------
// commands: capture
// ---------------------------------------------------------------------------

#[tauri::command]
async fn capture_fullscreen(app: AppHandle) -> Result<CaptureRecord, String> {
    run_blocking(move || grab(&app, "fullscreen", capture::capture_virtual_desktop)).await
}

#[tauri::command]
async fn capture_monitor(app: AppHandle, id: u32) -> Result<CaptureRecord, String> {
    run_blocking(move || grab(&app, "monitor", || capture::capture_monitor_by_id(id))).await
}

#[tauri::command]
async fn capture_active_window(app: AppHandle) -> Result<CaptureRecord, String> {
    run_blocking(move || grab(&app, "window", capture::capture_focused_window)).await
}

#[tauri::command]
async fn capture_window(app: AppHandle, id: u32) -> Result<CaptureRecord, String> {
    run_blocking(move || grab(&app, "window", || capture::capture_window_by_id(id))).await
}

// ---------------------------------------------------------------------------
// hotkeys
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Region,
    Fullscreen,
    ActiveWindow,
}

/// Fire-and-forget: hotkey and tray handlers have nowhere to return an error to,
/// so failures go out on `capture://error` for the UI to surface.
///
/// This is also what the low-level keyboard hook calls, which is why it may not
/// do any work inline — `spawn_blocking` returns immediately and the hook
/// procedure gets to return within microseconds (M2.6 §1).
pub(crate) fn dispatch(app: &AppHandle, action: Action) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Taking the previous preview off screen is the job of the two functions
        // below — `start_region_blocking` and `grab` — because the UI reaches
        // both of them without coming through here. Doing it a third time on
        // this path would only hide an already-hidden window.
        let result = match action {
            Action::Region => overlay::start_region_blocking(&app),
            Action::Fullscreen => {
                grab(&app, "fullscreen", capture::capture_virtual_desktop).map(|_| ())
            }
            Action::ActiveWindow => {
                grab(&app, "window", capture::capture_focused_window).map(|_| ())
            }
        };
        if let Err(e) = result {
            let _ = app.emit("capture://error", e);
        }
    });
}

/// Re-registers every hotkey from scratch, `RegisterHotKey` first and the
/// low-level hook only for what it refuses (M2.6 §1).
///
/// The plugin is tried first because it is cheaper and the OS does the matching;
/// the hook exists solely for the combos the shell or another capture tool
/// already owns, which `RegisterHotKey` can never take. A binding that neither
/// mechanism can claim is reported and skipped — it must never stop the others
/// from working or abort startup.
fn register_hotkeys(app: &AppHandle, settings: &Settings) {
    let shortcuts = app.global_shortcut();
    if let Err(e) = shortcuts.unregister_all() {
        eprintln!("santi.sharex: could not clear global shortcuts: {e}");
    }
    // Every rebind tears the hook down first, so a combo removed from Settings
    // stops being swallowed immediately.
    hotkeys::uninstall();

    let hk = &settings.hotkeys;
    let bindings = [
        ("region", hk.region.as_str(), Action::Region),
        ("fullscreen", hk.fullscreen.as_str(), Action::Fullscreen),
        ("activeWindow", hk.active_window.as_str(), Action::ActiveWindow),
    ];

    let mut statuses: Vec<HotkeyStatus> = Vec::with_capacity(bindings.len());
    let mut fallback: Vec<(hotkeys::Combo, Action)> = Vec::new();
    // Which rows the hook is about to own, so a failed install can downgrade
    // exactly those instead of guessing.
    let mut hooked_rows: Vec<usize> = Vec::new();

    for (name, accelerator, action) in bindings {
        let accelerator = accelerator.trim();
        if accelerator.is_empty() {
            statuses.push(HotkeyStatus {
                action: name.to_string(),
                accelerator: String::new(),
                mechanism: MECHANISM_NONE.to_string(),
                bound: false,
                error: None,
            });
            continue;
        }

        let plugin = shortcuts.on_shortcut(accelerator, move |app, _shortcut, event| {
            // Without this the handler runs twice, once on press and once on release.
            if event.state == ShortcutState::Pressed {
                dispatch(app, action);
            }
        });

        let plugin_error = match plugin {
            Ok(()) => {
                statuses.push(HotkeyStatus {
                    action: name.to_string(),
                    accelerator: accelerator.to_string(),
                    mechanism: MECHANISM_PLUGIN.to_string(),
                    bound: true,
                    error: None,
                });
                continue;
            }
            Err(e) => e.to_string(),
        };

        if settings.use_low_level_hotkeys {
            if let Some(combo) = hotkeys::parse(accelerator) {
                fallback.push((combo, action));
                hooked_rows.push(statuses.len());
                statuses.push(HotkeyStatus {
                    action: name.to_string(),
                    accelerator: accelerator.to_string(),
                    mechanism: MECHANISM_HOOK.to_string(),
                    bound: true,
                    error: None,
                });
                continue;
            }
        }

        let message = format!(
            "Hotkey \"{accelerator}\" could not be registered — another app may already own it ({plugin_error})."
        );
        eprintln!("santi.sharex: {message}");
        let _ = app.emit("capture://error", message.clone());
        statuses.push(HotkeyStatus {
            action: name.to_string(),
            accelerator: accelerator.to_string(),
            mechanism: MECHANISM_NONE.to_string(),
            bound: false,
            error: Some(message),
        });
    }

    if !fallback.is_empty() {
        if let Err(e) = hotkeys::install(app, fallback) {
            let message = format!("santi.sharex could not install its keyboard hook: {e}");
            eprintln!("santi.sharex: {message}");
            let _ = app.emit("capture://error", message.clone());
            for row in hooked_rows {
                statuses[row].mechanism = MECHANISM_NONE.to_string();
                statuses[row].bound = false;
                statuses[row].error = Some(message.clone());
            }
        }
    }

    hotkeys::set_status(statuses);
    let _ = app.emit("hotkeys://status", hotkeys::status());
}

/// Which mechanism ended up owning each hotkey, for Settings to show.
#[tauri::command]
fn get_hotkey_status() -> Vec<HotkeyStatus> {
    hotkeys::status()
}

// ---------------------------------------------------------------------------
// tray
// ---------------------------------------------------------------------------

/// Builds the tray, or explains why it could not.
///
/// A tray icon with no image is not a cosmetic problem: `Shell_NotifyIcon` gives
/// it a blank slot in the notification area that the user cannot see or find, and
/// since M2.6 §2 santi.sharex launches with no window. An invisible tray plus a hidden
/// window is an app reachable only through Task Manager, so a missing icon is
/// reported as a failure and `setup` shows the window instead.
fn build_tray(app: &AppHandle) -> Result<(), String> {
    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or("this build has no icon to put in the tray")?;
    build_tray_inner(app, icon).map_err(|e| e.to_string())
}

fn build_tray_inner(app: &AppHandle, icon: tauri::image::Image<'_>) -> tauri::Result<()> {
    let region = MenuItem::with_id(app, "tray:region", "Capture region", true, None::<&str>)?;
    let fullscreen = MenuItem::with_id(
        app,
        "tray:fullscreen",
        "Capture fullscreen",
        true,
        None::<&str>,
    )?;
    let active = MenuItem::with_id(
        app,
        "tray:window",
        "Capture active window",
        true,
        None::<&str>,
    )?;
    let separator = PredefinedMenuItem::separator(app)?;
    let show = MenuItem::with_id(app, "tray:show", "Show santi.sharex", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "tray:quit", "Quit", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[&region, &fullscreen, &active, &separator, &show, &quit],
    )?;

    let builder = TrayIconBuilder::with_id("santi-sharex")
        .icon(icon)
        .tooltip("santi.sharex")
        .menu(&menu)
        // Left click restores the window, so the menu belongs on right click only.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().0.as_str() {
            "tray:region" => dispatch(app, Action::Region),
            "tray:fullscreen" => dispatch(app, Action::Fullscreen),
            "tray:window" => dispatch(app, Action::ActiveWindow),
            "tray:show" => show_main(app),
            "tray:quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });

    builder.build(app)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// entrypoint
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        // `--autostarted` is passed only by the launch-at-login registration, so
        // a boot-time start can be told apart from the user opening the app. It
        // is what keeps an autostarted instance in the tray even for someone who
        // turned `start_hidden` off.
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostarted"]),
        ))
        .setup(|app| {
            let handle = app.handle().clone();

            // First, and before anything reads either file: `app_data_dir()` is
            // derived from the bundle identifier, which the rename changed, so
            // the settings and the history the user already has live in the old
            // identifier's directory until this brings them across. `AppState`
            // below is the first read, and a read that lost this race would
            // hand back defaults and then persist them.
            migrate::adopt_legacy_app_data(&handle);

            let state = AppState::new(&handle);
            let settings = lock(&state.settings).clone();
            app.manage(state);
            app.manage(CaptureFlags::default());
            app.manage(overlay::OverlayState::default());
            app.manage(preview::PreviewState::default());
            app.manage(scroll::ScrollState::default());

            // santi.sharex starts to tray, so the tray icon is the *only* way in. A
            // tray that failed to build would leave the user with no icon and
            // no window — an app that can only be reached through Task Manager.
            // The window is shown instead, whatever `start_hidden` says.
            let tray_ok = match build_tray(&handle) {
                Ok(()) => true,
                Err(e) => {
                    report_startup_error(
                        &handle,
                        format!(
                            "santi.sharex could not create its tray icon ({e}), so it is showing its window instead. Closing the window will now quit."
                        ),
                    );
                    false
                }
            };
            TRAY_ALIVE.store(tray_ok, Ordering::SeqCst);

            register_hotkeys(&handle, &settings);

            // M2.6 §2: santi.sharex starts to tray. `main` is declared
            // `"visible": false` in tauri.conf.json — showing it and hiding it
            // a frame later reads as a flicker — so this is the only place it
            // comes up at launch, and only when the user asked it to or the
            // tray is missing. The window still exists either way: the stores
            // and the `capture://new` listeners live in it.
            // A launch-at-login start always goes to the tray, whatever
            // `start_hidden` says — having santi.sharex throw a window in your face
            // every time you log in is the fastest way to get it uninstalled.
            let autostarted = std::env::args().any(|a| a == "--autostarted");
            if (!settings.start_hidden && !autostarted) || !tray_ok {
                show_main(&handle);
            }

            // Converge the real registration on the setting (see `sync_autostart`).
            sync_autostart(&handle, settings.launch_at_login);

            // And only then take down the registration the old build left
            // behind: it is keyed by app name, so the rename orphaned it
            // pointing at an exe that is no longer installed. Runs after the
            // sync above so the new entry is already in place, and only ever
            // touches a value that still names the old executable.
            migrate::remove_legacy_autostart();

            // Off the main thread and after a beat, so building the overlay
            // never delays launch. From here on the region hotkey only has to
            // arm and show it (M2.5 §1).
            overlay::prewarm(&handle);

            Ok(())
        })
        .on_window_event(|window, event| match event {
            // santi.sharex lives in the tray: closing the shell hides it, only the
            // tray's Quit actually exits.
            WindowEvent::CloseRequested { api, .. } if window.label() == MAIN_LABEL => {
                api.prevent_close();
                if TRAY_ALIVE.load(Ordering::SeqCst) {
                    let _ = window.hide();
                } else {
                    // No tray to live in, so the main window is the app's only
                    // affordance and closing it has to actually quit.
                    //
                    // Letting the close through is *not* enough: Tauri only
                    // exits when its window map empties, and the pre-warmed
                    // overlay is a live window that is hidden and
                    // `skip_taskbar`. Destroying main alone would leave a
                    // process with no tray, no window and no taskbar entry —
                    // still holding the keyboard hook and still taking captures
                    // on it — reachable only through Task Manager, which is the
                    // exact state this fallback exists to prevent. Quit the way
                    // tray Quit does instead, so `RunEvent::Exit` still runs and
                    // the hook comes down with the process.
                    window.app_handle().exit(0);
                }
            }
            // The overlay is hidden, not destroyed, on every normal exit path
            // now — so this only fires when it goes away some other way (Alt+F4
            // while shown, a webview crash, the unresponsive-overlay teardown).
            // Still the safety net that stops the main window staying hidden
            // forever; the next capture rebuilds the overlay on demand.
            WindowEvent::Destroyed if window.label() == overlay::OVERLAY_LABEL => {
                restore_main_after_capture(window.app_handle());
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            get_history,
            delete_capture,
            clear_history,
            copy_capture,
            reveal_capture,
            open_capture,
            open_save_dir,
            list_monitors,
            list_windows,
            capture_fullscreen,
            capture_monitor,
            capture_active_window,
            capture_window,
            get_hotkey_status,
            overlay::start_region_capture,
            overlay::overlay_ready,
            overlay::get_freeze_frame,
            overlay::finish_region_capture,
            overlay::finish_region_capture_annotated,
            overlay::cancel_region_capture,
            editor::open_editor,
            editor::get_capture,
            editor::get_capture_image,
            editor::save_edit,
            editor::copy_edit,
            editor::close_editor,
            preview::hide_capture_preview,
            preview::get_preview_record,
            ocr::ocr_capture,
            scroll::start_scroll_capture,
            scroll::cancel_scroll_capture,
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application");

    // `build` + `run(callback)` rather than `run(context)` so shutdown has a
    // seam: a `WH_KEYBOARD_LL` hook left installed while the process tears down
    // is a hook whose procedure can be called after its code is gone (M2.6 §1,
    // "uninstalled on shutdown").
    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            hotkeys::uninstall();
            // The last rung of the preview's every-path-ends-hidden ladder
            // (M2.9 §3). It is always-on-top, borderless and unfocusable, so a
            // teardown that stalls with it still up is a sticker the user
            // cannot click away.
            preview::hide(handle);
        }
    });
}
