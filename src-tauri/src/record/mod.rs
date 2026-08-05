//! Screen recording (M4).
//!
//! `ffmpeg.rs` finds the binary, `pipeline.rs` turns frames into a file, and this
//! module owns everything around them: the source pickers' rects, the session
//! that can only be one at a time, the HUD window, the three stop paths, and the
//! finished file's trip through the same output pipeline every capture takes.
//!
//! # Stopping (M4 §4)
//!
//! **A recording that cannot be stopped is the worst outcome in this milestone**,
//! so stopping is designed backwards from that. All three paths —
//!
//! - the HUD's Stop button, through [`stop_recording`]
//! - the global stop hotkey, through `lib.rs`'s `Action::StopRecording`
//! - the tray's *Stop recording* item, through the same function
//!
//! — end in [`request_stop`], which does exactly one thing: set a mode on the
//! session's [`Control`] and notify a condvar. No window, no IPC, no event loop,
//! no child process. That is what makes them independent: the hotkey works when
//! the HUD's webview has crashed, the tray works when the hotkey could not be
//! registered, and the HUD works when the tray failed to build. Every one of them
//! is a mutex lock and a store.
//!
//! The session thread wakes on that flag and tears the capture down *first* —
//! recorder stopped, queue closed — before it starts negotiating with ffmpeg,
//! and every step of that negotiation has a deadline that ends in a kill. So a
//! wedged encoder can cost the tail of a recording but can never cost the
//! ability to stop one. Asking twice says "now": once the first stop has had a
//! moment to work, a second one shortens every remaining deadline to seconds —
//! but never to zero, because a user tapping a hotkey twice to be sure must not
//! thereby kill ffmpeg before it wrote the MP4's trailer.
//!
//! The teardown that follows is unconditional, and so is the one in
//! [`spawn_session`] that hides the HUD and frees the slot: it runs even if the
//! pipeline *panics*, because a panicking session thread would otherwise leave
//! an always-on-top window on screen over a slot that refuses every later
//! recording.

pub mod ffmpeg;
pub mod pipeline;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::menu::MenuItem;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, Wry,
};

use crate::store::{self, AppState, CaptureRecord};
use crate::{lock, run_blocking};
use ffmpeg::Ffmpeg;
use pipeline::{Control, Format, Job, Mode, Outcome, Progress, Rect};

/// The HUD's window label, and the `?w=` the webview switches on.
pub const HUD_LABEL: &str = "recorder";

/// `CaptureRecord::kind` for a recording (M4 §5).
pub const RECORD_KIND: &str = "recording";

/// `HotkeyStatus::action` for the global stop hotkey. Shared with `lib.rs`'s
/// binding table so the name the HUD looks the hotkey up by cannot drift from
/// the name it was registered under.
pub const STOP_HOTKEY_ACTION: &str = "recordStop";

/// Everything the HUD renders, and the only event it needs. Emitted on start,
/// every [`STATUS_INTERVAL`] while a recording is live, and once more with
/// `active: false` when it ends — however it ends.
pub const STATUS_EVENT: &str = "record://status";

const STATUS_INTERVAL: Duration = Duration::from_millis(250);

/// Logical size of the HUD and its inset from the work area's corner.
const HUD_WIDTH: f64 = 340.0;
const HUD_HEIGHT: f64 = 96.0;
const HUD_INSET: f64 = 24.0;

/// Paid once, the first time a recording needs the window: the webview has just
/// been created and has not attached its listener yet. Same reasoning as the
/// capture preview's warm-up (M2.9 §3).
const HUD_WARMUP: Duration = Duration::from_millis(300);

// ---------------------------------------------------------------------------
// the wire types
// ---------------------------------------------------------------------------

/// What to record. Tagged, so the frontend sends
/// `{ "type": "region", "x": …, "y": …, "width": …, "height": … }`.
///
/// **Region coordinates are absolute xcap physical screen pixels** — the same
/// space `WindowRect` and the freeze frame's origin live in, *not* overlay-local
/// pixels. A caller working from the region overlay adds the freeze frame's
/// origin before sending.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum RecordSource {
    Region {
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
    Window {
        id: u32,
    },
    Monitor {
        id: u32,
    },
}

impl RecordSource {
    fn id(&self) -> &'static str {
        match self {
            RecordSource::Region { .. } => "region",
            RecordSource::Window { .. } => "window",
            RecordSource::Monitor { .. } => "monitor",
        }
    }
}

/// M4 §3's `RecordSpec`, with everything but the source optional: a caller that
/// sends only `{ source }` gets exactly what Settings says, which is what the
/// Tools page does. An explicit value overrides for this recording only and is
/// never written back.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordSpec {
    pub source: RecordSource,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub fps: Option<u32>,
    #[serde(default)]
    pub capture_cursor: Option<bool>,
    #[serde(default)]
    pub hw_encode: Option<bool>,
}

/// The HUD's whole model.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordStatus {
    pub active: bool,
    pub id: String,
    /// `"idle"` | `"starting"` | `"recording"` | `"finishing"` | `"encoding"` |
    /// `"done"`.
    pub phase: String,
    /// Wall clock since the first frame. The finished file may be shorter — see
    /// `dropped`.
    pub elapsed_ms: u64,
    pub frames: u64,
    /// Frames the encoder could not be handed. Not the same as frames the pacer
    /// skipped because the screen updates faster than the recording rate — those
    /// are downsampling, not loss.
    pub dropped: u64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// `"mp4"` | `"gif"`.
    pub format: String,
    /// `"region"` | `"window"` | `"monitor"`.
    pub source: String,
    pub cursor: bool,
    pub hardware: bool,
    /// The accelerator the HUD shows beside Stop, so the one control that works
    /// when the HUD does not is discoverable.
    pub stop_hotkey: String,
    pub output_name: String,
}

