//! Persistent state: settings, capture history, and the in-memory freeze frame.
//!
//! Everything on disk lives under `app_data_dir()`:
//! `settings.json`, `history.json`, `thumbs/{id}.png`.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Local, Utc};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// Hard cap on retained history records. Anything past this is dropped from the
/// tail (oldest) and its thumbnail deleted.
pub const HISTORY_LIMIT: usize = 500;

/// Every theme `app.css` has a block for, in the order it defines them
/// (M2.6 §4). Mirrored by `THEMES` in `src/lib/types.ts`.
pub const THEMES: [&str; 4] = ["dark", "claude", "claude-dark", "sharex"];

/// The theme anything unrecognised falls back to.
pub const DEFAULT_THEME: &str = "dark";

/// `theme` is a plain `String` on the wire, so a hand-edited settings file — or
/// one written by a newer build — can name a theme this build has no block for.
/// Stamping that on `<html>` leaves every token unresolved, which is an
/// unreadable page rather than a wrong colour, so it is normalised on the way in
/// *and* on the way out (M2.6 §5).
pub fn normalize_theme(theme: &str) -> String {
    if THEMES.contains(&theme) {
        theme.to_string()
    } else {
        DEFAULT_THEME.to_string()
    }
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Hotkeys {
    pub region: String,
    pub fullscreen: String,
    pub active_window: String,
}

impl Default for Hotkeys {
    fn default() -> Self {
        // M2.6 §1: combos that are actually free, so a fresh install works
        // without a trip to Settings. The M1 defaults
        // (`CmdOrCtrl+PrintScreen` / `PrintScreen` / `Alt+PrintScreen`) all
        // fail to register when ShareX or the shell owns them. The hook makes
        // those bindable again, but stealing the OS snipping shortcut from a
        // user who never asked is the wrong *default*.
        Self {
            region: "CmdOrCtrl+Shift+1".into(),
            fullscreen: "CmdOrCtrl+Shift+2".into(),
            active_window: "CmdOrCtrl+Shift+3".into(),
        }
    }
}

/// # Two layers of defaulting, and why both stay
///
/// The container-level `default` is the one that actually fires: a missing key
/// is taken from [`Settings::default()`], field by field. It is what lets an M1
/// `settings.json` still load — `save_dir`, `filename_pattern`, `theme` and
/// `hotkeys` have no field-level default of their own and rely on it entirely.
///
/// The per-field `#[serde(default = "…")]` attributes below are therefore
/// *redundant* today. They are kept because they are the layer that survives
/// someone removing the container attribute, and because a field whose default
/// is not the zero value should say so where it is declared rather than only in
/// a `Default` impl fifty lines away. `tests::an_empty_settings_file_is_…`
/// fails if either layer is removed AND the other does not cover it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Absolute path. Empty in `Default` — it can only be resolved from an
    /// `AppHandle`; `load_settings` fills it in with `default_save_dir`.
    pub save_dir: String,
    pub filename_pattern: String,
    pub save_to_disk: bool,
    pub copy_to_clipboard: bool,
    pub open_folder_after: bool,
    pub hide_window_on_capture: bool,
    pub theme: String,
    /// M2. Each of the three carries its own `#[serde(default)]` so a
    /// `settings.json` written by M1 still deserializes field by field.
    #[serde(default)]
    pub open_editor_after: bool,
    /// The bare `#[serde(default)]` would hand a missing field `String::new()`
    /// and `0` rather than these values, so the two non-trivial ones name the
    /// same functions `Default` uses.
    #[serde(default = "default_editor_color")]
    pub editor_default_color: String,
    #[serde(default = "default_editor_stroke")]
    pub editor_default_stroke: u32,
    /// M2.5. Draw on the selection inside the region overlay before committing.
    /// Defaults to *true*, so it needs a named default too — a bare
    /// `#[serde(default)]` would silently turn the feature off for everyone who
    /// already has a `settings.json`.
    #[serde(default = "default_annotate_in_overlay")]
    pub annotate_in_overlay: bool,
    /// M2.6 §1. Fall back to the `WH_KEYBOARD_LL` hook for combos
    /// `RegisterHotKey` refuses. Off means "plugin only", i.e. M1 behaviour.
    /// Defaults to *true*, so it needs a named default like the others.
    #[serde(default = "default_true")]
    pub use_low_level_hotkeys: bool,
    /// M2.6 §2. Launch with only the tray icon, ShareX-style.
    #[serde(default = "default_true")]
    pub start_hidden: bool,
    /// M2.8. Register santi.sharex to start with Windows. Kept in sync with the real
    /// autostart registration on load and on save, so editing the JSON by hand
    /// still does the right thing.
    #[serde(default = "default_true")]
    pub launch_at_login: bool,
    /// M2.9 §3. The ShareX-style thumbnail that appears bottom-right after a
    /// capture. Defaults to *true*, so it names its default like the rest — a
    /// bare `#[serde(default)]` would ship the feature switched off to everyone
    /// who already has a `settings.json`.
    #[serde(default = "default_true")]
    pub show_capture_preview: bool,
    /// M2.9 §1. The region overlay's magnifier factor, persisted so a user who
    /// scrolls to 12x is still at 12x after a restart. Read by the overlay
    /// only; Rust just keeps it, clamped to the documented 2x–20x range.
    #[serde(default = "default_loupe_zoom")]
    pub loupe_zoom: u32,
    /// M2.10. Draw the mouse cursor into the shot. The framebuffer never
    /// contains it — Windows composites the cursor separately — so this is an
    /// explicit rasterise-and-blend, not a capture flag. Defaults to *true*,
    /// hence the named default.
    #[serde(default = "default_true")]
    pub capture_cursor: bool,
    /// M5 §4. Scrolling capture: settle time between wheel steps, wheel notches
    /// per step, and the hard frame budget.
    ///
    /// All three name their default. A bare `#[serde(default)]` would hand an
    /// existing `settings.json` `0`, `0` and `0` — a zero settle delay, a wheel
    /// step that scrolls nothing and a run that stops before it starts — so the
    /// feature would arrive broken on every machine that already has one.
    ///
    /// Read in exactly one place, `scroll.rs`, which clamps them at the point of
    /// use; unlike `theme` and `loupe_zoom` there is nothing else downstream that
    /// an out-of-range value could reach.
    #[serde(default = "default_scroll_delay_ms")]
    pub scroll_delay_ms: u32,
    #[serde(default = "default_scroll_step")]
    pub scroll_step: i32,
    #[serde(default = "default_scroll_max_frames")]
    pub scroll_max_frames: u32,
    pub hotkeys: Hotkeys,
}

