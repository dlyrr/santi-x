//! Persistent state: settings, capture history, and the in-memory freeze frame.
//!
//! Everything on disk lives under `app_data_dir()`:
//! `settings.json`, `history.json`, `thumbs/{id}.png`.

use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeMap;
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

/// Every upload destination M3 implements, by id (M3 §6). Mirrored by
/// `DESTINATIONS` in `src/lib/types.ts`, and parsed back into
/// `upload::DestinationKind` on the way to a transfer.
pub const DESTINATIONS: [&str; 3] = ["imgur", "custom", "ftp"];

/// [`Settings::destination`] when nothing is set up — the default, and the state
/// in which no capture can leave the machine however the other switches are set
/// (M3 §1).
pub const DESTINATION_NONE: &str = "none";

/// `destination` is a plain `String` on the wire for the same reason `theme` is,
/// and carries the same risk: a hand-edited or newer-build value naming a
/// destination this build cannot dispatch. Unknown ids normalise to
/// [`DESTINATION_NONE`] rather than to some destination — "I did not recognise
/// that, so nothing uploads" is the only safe reading (M3 §1).
pub fn normalize_destination(kind: &str) -> String {
    if DESTINATIONS.contains(&kind) {
        kind.to_string()
    } else {
        DESTINATION_NONE.to_string()
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
    /// M3 §1. **Upload every capture, without being asked.**
    ///
    /// Names its default like the rest, and the default is `false`. That is the
    /// single most important line in this file: a bare `#[serde(default)]`
    /// would give the same answer today, but this field must never be able to
    /// arrive `true` from a `settings.json` that predates it, from a container
    /// default that someone changes later, or from a `Settings::default()` that
    /// gets rewritten in a hurry. A screen capture tool that uploads without
    /// being asked is a data-exfiltration tool with a friendly icon.
    ///
    /// Even when it is on, [`Settings::destination`] defaults to
    /// [`DESTINATION_NONE`], so there is nowhere for a capture to go until the
    /// user sets a destination up themselves.
    #[serde(default = "default_auto_upload")]
    pub auto_upload: bool,
    /// M3 §6. Put the returned link on the clipboard. Defaults to *true* — it is
    /// the point of uploading — so it names its default.
    #[serde(default = "default_true")]
    pub copy_url_after_upload: bool,
    /// M3 §6. Which destination an upload goes to: `"none"` (the default),
    /// `"imgur"`, `"custom"` or `"ftp"`. Normalised on the way in and on the way
    /// out by [`normalize_destination`].
    #[serde(default = "default_destination")]
    pub destination: String,
    /// M3 §4/§5. **Non-secret** per-destination configuration, and only that:
    /// passwords and API keys live in Windows Credential Manager
    /// (`upload::secrets`), never here. Imgur has no non-secret configuration at
    /// all — its client ID is a secret — so it has no struct.
    ///
    /// Both are the *stored* form. `upload::custom` and `upload::ftp` re-validate
    /// them into their own ready-to-send types on the way out, because
    /// `settings.json` is a text file a user can edit.
    #[serde(default)]
    pub custom_uploader: CustomUploaderSettings,
    #[serde(default)]
    pub ftp: FtpSettings,
    /// M4 §1. An explicit ffmpeg to use. Empty — the default — means "resolve
    /// one", which is `PATH`, then scoop, then WinGet, then Program Files.
    /// santi.sharex never downloads one, so this is the escape hatch for a build
    /// that lives somewhere else.
    #[serde(default)]
    pub ffmpeg_path: String,
    /// M4 §6. Seven fields, all with named defaults, for the same reason M5 §4's
    /// three have them: a bare `#[serde(default)]` would hand every existing
    /// `settings.json` `0 fps`, an empty format string and no stop hotkey — a
    /// feature that arrives broken on every machine that already has one, and
    /// invisible until someone tries it.
    #[serde(default = "default_record_fps")]
    pub record_fps: u32,
    /// `"mp4"` | `"gif"`. A plain `String` on the wire like `theme` and
    /// `destination`, normalised by [`normalize_record_format`] so an
    /// unrecognised value cannot reach the encoder.
    #[serde(default = "default_record_format")]
    pub record_format: String,
    /// `h264_nvenc` instead of libx264. Off by default and deliberately so
    /// (M4 §3): NVENC quality at low bitrates is worse, and "my recordings look
    /// soft" is a much harder problem to diagnose than "encoding is slow".
    #[serde(default = "default_record_hw_encode")]
    pub record_hw_encode: bool,
    /// GIF gets its own rate and width ceiling: a 4K 30 fps GIF is a
    /// several-hundred-megabyte file nobody wants.
    #[serde(default = "default_record_gif_fps")]
    pub record_gif_fps: u32,
    #[serde(default = "default_record_gif_max_width")]
    pub record_gif_max_width: u32,
    /// The global stop hotkey (M4 §4). The HUD cannot take focus, so it cannot
    /// have a shortcut of its own — this is the one that works while the user is
    /// in whatever they are recording.
    #[serde(default = "default_record_stop_hotkey")]
    pub record_stop_hotkey: String,
    pub hotkeys: Hotkeys,
}

/// Every recording format M4 implements. Mirrored by `RECORD_FORMATS` in
/// `src/lib/types.ts`, and parsed into `record::pipeline::Format` on the way to
/// an encoder.
pub const RECORD_FORMATS: [&str; 2] = ["mp4", "gif"];

/// Same contract as [`normalize_theme`] and [`normalize_destination`]: an
/// unrecognised value in a hand-edited file must not reach the code that acts on
/// it. Unknown formats become `"mp4"` — the one that plays everywhere.
pub fn normalize_record_format(format: &str) -> String {
    let lowered = format.trim().to_ascii_lowercase();
    if RECORD_FORMATS.contains(&lowered.as_str()) {
        lowered
    } else {
        RECORD_FORMATS[0].to_string()
    }
}

/// An imported ShareX `.sxcu`, minus anything secret (M3 §4).
///
/// Field names mirror the `.sxcu` vocabulary rather than being renamed to
/// something prettier: this is a transcription of a file format, and a user
/// comparing it against the original in Notepad should recognise every line.
/// `upload::custom::import` is what validates it — an unsupported
/// `RequestBodyType` is rejected *there*, with the field named, rather than
/// stored and then mysteriously failing at upload time.
///
/// `BTreeMap` rather than `HashMap` so `settings.json` does not reshuffle its
/// own keys on every write.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CustomUploaderSettings {
    /// `Name` from the file, for the UI to show. Empty means "nothing imported".
    pub name: String,
    /// `"POST"` or `"PUT"`.
    pub request_method: String,
    pub request_url: String,
    /// Header values that looked like credentials are **not** here — their
    /// names are in [`Self::secret_headers`] and their values are in the
    /// credential store (M3 §2, §4).
    pub headers: BTreeMap<String, String>,
    /// `Parameters`, or `Arguments` in older files.
    pub parameters: BTreeMap<String, String>,
    /// `"MultipartFormData"` or `"Binary"`.
    pub body: String,
    pub file_form_name: String,
    /// Response templates: `$json:path.to.field$` / `$response$`.
    pub url: String,
    pub thumbnail_url: String,
    pub deletion_url: String,
    /// Header names whose values are held in Windows Credential Manager under
    /// `santi.sharex/custom/<header-name>`. The list of names is not itself a
    /// secret — it is what lets Settings show a "Configured" indicator, and what
    /// tells the uploader which headers to fetch before a request.
    pub secret_headers: Vec<String>,
}