impl RecordStatus {
    pub fn idle() -> Self {
        Self {
            active: false,
            id: String::new(),
            phase: "idle".into(),
            elapsed_ms: 0,
            frames: 0,
            dropped: 0,
            width: 0,
            height: 0,
            fps: 0,
            format: String::new(),
            source: String::new(),
            cursor: false,
            hardware: false,
            stop_hotkey: String::new(),
            output_name: String::new(),
        }
    }
}

/// What a finished recording produced, for the caller that started it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordOutcome {
    /// `None` for a cancel, which by definition kept nothing.
    pub record: Option<CaptureRecord>,
    pub frames: u64,
    pub dropped: u64,
    pub duration_ms: u32,
    pub cancelled: bool,
    /// ffmpeg had to be killed, so the file may be short. Said out loud rather
    /// than hidden behind a successful-looking record.
    pub truncated: bool,
}

// ---------------------------------------------------------------------------
// session state
// ---------------------------------------------------------------------------

/// The recording in flight, if there is one. Managed state, registered in
/// `lib.rs`.
#[derive(Default)]
pub struct RecordState {
    session: Mutex<Option<Arc<Session>>>,
}

/// One recording. Shared by the session thread, the status ticker and all three
/// stop paths.
struct Session {
    id: String,
    control: Control,
    progress: Progress,
    started: Instant,
    /// Set the moment the session thread is finished with the slot, so the
    /// ticker stops without having to consult the state it may already have been
    /// removed from.
    done: AtomicBool,
    info: Info,
}

/// The constants of a recording — everything the HUD shows that cannot change
/// while it runs.
#[derive(Clone)]
struct Info {
    width: u32,
    height: u32,
    fps: u32,
    format: Format,
    source: &'static str,
    cursor: bool,
    hardware: bool,
    stop_hotkey: String,
    output_name: String,
}

impl Session {
    fn status(&self) -> RecordStatus {
        let phase = self.progress.phase.load(Ordering::SeqCst);
        RecordStatus {
            active: !self.done.load(Ordering::SeqCst),
            id: self.id.clone(),
            phase: pipeline::phase_name(phase).to_string(),
            elapsed_ms: self.started.elapsed().as_millis() as u64,
            frames: self.progress.frames.load(Ordering::Relaxed),
            dropped: self.progress.dropped.load(Ordering::Relaxed),
            width: self.info.width,
            height: self.info.height,
            fps: self.info.fps,
            format: self.info.format.id().to_string(),
            source: self.info.source.to_string(),
            cursor: self.info.cursor,
            hardware: self.info.hardware,
            stop_hotkey: self.info.stop_hotkey.clone(),
            output_name: self.info.output_name.clone(),
        }
    }
}

fn current(app: &AppHandle) -> Option<Arc<Session>> {
    let state = app.state::<RecordState>();
    let session = lock(&state.session).clone();
    session
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

/// Whether recording can be offered at all, and what to say when it cannot
/// (M4 §1). Structured: which binary or encoder is missing, where santi.sharex
/// looked, and the exact commands that fix it.
#[tauri::command]
pub async fn record_availability(app: AppHandle) -> ffmpeg::Availability {
    run_blocking(move || {
        let setting = {
            let state = app.state::<AppState>();
            let settings = lock(&state.settings);
            settings.ffmpeg_path.clone()
        };
        Ok(ffmpeg::availability(&setting, &ffmpeg::required_encoders()))
    })
    .await
    .unwrap_or_else(|e| {
        let mut unavailable = ffmpeg::availability("", &ffmpeg::required_encoders());
        unavailable.available = false;
        eprintln!("santi.sharex: could not probe ffmpeg: {e}");
        unavailable
    })
}

/// Live status, for a HUD that mounted after the first emit — the same catch-up
/// path the capture preview has.
#[tauri::command]
pub fn recording_status(app: AppHandle) -> RecordStatus {
    current(&app)
        .map(|s| s.status())
        .unwrap_or_else(RecordStatus::idle)
}

/// Start recording. Returns as soon as frames are flowing; the recording itself
/// runs on its own thread and reports on [`STATUS_EVENT`].
#[tauri::command]
pub async fn start_recording(app: AppHandle, spec: RecordSpec) -> Result<RecordStatus, String> {
    run_blocking(move || start(&app, spec)).await
}

/// Finish and keep. The HUD's Stop button; also what the hotkey and the tray
/// reach, by way of [`request_stop`].
#[tauri::command]
pub fn stop_recording(app: AppHandle) -> Result<(), String> {
    if current(&app).is_none() {
        return Err("nothing is recording".into());
    }
    request_stop(&app);
    Ok(())
}

/// Finish and discard.
#[tauri::command]
pub fn cancel_recording(app: AppHandle) -> Result<(), String> {
    if current(&app).is_none() {
        return Err("nothing is recording".into());
    }
    request_cancel(&app);
    Ok(())
}

/// The one function all three stop paths call (see the module header).
///
/// Deliberately infallible and deliberately trivial: a lock, a store, a notify.
/// It never touches a window, never waits on a process and never returns an
/// error, because the two callers that are not a command — the keyboard hook's
/// dispatcher and the tray menu — have nowhere to put one and everything to lose
/// by blocking.
pub(crate) fn request_stop(app: &AppHandle) {
    if let Some(session) = current(app) {
        session.control.request(Mode::Stopping);
    }
}

pub(crate) fn request_cancel(app: &AppHandle) {
    if let Some(session) = current(app) {
        session.control.request(Mode::Cancelling);
    }
}

// ---------------------------------------------------------------------------
// starting
// ---------------------------------------------------------------------------

fn start(app: &AppHandle, spec: RecordSpec) -> Result<RecordStatus, String> {
    let settings = {
        let state = app.state::<AppState>();
        let guard = lock(&state.settings);
        guard.clone()
    };

    let format = Format::parse(
        spec.format
            .as_deref()
            .unwrap_or(settings.record_format.as_str()),
    );
    let ffmpeg: Ffmpeg = ffmpeg::require(&settings.ffmpeg_path, format.encoders())?;

    let hw_encode = spec.hw_encode.unwrap_or(settings.record_hw_encode)
        && format == Format::Mp4
        && ffmpeg.has(ffmpeg::ENCODER_NVENC);
    let fps = spec
        .fps
        .unwrap_or(match format {
            // A GIF at 30 fps is twice the file for motion nobody perceives as
            // smoother in a palette-limited format (M4 §3).
            Format::Gif => settings.record_gif_fps,
            Format::Mp4 => settings.record_fps,
        })
        .clamp(pipeline::FPS_MIN, pipeline::FPS_MAX);

    let (monitor_id, monitor_origin, rect) = resolve_source(&spec.source)?;
    let rect = rect.to_even();
    if rect.width == 0 || rect.height == 0 {
        return Err("that area is too small to record".into());
    }

    let dir = PathBuf::from(&settings.save_dir);
    fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let output = store::resolve_filename_ext(
        &settings.filename_pattern,
        RECORD_KIND,
        &dir,
        format.ext(),
    );
    let output_name = output
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("{RECORD_KIND}.{}", format.ext()));

    let job = Job {
        ffmpeg,
        monitor_id,
        monitor_origin,
        rect,
        fps,
        format,
        capture_cursor: spec.capture_cursor.unwrap_or(settings.capture_cursor),
        hw_encode,
        gif_max_width: settings.record_gif_max_width,
        output,
        work_dir: store::temp_dir(app),
    };

    let session = Arc::new(Session {
        id: store::next_id(),
        control: Control::default(),
        progress: Progress::default(),
        started: Instant::now(),
        done: AtomicBool::new(false),
        info: Info {
            width: rect.width,
            height: rect.height,
            fps,
            format,
            source: spec.source.id(),
            cursor: job.capture_cursor,
            hardware: hw_encode,
            stop_hotkey: advertisable_stop_hotkey(),
            output_name,
        },
    });

    // Claimed before anything can fail slowly, so a second start is refused
    // rather than racing this one onto the same monitor.
    {
        let state = app.state::<RecordState>();
        let mut slot = lock(&state.session);
        if slot.is_some() {
            return Err("a recording is already running".into());
        }
        *slot = Some(session.clone());
    }

    // The capture preview is always-on-top, so a preview left over from the last
    // screenshot would be recorded into this clip (M2.9 §3).
    crate::hide_preview_for_capture(app);

    // Up before the first frame, and placed clear of the rect: a HUD recorded
    // into the video is a bug (M4 §4).
    if let Err(e) = show_hud(app, rect) {
        eprintln!("santi.sharex: could not show the recording HUD: {e}");
        let _ = app.emit(
            "capture://error",
            format!(
                "The recording HUD could not be shown ({e}) — {} still stops the recording.",
                session.info.stop_hotkey
            ),
        );
    }
    set_tray_stop_enabled(true);

    let status = session.status();
    let _ = app.emit(STATUS_EVENT, status.clone());

    spawn_ticker(app.clone(), session.clone());
    spawn_session(app.clone(), session, job);

    Ok(status)
}