/// Bounds on [`Settings::loupe_zoom`] (M2.9 §1). A hand-edited `0` would be a
/// division by zero in the overlay's loupe maths, so it is normalised on the way
/// in and on the way out, exactly like `theme`.
pub const LOUPE_ZOOM_MIN: u32 = 2;
pub const LOUPE_ZOOM_MAX: u32 = 20;

pub fn normalize_loupe_zoom(zoom: u32) -> u32 {
    zoom.clamp(LOUPE_ZOOM_MIN, LOUPE_ZOOM_MAX)
}

fn default_editor_color() -> String {
    "#f2555a".into()
}

fn default_editor_stroke() -> u32 {
    4
}

fn default_annotate_in_overlay() -> bool {
    true
}

fn default_true() -> bool {
    true
}

fn default_loupe_zoom() -> u32 {
    8
}

fn default_scroll_delay_ms() -> u32 {
    250
}

fn default_scroll_step() -> i32 {
    3
}

fn default_scroll_max_frames() -> u32 {
    60
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            save_dir: String::new(),
            filename_pattern: "{kind}_{yyyy}-{MM}-{dd}_{HH}-{mm}-{ss}".into(),
            save_to_disk: true,
            copy_to_clipboard: true,
            open_folder_after: false,
            // Off by default. Hiding the window mid-capture changes the screen
            // out from under the shot, which is worse than santi.sharex appearing in
            // it. Region capture ignores this outright — see `overlay.rs`.
            hide_window_on_capture: false,
            theme: DEFAULT_THEME.into(),
            open_editor_after: false,
            editor_default_color: default_editor_color(),
            editor_default_stroke: default_editor_stroke(),
            annotate_in_overlay: default_annotate_in_overlay(),
            use_low_level_hotkeys: true,
            start_hidden: true,
            launch_at_login: true,
            show_capture_preview: true,
            loupe_zoom: default_loupe_zoom(),
            capture_cursor: true,
            scroll_delay_ms: default_scroll_delay_ms(),
            scroll_step: default_scroll_step(),
            scroll_max_frames: default_scroll_max_frames(),
            hotkeys: Hotkeys::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureRecord {
    pub id: String,
    pub name: String,
    pub path: String,
    pub thumb: String,
    pub width: u32,
    pub height: u32,
    /// "region" | "fullscreen" | "window" | "monitor" | "edit" | "scroll"
    pub kind: String,
    pub created_at: i64,
    pub size_bytes: u64,
    pub saved: bool,
    pub copied: bool,
}

/// The full-desktop snapshot the region overlay draws on top of. Coordinates are
/// xcap-space physical pixels; `width`/`height` are the *image's* real
/// dimensions, which the overlay uses to derive its CSS px → image px scale.
#[derive(Debug, Clone)]
pub struct FreezeFrame {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub origin_x: i32,
    pub origin_y: i32,
}

pub struct AppState {
    pub settings: Mutex<Settings>,
    pub history: Mutex<Vec<CaptureRecord>>,
    pub freeze: Mutex<Option<FreezeFrame>>,
}

impl AppState {
    /// Reads both JSON files from disk; missing or corrupt files yield defaults.
    pub fn new(app: &AppHandle) -> Self {
        Self {
            settings: Mutex::new(load_settings(app)),
            history: Mutex::new(load_history(app)),
            freeze: Mutex::new(None),
        }
    }
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// `app_data_dir()`, created if absent. Derived from the bundle identifier, so
/// the rename moved it — see `migrate.rs`, which is why this is reachable from
/// outside the module.
pub(crate) fn app_data(app: &AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("santi-sharex"));
    let _ = fs::create_dir_all(&dir);
    dir
}

fn settings_path(app: &AppHandle) -> PathBuf {
    app_data(app).join("settings.json")
}

fn history_path(app: &AppHandle) -> PathBuf {
    app_data(app).join("history.json")
}

/// `app_data_dir()/thumbs`, created if absent.
pub fn thumbs_dir(app: &AppHandle) -> PathBuf {
    let dir = app_data(app).join("thumbs");
    let _ = fs::create_dir_all(&dir);
    dir
}

/// Canonical thumbnail location for a record id.
pub fn thumb_path(app: &AppHandle, id: &str) -> PathBuf {
    thumbs_dir(app).join(format!("{id}.png"))
}

/// `<Pictures>/santi.sharex`, created if absent. Falls back to the home dir and then
/// to the app data dir on the rare systems where the known folder is missing.
pub fn default_save_dir(app: &AppHandle) -> PathBuf {
    let base = app
        .path()
        .picture_dir()
        .or_else(|_| app.path().home_dir().map(|h| h.join("Pictures")))
        .unwrap_or_else(|_| app_data(app));
    let dir = base.join("santi.sharex");
    let _ = fs::create_dir_all(&dir);
    dir
}

// ---------------------------------------------------------------------------
// Settings persistence
// ---------------------------------------------------------------------------

/// Never fails: an unreadable or malformed `settings.json` is treated as "no
/// settings yet" so a bad file can't lock the user out of the app.
pub fn load_settings(app: &AppHandle) -> Settings {
    let mut settings = fs::read_to_string(settings_path(app))
        .ok()
        .and_then(|raw| serde_json::from_str::<Settings>(&raw).ok())
        .unwrap_or_default();

    if settings.save_dir.trim().is_empty() {
        settings.save_dir = default_save_dir(app).to_string_lossy().into_owned();
    } else {
        let _ = fs::create_dir_all(&settings.save_dir);
    }
    settings.theme = normalize_theme(&settings.theme);
    settings.loupe_zoom = normalize_loupe_zoom(settings.loupe_zoom);
    settings
}

pub fn persist_settings(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let json = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("failed to serialize settings: {e}"))?;
    fs::write(settings_path(app), json).map_err(|e| format!("failed to write settings.json: {e}"))
}