/// Plain FTP and explicit FTPS (M3 §5). **No SFTP**: it needs an SSH stack, and
/// half-implementing it would be worse than saying so — hence
/// [`FTP_SECURITY_SFTP`], which exists so the picker can show it as unavailable
/// and so a settings file naming it produces a sentence rather than a puzzle.
///
/// The password is a secret and is *not* in here — it lives under
/// `santi.sharex/ftp/password`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FtpSettings {
    pub host: String,
    /// The IANA control port unless the user says otherwise. Named default: a
    /// bare `#[serde(default)]` would hand an existing file port 0.
    #[serde(default = "default_ftp_port")]
    pub port: u16,
    pub username: String,
    /// Directory on the server, e.g. `/public_html/shots`. Empty means wherever
    /// the login lands.
    pub remote_dir: String,
    /// Passive mode — the default, and the only one that works from behind a
    /// consumer router without port forwarding. Named default for the same
    /// reason as `port`.
    #[serde(default = "default_true")]
    pub passive: bool,
    /// One of [`FTP_SECURITY_PLAIN`], [`FTP_SECURITY_EXPLICIT_TLS`],
    /// [`FTP_SECURITY_SFTP`]. A plain `String` rather than an enum for the same
    /// reason `theme` is: an unrecognised value must not make the whole file
    /// fail to parse and take every other setting down with it.
    #[serde(default = "default_ftp_security")]
    pub security: String,
    /// Public address the remote directory is served at, so the copied link
    /// points at the web host rather than at an `ftp://` path.
    pub url_prefix: String,
}