/// The stop accelerator the HUD is allowed to print: the configured one, and
/// only when something actually claimed it.
///
/// `Settings.record_stop_hotkey` is what the user *asked* for, which is not the
/// same question — `RegisterHotKey` hands a combo to exactly one process, and
/// the hook fallback is off unless `use_low_level_hotkeys` is on, so the
/// configured combo can perfectly well belong to another app. Printing it anyway
/// would put a shortcut that does nothing on the one card the user reads when
/// they want the recording to end, and send them pressing it instead of reaching
/// for the tray. An empty string is what the HUD turns into "No stop hotkey —
/// use the tray", which is the true sentence.
fn advertisable_stop_hotkey() -> String {
    crate::hotkeys::status()
        .into_iter()
        .find(|status| status.action == STOP_HOTKEY_ACTION && status.bound)
        .map(|status| status.accelerator)
        .unwrap_or_default()
}

/// Emits [`STATUS_EVENT`] on its own timer, so the HUD's clock keeps moving
/// while the session thread is blocked in the encoder.
fn spawn_ticker(app: AppHandle, session: Arc<Session>) {
    std::thread::spawn(move || {
        while !session.done.load(Ordering::SeqCst) {
            std::thread::sleep(STATUS_INTERVAL);
            if session.done.load(Ordering::SeqCst) {
                break;
            }
            let _ = app.emit(STATUS_EVENT, session.status());
        }
    });
}

fn spawn_session(app: AppHandle, session: Arc<Session>, job: Job) {
    std::thread::spawn(move || {
        // Caught rather than allowed to unwind the thread, because the teardown
        // below is the only thing that hides the HUD, frees the session slot and
        // re-greys the tray item. A panic that skipped it would leave an
        // always-on-top, unfocusable, borderless card on screen with no way to
        // close it, a slot that refuses every later recording with "a recording
        // is already running", and a tray Stop that stops nothing — the exact
        // shape of the worst outcome in this milestone, arrived at sideways.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pipeline::run(&job, &session.control, &session.progress)
        }))
        .unwrap_or_else(|_| {
            Err("the recording stopped unexpectedly (internal error)".to_string())
        });

        // The user-visible teardown happens before anything else: the slot is
        // freed, the HUD comes down and the HUD is told it is over. None of it
        // waits on the file work below.
        session.done.store(true, Ordering::SeqCst);
        {
            let state = app.state::<RecordState>();
            let mut slot = lock(&state.session);
            if slot.as_ref().map(|s| s.id == session.id).unwrap_or(false) {
                *slot = None;
            }
        }
        hide_hud(&app);
        set_tray_stop_enabled(false);
        let _ = app.emit(STATUS_EVENT, RecordStatus::idle());

        match result {
            Ok(outcome) => {
                if let Err(e) = publish(&app, &job, &outcome) {
                    eprintln!("santi.sharex: {e}");
                    let _ = app.emit("capture://error", e);
                }
            }
            Err(e) => {
                let message = format!("Recording failed: {e}");
                eprintln!("santi.sharex: {message}");
                let _ = app.emit("capture://error", message);
            }
        }
    });
}