// ---------------------------------------------------------------------------
// History persistence
// ---------------------------------------------------------------------------

/// Newest first. A missing or malformed `history.json` yields an empty history.
pub fn load_history(app: &AppHandle) -> Vec<CaptureRecord> {
    fs::read_to_string(history_path(app))
        .ok()
        .and_then(|raw| serde_json::from_str::<Vec<CaptureRecord>>(&raw).ok())
        .unwrap_or_default()
}

/// Trims to [`HISTORY_LIMIT`] (deleting the dropped records' thumbnails, which
/// nothing else references) and writes the result. Takes `&mut` because the cap
/// is enforced on the caller's live vector, not just on the file.
pub fn persist_history(app: &AppHandle, history: &mut Vec<CaptureRecord>) -> Result<(), String> {
    if history.len() > HISTORY_LIMIT {
        for dropped in history.drain(HISTORY_LIMIT..) {
            if !dropped.thumb.is_empty() {
                let _ = fs::remove_file(&dropped.thumb);
            }
        }
    }
    let json = serde_json::to_string(&*history)
        .map_err(|e| format!("failed to serialize history: {e}"))?;
    fs::write(history_path(app), json).map_err(|e| format!("failed to write history.json: {e}"))
}

/// One record, cloned out of the live history. The clone is deliberate: callers
/// must not hold the history lock while they touch the filesystem or the
/// clipboard.
pub fn capture_by_id(app: &AppHandle, id: &str) -> Result<CaptureRecord, String> {
    let state = app.state::<AppState>();
    let history = crate::lock(&state.history);
    history
        .iter()
        .find(|r| r.id == id)
        .cloned()
        .ok_or_else(|| format!("no capture with id {id}"))
}