/// The FTP transport values. Mirrored in `upload::ftp`, which is the only thing
/// that acts on them, and in the frontend's picker.
pub const FTP_SECURITY_PLAIN: &str = "plain";
pub const FTP_SECURITY_EXPLICIT_TLS: &str = "explicitTls";
pub const FTP_SECURITY_SFTP: &str = "sftp";

impl Default for FtpSettings {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: default_ftp_port(),
            username: String::new(),
            remote_dir: String::new(),
            passive: true,
            security: default_ftp_security(),
            url_prefix: String::new(),
        }
    }
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

/// M3 §1, spelled out rather than inferred. See [`Settings::auto_upload`].
fn default_auto_upload() -> bool {
    false
}

fn default_destination() -> String {
    DESTINATION_NONE.to_string()
}

fn default_ftp_port() -> u16 {
    21
}

fn default_record_fps() -> u32 {
    30
}

fn default_record_format() -> String {
    RECORD_FORMATS[0].to_string()
}

/// M4 §3, spelled out rather than inferred: libx264 is the default encoder, and
/// hardware encoding is something the user turns on knowing what it costs.
fn default_record_hw_encode() -> bool {
    false
}

fn default_record_gif_fps() -> u32 {
    15
}

fn default_record_gif_max_width() -> u32 {
    800
}

/// One past the three capture hotkeys, which are `CmdOrCtrl+Shift+1..3`.
fn default_record_stop_hotkey() -> String {
    "CmdOrCtrl+Shift+4".to_string()
}