// ---------------------------------------------------------------------------
// picking the rect (M4 §2)
// ---------------------------------------------------------------------------

/// Resolves a source into the monitor that will be duplicated, that monitor's
/// origin, and the rect to crop out of it — **once**. A window that moves or
/// resizes mid-recording keeps this rect (M4 §2); rawvideo has no way to say a
/// frame changed shape, so the alternative is not "track it", it is "restart the
/// encoder", which is a different feature.
fn resolve_source(source: &RecordSource) -> Result<(u32, (i32, i32), Rect), String> {
    let wanted = match *source {
        RecordSource::Region {
            x,
            y,
            width,
            height,
        } => Rect {
            x,
            y,
            width,
            height,
        },
        RecordSource::Window { id } => {
            let windows =
                xcap::Window::all().map_err(|e| format!("enumerating windows failed: {e}"))?;
            let window = windows
                .iter()
                .find(|w| w.id().map(|got| got == id).unwrap_or(false))
                .ok_or_else(|| format!("no window with id {id}"))?;
            if window.is_minimized().unwrap_or(false) {
                return Err("that window is minimized — restore it first".into());
            }
            Rect {
                x: window.x().map_err(|e| format!("window x() failed: {e}"))?,
                y: window.y().map_err(|e| format!("window y() failed: {e}"))?,
                width: window
                    .width()
                    .map_err(|e| format!("window width() failed: {e}"))?,
                height: window
                    .height()
                    .map_err(|e| format!("window height() failed: {e}"))?,
            }
        }
        RecordSource::Monitor { id } => {
            let (mid, origin, rect) = monitor_rect_by_id(id)?;
            return Ok((mid, origin, rect));
        }
    };

    if wanted.width == 0 || wanted.height == 0 {
        return Err("that area is too small to record".into());
    }

    // Duplication is per monitor, so the rect has to belong to one. The centre
    // decides, and the rect is then clipped to it: a window straddling two
    // displays records the part on the display it is mostly on, which is at
    // least a coherent answer.
    let centre_x = wanted.x.saturating_add(wanted.width as i32 / 2);
    let centre_y = wanted.y.saturating_add(wanted.height as i32 / 2);
    let (monitor_id, origin, bounds) = monitor_rect_at(centre_x, centre_y)?;

    let left = wanted.x.max(bounds.x);
    let top = wanted.y.max(bounds.y);
    let right = wanted
        .x
        .saturating_add(wanted.width as i32)
        .min(bounds.x.saturating_add(bounds.width as i32));
    let bottom = wanted
        .y
        .saturating_add(wanted.height as i32)
        .min(bounds.y.saturating_add(bounds.height as i32));
    if right <= left || bottom <= top {
        return Err("that area is not on any display".into());
    }

    Ok((
        monitor_id,
        origin,
        Rect {
            x: left,
            y: top,
            width: (right - left) as u32,
            height: (bottom - top) as u32,
        },
    ))
}

fn monitor_geometry(monitor: &xcap::Monitor) -> Result<(u32, (i32, i32), Rect), String> {
    let id = monitor
        .id()
        .map_err(|e| format!("monitor id() failed: {e}"))?;
    let x = monitor.x().map_err(|e| format!("monitor x() failed: {e}"))?;
    let y = monitor.y().map_err(|e| format!("monitor y() failed: {e}"))?;
    let width = monitor
        .width()
        .map_err(|e| format!("monitor width() failed: {e}"))?;
    let height = monitor
        .height()
        .map_err(|e| format!("monitor height() failed: {e}"))?;
    Ok((
        id,
        (x, y),
        Rect {
            x,
            y,
            width,
            height,
        },
    ))
}

fn monitor_rect_by_id(id: u32) -> Result<(u32, (i32, i32), Rect), String> {
    let monitors = xcap::Monitor::all().map_err(|e| format!("enumerating monitors failed: {e}"))?;
    let monitor = monitors
        .iter()
        .find(|m| m.id().map(|got| got == id).unwrap_or(false))
        .ok_or_else(|| format!("no monitor with id {id}"))?;
    monitor_geometry(monitor)
}

fn monitor_rect_at(x: i32, y: i32) -> Result<(u32, (i32, i32), Rect), String> {
    if let Ok(monitor) = xcap::Monitor::from_point(x, y) {
        return monitor_geometry(&monitor);
    }
    let monitors = xcap::Monitor::all().map_err(|e| format!("enumerating monitors failed: {e}"))?;
    let monitor = monitors
        .iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or_else(|| monitors.first())
        .ok_or_else(|| "no monitors found".to_string())?;
    monitor_geometry(monitor)
}

// ---------------------------------------------------------------------------
// output (M4 §5)
// ---------------------------------------------------------------------------