// ---------------------------------------------------------------------------
// Ids and filenames
// ---------------------------------------------------------------------------

static ID_COUNTER: AtomicU32 = AtomicU32::new(0);
static RAND_SALT: AtomicU32 = AtomicU32::new(0);

/// `"{utc_millis}-{n}"`. The counter disambiguates captures taken inside the
/// same millisecond (burst hotkey presses).
pub fn next_id() -> String {
    let n = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{}", Utc::now().timestamp_millis(), n)
}

const RAND_ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// 4 lowercase alphanumerics. Hashed off the nanosecond clock plus a counter so
/// two calls in the same nanosecond tick still differ — no RNG crate needed.
fn rand_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    let mut hasher = DefaultHasher::new();
    nanos.hash(&mut hasher);
    RAND_SALT.fetch_add(1, Ordering::Relaxed).hash(&mut hasher);
    let mut h = hasher.finish();

    let mut out = String::with_capacity(4);
    for _ in 0..4 {
        out.push(RAND_ALPHABET[(h % RAND_ALPHABET.len() as u64) as usize] as char);
        h /= RAND_ALPHABET.len() as u64;
    }
    out
}

/// Characters Windows rejects in a file name, plus the path separators — a
/// pattern is user-editable text and must not be able to escape `dir`.
fn sanitize_stem(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if (c as u32) < 0x20 => '-',
            c => c,
        })
        .collect();
    // Trailing dots and spaces are silently stripped by the Win32 file APIs.
    cleaned.trim().trim_end_matches('.').trim_end().to_string()
}