/// Plain FTP is what an FTP account is unless the server was set up for TLS, so
/// it is the value a config that never said gets. The UI is where the "this
/// sends your password in clear text" sentence belongs (M3 §5) — a default that
/// silently failed to connect everywhere would not be safer, just broken.
fn default_ftp_security() -> String {
    FTP_SECURITY_PLAIN.to_string()
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
            auto_upload: default_auto_upload(),
            copy_url_after_upload: true,
            destination: default_destination(),
            custom_uploader: CustomUploaderSettings::default(),
            ftp: FtpSettings::default(),
            ffmpeg_path: String::new(),
            record_fps: default_record_fps(),
            record_format: default_record_format(),
            record_hw_encode: default_record_hw_encode(),
            record_gif_fps: default_record_gif_fps(),
            record_gif_max_width: default_record_gif_max_width(),
            record_stop_hotkey: default_record_stop_hotkey(),
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
    /// "region" | "fullscreen" | "window" | "monitor" | "edit" | "scroll" |
    /// "recording"
    pub kind: String,
    pub created_at: i64,
    pub size_bytes: u64,
    pub saved: bool,
    pub copied: bool,
    /// M3 §6. Where this capture was uploaded to, once it has been. `None` for
    /// every record that has not — which is every record that already exists on
    /// disk, and stays that way unless the user asks for an upload.
    ///
    /// `#[serde(default)]`, and the *only* two fields in this struct that carry
    /// it, because these are the two M3 added to a file the user already has.
    ///
    /// Be precise about what that attribute buys, because it is easy to trust it
    /// for more than it does: serde already treats an `Option<T>` field as
    /// implicitly optional, so removing the attribute would change nothing and
    /// `tests::an_m5_history_file_still_loads_without_the_upload_fields` would
    /// still pass — that was checked by mutation, not assumed. The attribute is
    /// documentation and insurance against the field type changing; the *test*
    /// pins the behaviour, which is the thing the user's 82 records depend on,
    /// and the only edit that can break it — making either field non-`Option`
    /// without a default — fails to compile against that same test.
    #[serde(default)]
    pub url: Option<String>,
    /// The destination's "delete this upload" link, when it hands one back
    /// (Imgur does; a `.sxcu` may). Not every destination has one, so `None` is
    /// a normal state for an uploaded record.
    #[serde(default)]
    pub deletion_url: Option<String>,
    /// M4 §5. How long a recording runs, as the *file* plays it: frames divided
    /// by the frame rate, not wall clock, because dropped frames make a clip
    /// shorter than the time it took to make.
    ///
    /// `None` for every record that is not a recording — which is all 82 of the
    /// ones already on this machine's disk, none of which carry the key at all.
    /// Same `#[serde(default)]` reasoning as `url` and `deletion_url` above, and
    /// the same test standing behind it:
    /// `tests::an_m3_history_file_still_loads_without_the_duration`.
    #[serde(default)]
    pub duration_ms: Option<u32>,
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

/// `app_data_dir()/tmp`, created if absent. M4's GIF intermediate and its
/// palette live here rather than in `save_dir`: they are scaffolding, they are
/// large, and a user watching their captures folder should never see them.
/// `record::pipeline` deletes both on every exit path, including a cancel.
pub fn temp_dir(app: &AppHandle) -> PathBuf {
    let dir = app_data(app).join("tmp");
    let _ = fs::create_dir_all(&dir);
    dir
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
    // An unrecognised destination becomes "none", so a settings file this build
    // cannot make sense of uploads nowhere rather than somewhere (M3 §1).
    settings.destination = normalize_destination(&settings.destination);
    // And an unrecognised recording format becomes the one that plays everywhere
    // (M4 §6), for the same reason.
    settings.record_format = normalize_record_format(&settings.record_format);
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
    resolve_filename_ext(pattern, kind, dir, "png")
}

/// [`resolve_filename`] with the extension named, for M4's `.mp4` and `.gif`.
/// The de-duplication counter goes before the extension either way, so
/// `recording (2).mp4` and never `recording.mp4 (2)`.
pub fn resolve_filename_ext(pattern: &str, kind: &str, dir: &Path, ext: &str) -> PathBuf {
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

    let mut candidate = dir.join(format!("{stem}.{ext}"));
    let mut n: u32 = 2;
    while candidate.exists() {
        candidate = dir.join(format!("{stem} ({n}).{ext}"));
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
        assert_ne!(s.record_fps, 0);
        assert_ne!(s.record_gif_fps, 0);
        assert_ne!(s.record_gif_max_width, 0);
    }

    /// M4 §6 added seven fields to the same struct, and six of them are broken
    /// at zero: 0 fps is not a recording, an empty format string is not a
    /// container, and an empty stop hotkey is a recording that can only be
    /// stopped from the HUD and the tray. (`ffmpegPath` is the one field whose
    /// empty value is meaningful — it means "resolve one".)
    #[test]
    fn an_existing_settings_file_gains_the_recording_defaults_not_zeroes() {
        let s: Settings = serde_json::from_str(M2_11_SETTINGS).expect("M2.11 settings must parse");

        assert_eq!(s.record_fps, 30);
        assert_eq!(s.record_format, "mp4");
        assert!(!s.record_hw_encode, "hardware encoding is opt-in (M4 §3)");
        assert_eq!(s.record_gif_fps, 15);
        assert_eq!(s.record_gif_max_width, 800);
        assert_eq!(s.record_stop_hotkey, "CmdOrCtrl+Shift+4");
        assert!(
            s.ffmpeg_path.is_empty(),
            "an empty ffmpeg path means auto-resolve, and that is the default"
        );

        // And nothing the user had configured moved.
        assert_eq!(s.theme, "sharex");
        assert_eq!(s.hotkeys.region, "Shift+Super+S");
    }

    #[test]
    fn an_unknown_recording_format_normalizes_to_mp4() {
        assert_eq!(normalize_record_format("gif"), "gif");
        assert_eq!(normalize_record_format("mp4"), "mp4");
        // Case-folded rather than rejected: "MP4" is not a mystery.
        assert_eq!(normalize_record_format("GIF"), "gif");
        assert_eq!(normalize_record_format("webm"), "mp4");
        assert_eq!(normalize_record_format(""), "mp4");
    }

    /// The extension is the only thing that changes: the pattern, the sanitising
    /// and the ` (2)` de-duplication are shared with every screenshot.
    #[test]
    fn a_recording_is_named_by_the_same_pattern_with_its_own_extension() {
        let dir = std::env::temp_dir();
        let mp4 = resolve_filename_ext("{kind}_fixed", "recording", &dir, "mp4");
        assert_eq!(
            mp4.file_name().unwrap().to_string_lossy(),
            "recording_fixed.mp4"
        );

        let gif = resolve_filename_ext("{kind}_fixed", "recording", &dir, "gif");
        assert_eq!(
            gif.file_name().unwrap().to_string_lossy(),
            "recording_fixed.gif"
        );

        // And the screenshot path is unchanged.
        let png = resolve_filename("{kind}_fixed", "region", &dir);
        assert_eq!(png.file_name().unwrap().to_string_lossy(), "region_fixed.png");
    }

    /// M3 §1, guarded from three directions at once. This is the test that has
    /// to fail loudly if `auto_upload` ever acquires a `true` default by
    /// accident — through the container default, through `Settings::default()`,
    /// or through a settings file written before the field existed.
    #[test]
    fn auto_upload_is_off_by_default_and_stays_off_for_an_existing_settings_file() {
        assert!(!Settings::default().auto_upload);
        assert!(!default_auto_upload());

        let from_empty: Settings = serde_json::from_str("{}").expect("{} must parse");
        assert!(!from_empty.auto_upload);

        let existing: Settings =
            serde_json::from_str(M2_11_SETTINGS).expect("M2.11 settings must parse");
        assert!(
            !existing.auto_upload,
            "an existing settings.json must not be able to acquire auto-upload"
        );
        // And with it off there is still nowhere for a capture to go.
        assert_eq!(existing.destination, DESTINATION_NONE);
        assert!(existing.ftp.host.is_empty());
        assert!(existing.custom_uploader.request_url.is_empty());

        // And the rest of M3's settings arrive at their documented values
        // rather than at zero/empty — a port of 0 and an active-mode transfer
        // would be an FTP destination that arrives broken on every machine that
        // already has a settings file.
        assert!(existing.copy_url_after_upload);
        assert_eq!(existing.ftp.port, 21);
        assert!(existing.ftp.passive);
        assert_eq!(existing.ftp.security, FTP_SECURITY_PLAIN);
    }

    /// A destination this build cannot dispatch must not be dispatched to. The
    /// unknown value is not preserved, on the theory that "upload nowhere" is
    /// the only safe reading of "I do not know what that is".
    #[test]
    fn an_unknown_destination_normalizes_to_none() {
        assert_eq!(normalize_destination("imgur"), "imgur");
        assert_eq!(normalize_destination("ftp"), "ftp");
        assert_eq!(normalize_destination("custom"), "custom");
        assert_eq!(normalize_destination("s3"), DESTINATION_NONE);
        assert_eq!(normalize_destination(""), DESTINATION_NONE);
        assert_eq!(normalize_destination("IMGUR"), DESTINATION_NONE);
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

    /// M3 §6 added `url` and `deletionUrl` to a struct that has 82 real records
    /// on this machine's disk, none of which carry either key. A `CaptureRecord`
    /// without `#[serde(default)]` on both would make `serde_json::from_str`
    /// fail on the *whole array* — `load_history` swallows that into
    /// `unwrap_or_default()`, so the failure mode is not an error message, it is
    /// an empty gallery and 82 records overwritten with `[]` by the next
    /// capture. This test is the thing standing between the user and that.
    #[test]
    fn an_m5_history_file_still_loads_without_the_upload_fields() {
        // Shaped exactly like the file on disk: no `url`, no `deletionUrl`,
        // several kinds, and one record that was never saved to disk.
        let raw = r#"[
          {"id":"1785835580740-13","name":"region_2026-08-04_04-26-20.png",
           "path":"C:\\Users\\santi\\Pictures\\Nimbus\\region_2026-08-04_04-26-20.png",
           "thumb":"C:\\thumbs\\1785835580740-13.png","width":316,"height":72,
           "kind":"region","createdAt":1785835580747,"sizeBytes":5821,
           "saved":true,"copied":true},
          {"id":"1785835580741-14","name":"scroll_2026-08-04_04-26-21.png",
           "path":"C:\\Users\\santi\\Pictures\\Nimbus\\scroll_2026-08-04_04-26-21.png",
           "thumb":"C:\\thumbs\\1785835580741-14.png","width":1200,"height":8400,
           "kind":"scroll","createdAt":1785835580748,"sizeBytes":1048576,
           "saved":true,"copied":false},
          {"id":"1785835580742-15","name":"fullscreen_2026-08-04_04-26-22.png",
           "path":"","thumb":"C:\\thumbs\\1785835580742-15.png",
           "width":3840,"height":2160,"kind":"fullscreen",
           "createdAt":1785835580749,"sizeBytes":0,"saved":false,"copied":true}
        ]"#;

        let records: Vec<CaptureRecord> =
            serde_json::from_str(raw).expect("an M5 history.json must still parse");

        assert_eq!(records.len(), 3, "every record must survive");
        for record in &records {
            assert!(
                record.url.is_none(),
                "a record that was never uploaded must load with url: None"
            );
            assert!(record.deletion_url.is_none());
            // And nothing else moved.
            assert!(!record.id.is_empty() && !record.name.is_empty());
        }
        assert_eq!(records[1].kind, "scroll");
        assert_eq!(records[1].height, 8400);
        assert!(!records[2].saved && records[2].path.is_empty());
    }

    /// M4 §5 added a *third* field to the same 82 records, and the failure mode
    /// is unchanged and still the worst one in the app: without a default,
    /// `serde_json::from_str` fails on the whole array, `load_history` swallows
    /// that into `unwrap_or_default()`, and the user's gallery is empty and then
    /// overwritten with `[]` by the next capture. This is the test standing
    /// between them and that.
    #[test]
    fn an_m3_history_file_still_loads_without_the_duration() {
        // The real shape on disk: no `durationMs`, and — for the older records —
        // no `url` or `deletionUrl` either.
        let raw = r#"[
          {"id":"1785835580740-13","name":"region_2026-08-04_04-26-20.png",
           "path":"C:\\Users\\santi\\Pictures\\Nimbus\\region_2026-08-04_04-26-20.png",
           "thumb":"C:\\thumbs\\1785835580740-13.png","width":316,"height":72,
           "kind":"region","createdAt":1785835580747,"sizeBytes":5821,
           "saved":true,"copied":true},
          {"id":"1785835580741-14","name":"region_2026-08-04_04-26-21.png",
           "path":"C:\\Users\\santi\\Pictures\\Nimbus\\region_2026-08-04_04-26-21.png",
           "thumb":"C:\\thumbs\\1785835580741-14.png","width":800,"height":600,
           "kind":"region","createdAt":1785835580748,"sizeBytes":9001,
           "saved":true,"copied":false,
           "url":"https://i.imgur.com/abc123.png","deletionUrl":null}
        ]"#;

        let records: Vec<CaptureRecord> =
            serde_json::from_str(raw).expect("an M3 history.json must still parse");

        assert_eq!(records.len(), 2, "every record must survive");
        for record in &records {
            assert!(
                record.duration_ms.is_none(),
                "a screenshot has no duration, and a record that predates the field must load as None"
            );
        }
        assert_eq!(records[1].url.as_deref(), Some("https://i.imgur.com/abc123.png"));
    }

    /// And the other direction: a recording round-trips its duration and its
    /// kind, so History can label it after a restart without re-probing the
    /// file.
    #[test]
    fn a_recording_round_trips_its_duration() {
        let record = CaptureRecord {
            id: "1785835580740-99".into(),
            name: "recording_2026-08-04_20-00-00.mp4".into(),
            path: r"C:\shots\recording_2026-08-04_20-00-00.mp4".into(),
            thumb: r"C:\thumbs\1785835580740-99.png".into(),
            width: 1280,
            height: 720,
            kind: "recording".into(),
            created_at: 1785835580747,
            size_bytes: 4_194_304,
            saved: true,
            copied: false,
            url: None,
            deletion_url: None,
            duration_ms: Some(12_500),
        };

        let json = serde_json::to_string(&record).expect("must serialize");
        assert!(json.contains("\"durationMs\":12500"), "camelCase on the wire");

        let back: CaptureRecord = serde_json::from_str(&json).expect("must round trip");
        assert_eq!(back.duration_ms, Some(12_500));
        assert_eq!(back.kind, "recording");
    }

    /// The other half of the round trip: a record that *has* been uploaded
    /// serialises both fields and reads them back, so History can show and
    /// re-copy the link after a restart.
    #[test]
    fn an_uploaded_record_round_trips_its_link() {
        let record = CaptureRecord {
            id: "1785835580740-13".into(),
            name: "region.png".into(),
            path: r"C:\shots\region.png".into(),
            thumb: r"C:\thumbs\1785835580740-13.png".into(),
            width: 316,
            height: 72,
            kind: "region".into(),
            created_at: 1785835580747,
            size_bytes: 5821,
            saved: true,
            copied: true,
            url: Some("https://i.imgur.com/abc123.png".into()),
            deletion_url: Some("https://imgur.com/delete/xyz".into()),
            duration_ms: None,
        };

        let json = serde_json::to_string(&record).expect("must serialize");
        assert!(json.contains("\"url\""), "the link must be persisted");
        assert!(json.contains("\"deletionUrl\""));

        let back: CaptureRecord = serde_json::from_str(&json).expect("must round trip");
        assert_eq!(back.url.as_deref(), Some("https://i.imgur.com/abc123.png"));
        assert_eq!(
            back.deletion_url.as_deref(),
            Some("https://imgur.com/delete/xyz")
        );
    }
}