/// The finished file's trip through the pipeline every other capture takes:
/// named by the filename pattern, written to `save_dir`, thumbnailed, added to
/// history as `kind: "recording"` and emitted on `capture://new`.
///
/// Three things it deliberately does **not** do. It does not copy to the
/// clipboard — Windows has no clipboard format for a video file that anything
/// useful would paste. It does not open the editor, which is an image editor.
/// And it does not auto-upload: M3 §1's implicit-upload path was written for
/// screenshots, a recording can be two orders of magnitude larger, and M4 §5
/// says only that uploading is *allowed where the destination accepts it* —
/// which is a decision the user makes per recording, not one this function makes
/// for them.
fn publish(app: &AppHandle, job: &Job, outcome: &Outcome) -> Result<(), String> {
    if outcome.cancelled {
        let _ = app.emit(
            "record://cancelled",
            RecordOutcome {
                record: None,
                frames: outcome.frames,
                dropped: outcome.dropped,
                duration_ms: 0,
                cancelled: true,
                truncated: false,
            },
        );
        return Ok(());
    }

    let settings = {
        let state = app.state::<AppState>();
        let guard = lock(&state.settings);
        guard.clone()
    };

    let size_bytes = fs::metadata(&job.output).map(|m| m.len()).unwrap_or(0);
    if size_bytes == 0 {
        return Err("the recording produced an empty file".into());
    }

    let id = store::next_id();
    let name = job
        .output
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("{RECORD_KIND}.{}", job.format.ext()));

    // A frame from the middle of the clip (M4 §5) — the first frame of a
    // recording is usually the moment before anything happened, which makes for
    // a thumbnail that says nothing about what was recorded.
    let (thumb, width, height) = match thumbnail(app, job, outcome, &id) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("santi.sharex: could not thumbnail the recording: {e}");
            (String::new(), outcome.width, outcome.height)
        }
    };

    let record = CaptureRecord {
        id,
        name,
        path: job.output.to_string_lossy().into_owned(),
        thumb,
        width,
        height,
        kind: RECORD_KIND.to_string(),
        created_at: chrono::Utc::now().timestamp_millis(),
        size_bytes,
        // A recording has nowhere to live but disk: there is no in-memory form
        // of it to fall back on, so `save_to_disk` does not apply here.
        saved: true,
        copied: false,
        url: None,
        deletion_url: None,
        duration_ms: Some(outcome.duration_ms),
    };

    {
        let state = app.state::<AppState>();
        let mut history = lock(&state.history);
        history.insert(0, record.clone());
        store::persist_history(app, &mut history)?;
    }

    let _ = app.emit("capture://new", record.clone());
    let _ = app.emit(
        "record://finished",
        RecordOutcome {
            record: Some(record),
            frames: outcome.frames,
            dropped: outcome.dropped,
            duration_ms: outcome.duration_ms,
            cancelled: false,
            truncated: outcome.truncated,
        },
    );

    // Said out loud rather than left to be noticed: a recording that ended on
    // its own looks exactly like one the user stopped, and the file is short for
    // a reason they had no part in.
    if let Some(why) = &outcome.source_lost {
        let _ = app.emit(
            "capture://error",
            format!(
                "The display being recorded went away, so the recording ends there ({why}). What was captured up to that point was kept."
            ),
        );
    } else if outcome.truncated {
        let _ = app.emit(
            "capture://error",
            "ffmpeg had to be stopped, so the recording may be missing its last moments."
                .to_string(),
        );
    }

    if settings.open_folder_after {
        let _ = tauri_plugin_opener::reveal_item_in_dir(&job.output);
    }
    Ok(())
}

/// Writes the thumbnail and returns its path plus the clip's true dimensions —
/// which for a scaled GIF are only knowable by looking at the result.
fn thumbnail(
    app: &AppHandle,
    job: &Job,
    outcome: &Outcome,
    id: &str,
) -> Result<(String, u32, u32), String> {
    let middle = outcome.duration_ms / 2;
    let frame = pipeline::extract_frame(&job.ffmpeg, &job.output, middle)
        // A clip too short to seek into still has a first frame.
        .or_else(|_| pipeline::extract_frame(&job.ffmpeg, &job.output, 0))?;

    let (width, height) = frame.dimensions();
    let path = store::thumb_path(app, id);
    let bytes = crate::capture::encode_png_fast(&crate::capture::thumbnail(&frame))?;
    fs::write(&path, &bytes).map_err(|e| format!("could not write the thumbnail: {e}"))?;
    Ok((path.to_string_lossy().into_owned(), width, height))
}

// ---------------------------------------------------------------------------
// the HUD window (M4 §4)
// ---------------------------------------------------------------------------

/// Built once and reused, never destroyed, and **never focusable** — the same
/// discipline the capture preview follows (M2.9 §3). It appears while the user
/// is doing something else, over the thing they are recording, so taking focus
/// would swallow their keystrokes into a window that has nothing to type into.
/// `focused(false)` only covers the first show; `focusable(false)` is the
/// permanent `WS_EX_NOACTIVATE` that covers every show after a hide. Nothing
/// here calls `set_focus`.
///
/// Because it cannot take focus it cannot have a keyboard shortcut of its own,
/// which is exactly why the global stop hotkey exists and why the HUD shows it.
fn show_hud(app: &AppHandle, avoid: Rect) -> Result<(), String> {
    let (window, built) = match app.get_webview_window(HUD_LABEL) {
        Some(window) => (window, false),
        None => (build_hud(app)?, true),
    };
    if built {
        std::thread::sleep(HUD_WARMUP);
    }
    // Only when the placement search could not find anywhere clear — see
    // `exclude_from_capture` for why this is deliberately not applied every
    // time. Set on each show, and cleared on each show that does not need it,
    // because this window outlives every recording it is used for.
    let overlaps = place_hud(app, &window, avoid)?;
    exclude_from_capture(&window, overlaps);
    // `show`, never `set_focus`.
    window
        .show()
        .map_err(|e| format!("could not show the recording HUD: {e}"))
}

/// Ask Windows to leave the HUD out of every screen capture — including the
/// desktop duplication this app's own recorder is reading (M4 §4: "A HUD
/// recorded into the video is a bug").
///
/// `WDA_EXCLUDEFROMCAPTURE` is documented for exactly this case: *"One use for
/// this affinity is for windows that show video recording controls, so that the
/// controls are not included in the capture."* It is the only thing that can
/// keep the HUD out of a **full-monitor** recording on a single-display machine,
/// where every corner of the screen is inside the recorded rect and
/// [`place_hud`] has nowhere clear to put it.
///
/// Applied only in that case, and cleared otherwise. That is not timidity, it is
/// blast radius: display affinity changes how the compositor treats the window,
/// this one is a borderless WebView2, and the ordinary recordings — a window, a
/// region, anything on a second display — are already kept out of the shot by
/// position, which costs nothing and cannot render wrong.
///
/// Best effort either way: it needs Windows 10 2004 or later and some
/// remote-session stacks refuse it. Failing costs the HUD appearing in a
/// full-screen recording, so it is a note on stderr and nothing more.
#[cfg(windows)]
fn exclude_from_capture(window: &WebviewWindow, exclude: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE,
    };

    let hwnd = match window.hwnd() {
        Ok(hwnd) => hwnd,
        Err(e) => {
            eprintln!("santi.sharex: no window handle for the recording HUD: {e}");
            return;
        }
    };
    let affinity = if exclude {
        WDA_EXCLUDEFROMCAPTURE
    } else {
        WDA_NONE
    };
    if let Err(e) = unsafe { SetWindowDisplayAffinity(hwnd, affinity) } {
        eprintln!(
            "santi.sharex: could not set the recording HUD's display affinity ({e}); it may appear in a recording that covers the whole display"
        );
    }
}