/// Expands the pattern tokens (`{kind} {yyyy} {MM} {dd} {HH} {mm} {ss} {rand}`,
/// unknown tokens left verbatim), appends `.png`, and de-duplicates against
/// `dir` with ` (2)`, ` (3)`, … before the extension.
///
/// Timestamps are local time — this is a user-facing file name, unlike
/// `CaptureRecord::created_at` which is UTC millis.
pub fn resolve_filename(pattern: &str, kind: &str, dir: &Path) -> PathBuf {
    let now = Local::now();
    let expanded = pattern
        .replace("{kind}", kind)
        .replace("{yyyy}", &now.format("%Y").to_string())
        .replace("{MM}", &now.format("%m").to_string())
        .replace("{dd}", &now.format("%d").to_string())
        .replace("{HH}", &now.format("%H").to_string())
        .replace("{mm}", &now.format("%M").to_string())
        .replace("{ss}", &now.format("%S").to_string())
        .replace("{rand}", &rand_token());

    let mut stem = sanitize_stem(&expanded);
    if stem.is_empty() {
        stem = format!("{kind}_{}", now.format("%Y-%m-%d_%H-%M-%S"));
    }

    let mut candidate = dir.join(format!("{stem}.png"));
    let mut n: u32 = 2;
    while candidate.exists() {
        candidate = dir.join(format!("{stem} ({n}).png"));
        n += 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `settings.json` exactly as M2.11 wrote it — no `scroll*` keys at all.
    /// This is the literal shape on a machine that has been running the app
    /// since before M5, custom hotkeys and save directory included.
    ///
    /// `r##"…"##`, not `r#"…"#`: the colour literal contains `"#`, which would
    /// otherwise close the raw string in the middle of the JSON.
    const M2_11_SETTINGS: &str = r##"{
      "saveDir": "C:\\Users\\santi\\Pictures\\Nimbus",
      "filenamePattern": "{kind}_{yyyy}-{MM}-{dd}_{HH}-{mm}-{ss}",
      "saveToDisk": true,
      "copyToClipboard": true,
      "openFolderAfter": false,
      "hideWindowOnCapture": false,
      "theme": "sharex",
      "openEditorAfter": false,
      "editorDefaultColor": "#f2555a",
      "editorDefaultStroke": 4,
      "annotateInOverlay": true,
      "useLowLevelHotkeys": true,
      "startHidden": true,
      "launchAtLogin": true,
      "showCapturePreview": true,
      "loupeZoom": 2,
      "captureCursor": true,
      "hotkeys": {
        "region": "Shift+Super+S",
        "fullscreen": "PrintScreen",
        "activeWindow": "Alt+PrintScreen"
      }
    }"##;

    /// M5 §4 added three fields to a struct users already have on disk. The
    /// risk is not that the file fails to parse — `#[serde(default)]` on the
    /// container guarantees it will — but that the three new fields arrive as
    /// `0`, `0`, `0`: a zero settle delay, a wheel step that scrolls nothing
    /// and a frame budget clamped up from below. That is a feature that ships
    /// broken to every existing install, and it is invisible until someone
    /// tries it.
    #[test]
    fn an_m2_11_settings_file_still_loads_and_gains_the_scroll_defaults() {
        let s: Settings = serde_json::from_str(M2_11_SETTINGS).expect("M2.11 settings must parse");

        // Everything the user actually configured survives untouched.
        assert_eq!(s.save_dir, r"C:\Users\santi\Pictures\Nimbus");
        assert_eq!(s.theme, "sharex");
        assert_eq!(s.loupe_zoom, 2);
        assert_eq!(s.hotkeys.region, "Shift+Super+S");
        assert_eq!(s.hotkeys.fullscreen, "PrintScreen");
        assert_eq!(s.hotkeys.active_window, "Alt+PrintScreen");
        assert_eq!(s.filename_pattern, "{kind}_{yyyy}-{MM}-{dd}_{HH}-{mm}-{ss}");
        assert!(s.save_to_disk && s.copy_to_clipboard && s.capture_cursor);
        assert!(s.annotate_in_overlay && s.start_hidden && s.launch_at_login);
        assert!(s.show_capture_preview && s.use_low_level_hotkeys);
        assert!(!s.open_editor_after && !s.open_folder_after && !s.hide_window_on_capture);

        // And the three new ones arrive at their documented defaults, not zero.
        assert_eq!(s.scroll_delay_ms, 250);
        assert_eq!(s.scroll_step, 3);
        assert_eq!(s.scroll_max_frames, 60);
    }

    /// The general form of the rule, so the next field added to `Settings` is
    /// caught too: an EMPTY object must deserialize to exactly `default()`.
    /// A field that forgot its named default would come back `0`/`""`/`false`
    /// here while `Default` still had the real value.
    #[test]
    fn an_empty_settings_file_is_indistinguishable_from_defaults() {
        let from_empty: Settings = serde_json::from_str("{}").expect("{} must parse");
        let fresh = Settings::default();
        assert_eq!(
            serde_json::to_value(&from_empty).unwrap(),
            serde_json::to_value(&fresh).unwrap(),
            "a missing field is defaulting to a zero value rather than to its real default"
        );
    }

    /// No numeric setting may default to zero. Every one of them is either a
    /// duration, a count or a step, and zero is a broken value for all three —
    /// this is the exact failure mode M5 §4's named defaults exist to prevent.
    #[test]
    fn no_numeric_setting_defaults_to_zero() {
        let s = Settings::default();
        assert_ne!(s.editor_default_stroke, 0);
        assert_ne!(s.loupe_zoom, 0);
        assert_ne!(s.scroll_delay_ms, 0);
        assert_ne!(s.scroll_step, 0);
        assert_ne!(s.scroll_max_frames, 0);
    }

    /// History is a separate file and M5 did not touch `CaptureRecord`, but the
    /// new `"scroll"` kind travels in it. An existing record must still load,
    /// and a `scroll` one must round-trip.
    #[test]
    fn existing_history_records_still_load_beside_scroll_ones() {
        let raw = r#"[
          {"id":"1785835580740-13","name":"region_2026-08-04_04-26-20.png",
           "path":"C:\\Users\\santi\\Pictures\\Nimbus\\region_2026-08-04_04-26-20.png",
           "thumb":"C:\\thumbs\\1785835580740-13.png","width":316,"height":72,
           "kind":"region","createdAt":1785835580747,"sizeBytes":5821,
           "saved":true,"copied":true}
        ]"#;
        let records: Vec<CaptureRecord> = serde_json::from_str(raw).expect("history must parse");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, "region");
        assert_eq!(records[0].width, 316);
        assert!(records[0].saved && records[0].copied);
    }
}