#[cfg(not(windows))]
fn exclude_from_capture(_window: &WebviewWindow, _exclude: bool) {}

/// Every exit path ends here — the session thread's teardown, a failed start,
/// and `RunEvent::Exit`. An always-on-top unfocusable window left on screen is a
/// sticker the user cannot click away.
pub(crate) fn hide_hud(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(HUD_LABEL) {
        let _ = window.hide();
    }
}

fn build_hud(app: &AppHandle) -> Result<WebviewWindow, String> {
    WebviewWindowBuilder::new(
        app,
        HUD_LABEL,
        // The bare query, *not* "index.html?w=recorder" — see the same note in
        // preview.rs, overlay.rs and editor.rs.
        WebviewUrl::App(format!("?w={HUD_LABEL}").into()),
    )
    .title("santi.sharex recorder")
    .inner_size(HUD_WIDTH, HUD_HEIGHT)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .shadow(false)
    .visible(false)
    .focused(false)
    .focusable(false)
    .build()
    .map_err(|e| format!("could not create the recording HUD: {e}"))
}

/// One display the HUD could stand on: its work area in physical screen pixels,
/// and the scale factor the HUD has to be sized at if it lands there.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Spot {
    area: Rect,
    scale: f64,
}

/// The four corners of `area`, inset, for a HUD of `width` × `height`. In
/// preference order: bottom-right first, because that is where a status card
/// belongs and where the taskbar's own notification area lives.
fn corners(area: &Rect, width: u32, height: u32, inset: i32) -> [(i32, i32); 4] {
    let left = area.x + inset;
    let top = area.y + inset;
    // `max` so a work area narrower than the HUD does not put "right" left of
    // "left" and fling the card off the display.
    let right = (area.x + area.width as i32 - width as i32 - inset).max(left);
    let bottom = (area.y + area.height as i32 - height as i32 - inset).max(top);
    [(right, bottom), (right, top), (left, bottom), (left, top)]
}

/// Where the HUD goes, given the displays available and the rect being recorded.
/// `spots[0]` is the display the recording is on; the rest are the others, in
/// enumeration order.
///
/// Bottom-right of the recorded display first, then its other corners, **then
/// the other displays**. That last step is the one that matters: recording a
/// whole monitor puts every corner of it inside the recorded rect, which is the
/// common case (M4 §2 offers "Display" as a source) and the one a
/// corners-of-this-monitor-only search silently fails. On a two-display machine
/// the HUD simply moves next door.
///
/// If nothing is clear — a full-screen recording on the only display — the
/// bottom-right of the recorded display is used anyway. The HUD being in the
/// shot is bad; a recording with no visible way to stop it is worse. That last
/// case is also the one [`exclude_from_capture`] exists for, and between them
/// the HUD stays out of the video on every machine where Windows lets it.
pub(crate) fn choose_hud_placement(
    spots: &[Spot],
    logical: (f64, f64),
    inset_logical: f64,
    avoid: &Rect,
) -> (i32, i32, u32, u32) {
    let sized = |spot: &Spot| {
        let width = ((logical.0 * spot.scale).round() as u32).max(1);
        let height = ((logical.1 * spot.scale).round() as u32).max(1);
        let inset = (inset_logical * spot.scale).round() as i32;
        (width, height, inset)
    };

    for spot in spots {
        let (width, height, inset) = sized(spot);
        for (x, y) in corners(&spot.area, width, height, inset) {
            if !avoid.intersects(&Rect {
                x,
                y,
                width,
                height,
            }) {
                return (x, y, width, height);
            }
        }
    }

    let fallback = spots.first().copied().unwrap_or(Spot {
        area: *avoid,
        scale: 1.0,
    });
    let (width, height, inset) = sized(&fallback);
    let (x, y) = corners(&fallback.area, width, height, inset)[0];
    (x, y, width, height)
}

/// The displays the HUD may stand on, the one being recorded first.
fn hud_spots(app: &AppHandle, avoid: Rect) -> Vec<Spot> {
    let describe = |monitor: &tauri::Monitor| Spot {
        // The work area, not the monitor bounds: the difference is the taskbar,
        // and the HUD's first choice of corner is the one the taskbar lives in.
        area: {
            let area = monitor.work_area();
            Rect {
                x: area.position.x,
                y: area.position.y,
                width: area.size.width,
                height: area.size.height,
            }
        },
        scale: monitor.scale_factor(),
    };

    let recorded = app
        .monitor_from_point(
            f64::from(avoid.x) + f64::from(avoid.width) / 2.0,
            f64::from(avoid.y) + f64::from(avoid.height) / 2.0,
        )
        .ok()
        .flatten()
        .or_else(|| app.primary_monitor().ok().flatten());

    let mut spots = Vec::new();
    if let Some(monitor) = recorded.as_ref() {
        spots.push(describe(monitor));
    }
    // Everything else, in enumeration order, skipping the one already at the
    // front. Compared by position because `tauri::Monitor` has no id.
    if let Ok(all) = app.available_monitors() {
        for monitor in &all {
            let spot = describe(monitor);
            if spots
                .iter()
                .any(|first| (first.area.x, first.area.y) == (spot.area.x, spot.area.y))
            {
                continue;
            }
            spots.push(spot);
        }
    }
    spots
}

/// Bottom-right of the *monitor*, moved to another corner — or another monitor —
/// if that would put it inside the recorded rect. A HUD recorded into the video
/// is a bug (M4 §4).
///
/// Returns whether the spot it settled on is still inside the recorded rect,
/// i.e. whether there was nowhere clear at all. That is the one case
/// [`exclude_from_capture`] is for.
fn place_hud(app: &AppHandle, window: &WebviewWindow, avoid: Rect) -> Result<bool, String> {
    let spots = hud_spots(app, avoid);
    if spots.is_empty() {
        return Err("no monitor to place the recording HUD on".to_string());
    }
    let (x, y, width, height) =
        choose_hud_placement(&spots, (HUD_WIDTH, HUD_HEIGHT), HUD_INSET, &avoid);

    // Position before size: moving between monitors of different DPI makes
    // Windows resize the window to its own suggestion, so the size has to be the
    // last word (M2.9 §3).
    window
        .set_position(PhysicalPosition::new(x, y))
        .map_err(|e| format!("could not position the recording HUD: {e}"))?;
    window
        .set_size(PhysicalSize::new(width, height))
        .map_err(|e| format!("could not size the recording HUD: {e}"))?;

    Ok(avoid.intersects(&Rect {
        x,
        y,
        width,
        height,
    }))
}

// ---------------------------------------------------------------------------
// the tray's stop item
// ---------------------------------------------------------------------------

/// The tray's *Stop recording* item, kept so it can be greyed out when there is
/// nothing to stop. `lib.rs` hands it over as the tray is built.
static TRAY_STOP: OnceLock<Mutex<Option<MenuItem<Wry>>>> = OnceLock::new();

pub(crate) fn adopt_tray_stop(item: MenuItem<Wry>) {
    let slot = TRAY_STOP.get_or_init(|| Mutex::new(None));
    *lock(slot) = Some(item);
}

fn set_tray_stop_enabled(enabled: bool) {
    let Some(slot) = TRAY_STOP.get() else {
        return;
    };
    if let Some(item) = lock(slot).as_ref() {
        let _ = item.set_enabled(enabled);
    }
}

// ---------------------------------------------------------------------------
// settings plumbing
// ---------------------------------------------------------------------------

/// Called from `save_settings`: a user who browses to a different ffmpeg must
/// not still be running the old one for the rest of the session.
pub(crate) fn settings_changed() {
    ffmpeg::forget();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire shape the frontend has to produce. Tagged and camelCase, with
    /// everything but the source optional so `{ source }` alone is a valid spec.
    #[test]
    fn a_minimal_spec_deserializes_and_defers_to_settings() {
        let spec: RecordSpec =
            serde_json::from_str(r#"{"source":{"type":"monitor","id":7}}"#).expect("must parse");
        assert!(matches!(spec.source, RecordSource::Monitor { id: 7 }));
        assert!(spec.format.is_none());
        assert!(spec.fps.is_none());
        assert!(spec.capture_cursor.is_none());
        assert!(spec.hw_encode.is_none());
    }

    #[test]
    fn a_region_spec_carries_an_absolute_rect() {
        let spec: RecordSpec = serde_json::from_str(
            r#"{"source":{"type":"region","x":-1920,"y":40,"width":800,"height":600},
                "format":"gif","fps":12,"captureCursor":false,"hwEncode":true}"#,
        )
        .expect("must parse");

        match spec.source {
            RecordSource::Region {
                x,
                y,
                width,
                height,
            } => {
                // Negative x is normal: a second monitor left of the primary.
                assert_eq!((x, y, width, height), (-1920, 40, 800, 600));
            }
            other => panic!("wrong source: {other:?}"),
        }
        assert_eq!(spec.format.as_deref(), Some("gif"));
        assert_eq!(spec.fps, Some(12));
        assert_eq!(spec.capture_cursor, Some(false));
        assert_eq!(spec.hw_encode, Some(true));
    }

    // -- what the HUD may claim about stopping ------------------------------

    fn hotkey_row(action: &str, accelerator: &str, bound: bool) -> crate::hotkeys::HotkeyStatus {
        crate::hotkeys::HotkeyStatus {
            action: action.to_string(),
            accelerator: accelerator.to_string(),
            mechanism: if bound { "plugin" } else { "none" }.to_string(),
            bound,
            error: None,
        }
    }

    /// The HUD prints the stop hotkey because it is the one control that works
    /// while the card itself cannot be clicked. Printing one that *nothing
    /// claimed* is therefore the worst possible thing to be wrong about: the
    /// user presses it instead of reaching for the tray, and the recording keeps
    /// going. `RegisterHotKey` gives a combo to exactly one process, so "the
    /// user configured it" and "it is bound" are different facts.
    #[test]
    fn the_hud_only_advertises_a_stop_hotkey_that_is_actually_bound() {
        crate::hotkeys::set_status(vec![
            hotkey_row("region", "CmdOrCtrl+Shift+1", true),
            hotkey_row(STOP_HOTKEY_ACTION, "CmdOrCtrl+Shift+4", true),
        ]);
        assert_eq!(advertisable_stop_hotkey(), "CmdOrCtrl+Shift+4");

        // Another app owns it, and the hook fallback is off or could not parse
        // it. The HUD must say "no stop hotkey", not name one.
        crate::hotkeys::set_status(vec![
            hotkey_row("region", "CmdOrCtrl+Shift+1", true),
            hotkey_row(STOP_HOTKEY_ACTION, "CmdOrCtrl+Shift+4", false),
        ]);
        assert_eq!(advertisable_stop_hotkey(), "");

        // Cleared in Settings: nothing to advertise either.
        crate::hotkeys::set_status(vec![hotkey_row(STOP_HOTKEY_ACTION, "", false)]);
        assert_eq!(advertisable_stop_hotkey(), "");

        crate::hotkeys::set_status(Vec::new());
    }

    // -- where the HUD stands (M4 §4) --------------------------------------

    fn spot(x: i32, y: i32, width: u32, height: u32) -> Spot {
        Spot {
            area: Rect {
                x,
                y,
                width,
                height,
            },
            scale: 1.0,
        }
    }

    /// A 1920×1080 display whose work area stops above a 40px taskbar.
    fn primary() -> Spot {
        spot(0, 0, 1920, 1040)
    }

    const HUD: (f64, f64) = (HUD_WIDTH, HUD_HEIGHT);

    /// The ordinary case: a small region in the top-left, HUD bottom-right.
    #[test]
    fn the_hud_takes_the_bottom_right_corner_when_it_is_free() {
        let avoid = Rect {
            x: 100,
            y: 100,
            width: 640,
            height: 360,
        };
        let (x, y, w, h) = choose_hud_placement(&[primary()], HUD, HUD_INSET, &avoid);
        assert_eq!((w, h), (HUD_WIDTH as u32, HUD_HEIGHT as u32));
        assert_eq!(x, 1920 - w as i32 - HUD_INSET as i32);
        assert_eq!(y, 1040 - h as i32 - HUD_INSET as i32);
    }

    /// A region that covers the bottom-right corner pushes the HUD to another
    /// corner of the same display rather than into the shot.
    #[test]
    fn a_region_over_the_corner_moves_the_hud_off_it() {
        let avoid = Rect {
            x: 1200,
            y: 600,
            width: 700,
            height: 430,
        };
        let (x, y, w, h) = choose_hud_placement(&[primary()], HUD, HUD_INSET, &avoid);
        let placed = Rect {
            x,
            y,
            width: w,
            height: h,
        };
        assert!(
            !avoid.intersects(&placed),
            "the HUD must not overlap the recorded rect: {placed:?}"
        );
    }

    /// **The case a corners-of-this-monitor search silently fails.** Recording a
    /// whole display puts every one of its corners inside the recorded rect, and
    /// "Display" is one of the three sources M4 §2 offers — so this is not an
    /// edge case, it is one of the three buttons. With a second display present
    /// the HUD moves next door.
    #[test]
    fn a_full_monitor_recording_moves_the_hud_to_another_display() {
        let recorded = primary();
        // A second display to the right, ordinary 1920×1080 with its own taskbar.
        let second = spot(1920, 0, 1920, 1040);
        // The whole of the first display, taskbar included — what
        // `RecordSource::Monitor` resolves to.
        let avoid = Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };

        let (x, y, w, h) =
            choose_hud_placement(&[recorded, second], HUD, HUD_INSET, &avoid);
        let placed = Rect {
            x,
            y,
            width: w,
            height: h,
        };
        assert!(
            !avoid.intersects(&placed),
            "a full-display recording must not have the HUD in it: {placed:?}"
        );
        assert!(x >= 1920, "the HUD belongs on the display that is not being recorded");
    }

    /// A display left of the primary has negative coordinates, which is normal
    /// and must not be mistaken for "off screen".
    #[test]
    fn the_second_display_may_be_at_a_negative_origin() {
        let recorded = primary();
        let second = spot(-1920, 0, 1920, 1040);
        let avoid = Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };

        let (x, y, w, h) = choose_hud_placement(&[recorded, second], HUD, HUD_INSET, &avoid);
        assert!(!avoid.intersects(&Rect {
            x,
            y,
            width: w,
            height: h
        }));
        assert!(x < 0);
    }

    /// The HUD is sized for the display it actually lands on, not for the one
    /// being recorded — a 150% laptop panel beside a 100% external is the normal
    /// Windows arrangement, and a HUD sized for the wrong one is either a
    /// postage stamp or clipped.
    #[test]
    fn the_hud_is_sized_for_the_display_it_lands_on() {
        let recorded = primary();
        let mut second = spot(1920, 0, 2560, 1400);
        second.scale = 2.0;
        let avoid = Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };

        let (_, _, w, h) = choose_hud_placement(&[recorded, second], HUD, HUD_INSET, &avoid);
        assert_eq!((w, h), ((HUD_WIDTH * 2.0) as u32, (HUD_HEIGHT * 2.0) as u32));
    }

    /// One display, recording all of it: there is nowhere clear, and the HUD is
    /// shown anyway. A recording with no visible way to stop it is worse than
    /// one with a card in the corner — and `exclude_from_capture` is what keeps
    /// that card out of the pixels.
    #[test]
    fn with_nowhere_clear_the_hud_still_appears() {
        let avoid = Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let (x, y, w, h) = choose_hud_placement(&[primary()], HUD, HUD_INSET, &avoid);
        assert_eq!(x, 1920 - w as i32 - HUD_INSET as i32);
        assert_eq!(y, 1040 - h as i32 - HUD_INSET as i32);
        assert!(w > 0 && h > 0);
    }

    /// A work area smaller than the HUD must not fling it off the display.
    #[test]
    fn a_tiny_work_area_still_places_the_hud_inside_it() {
        let tiny = spot(0, 0, 200, 60);
        let avoid = Rect {
            x: 5000,
            y: 5000,
            width: 100,
            height: 100,
        };
        let (x, y, _, _) = choose_hud_placement(&[tiny], HUD, HUD_INSET, &avoid);
        assert_eq!((x, y), (HUD_INSET as i32, HUD_INSET as i32));
    }

    #[test]
    fn an_idle_status_is_inert() {
        let idle = RecordStatus::idle();
        assert!(!idle.active);
        assert_eq!(idle.phase, "idle");
        assert_eq!(idle.frames, 0);
        let json = serde_json::to_string(&idle).expect("must serialize");
        assert!(json.contains("\"stopHotkey\""), "camelCase on the wire");
        assert!(json.contains("\"elapsedMs\""));
    }
}
