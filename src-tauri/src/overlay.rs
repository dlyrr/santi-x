//! Freeze-frame lifecycle and the region-select overlay window.
//!
//! Region capture is a dance because the screen must be frozen *before* the
//! selector is drawn: grab the virtual desktop, write it to the cache as a PNG,
//! then float a borderless window over the desktop that displays it. The user
//! drags on a still image, so nothing moves under the crosshair and the crop is
//! exact.
//!
//! M2.5 changed *when* that window exists. It used to be built per capture and
//! destroyed after, which put a webview creation plus a SvelteKit page load on
//! the critical path of every hotkey press. Now it is built once at startup,
//! hidden, and reused forever; a capture only arms it and shows it.
//!
//! The arming handshake:
//!
//! 1. `start_region_capture` grabs, encodes and enumerates, then emits
//!    `overlay://arm` with an [`ArmPayload`] carrying a monotonic `armId`.
//! 2. It blocks until the overlay answers `overlay_ready(armId)` — i.e. until
//!    the bitmap has decoded and painted. Showing before that flashes an empty
//!    black window across the whole desktop.
//! 3. Only then does it position, size, show and focus the window.
//!
//! A superseded arm (a second hotkey press) can never show a stale frame,
//! because `overlay_ready` only counts for the arm that is currently live.

use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};

use crate::capture::{self, WindowRect};
use crate::store::{AppState, CaptureRecord};
use crate::{finalize, lock, run_blocking};

/// The frozen desktop currently backing an open overlay. It lives in `store` next
/// to `AppState`, which owns the `Mutex` holding it.
pub use crate::store::FreezeFrame;

pub const OVERLAY_LABEL: &str = "overlay";

/// Hands the overlay a freeze frame and everything it needs to select against.
const ARM_EVENT: &str = "overlay://arm";
/// Sent when the overlay is hidden, so it can drop the decoded bitmap and go
/// back to idle. Advisory — nothing breaks if it is ignored, because an arm is
/// never shown until it has acknowledged that arm's own frame.
const DISARM_EVENT: &str = "overlay://disarm";

/// How long a capture waits for `overlay_ready` before giving up. Generous: this
/// is a bitmap decode plus one paint, tens of milliseconds in practice, and the
/// cost of being wrong is a capture that silently does nothing.
const READY_TIMEOUT: Duration = Duration::from_millis(2500);
/// How often the wait wakes up to re-emit the arm.
const READY_POLL: Duration = Duration::from_millis(100);

/// Delay before the startup pre-warm builds its window, so it cannot contend
/// with the main window's own creation or slow the visible part of launch.
const PREWARM_DELAY: Duration = Duration::from_millis(600);

/// The compositor needs a beat to actually drop a hidden window out of the
/// framebuffer, same as `HIDE_SETTLE` for the main window.
const OVERLAY_SETTLE: Duration = Duration::from_millis(120);

// -------------------------------------------------------------------------
// types
// -------------------------------------------------------------------------

/// What the overlay webview needs to paint itself.
///
/// `src` is a plain absolute filesystem path; the frontend runs it through
/// `convertFileSrc` and then **fetches** it — never `new Image()`, which taints
/// the canvas and breaks PNG export at commit time (M2.5 §0).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArmPayload {
    /// Monotonic, starts at 1. Echoed back by `overlay_ready`.
    pub arm_id: u64,
    pub src: String,
    pub width: u32,
    pub height: u32,
    pub origin_x: i32,
    pub origin_y: i32,
    pub windows: Vec<WindowRect>,
    /// `Settings.annotate_in_overlay`, resolved at arm time so the overlay does
    /// not have to round-trip for it.
    pub annotate: bool,
}

/// The M1 pull-based twin of `ArmPayload`, kept for the freeze frame alone.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FreezeInfo {
    pub src: String,
    pub width: u32,
    pub height: u32,
}

/// A selection in freeze-image pixel coordinates. The overlay measures its own
/// CSS-px -> image-px scale at runtime, so this is the only space Rust knows.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Which arm the overlay is on. Managed state, registered in `lib.rs`.
#[derive(Default)]
pub struct OverlayState {
    next_arm: AtomicU64,
    arm: Mutex<Arm>,
    ready: Condvar,
    /// Serialises `start_region_blocking`. Held for the whole arm, and never
    /// while `arm` is held.
    starting: Mutex<()>,
}

#[derive(Default)]
struct Arm {
    /// The arm currently in flight or on screen. 0 means idle.
    current: u64,
    /// The arm the overlay has acknowledged.
    ready: u64,
}

// -------------------------------------------------------------------------
// commands
// -------------------------------------------------------------------------

#[tauri::command]
pub async fn start_region_capture(app: AppHandle) -> Result<(), String> {
    run_blocking(move || start_region_blocking(&app)).await
}

/// The overlay has decoded and painted `armId`'s freeze frame and is safe to
/// show. Idempotent, and anything but the live arm is ignored — a superseded arm
/// answering late must not put a stale desktop on screen.
#[tauri::command]
pub fn overlay_ready(app: AppHandle, arm_id: u64) -> Result<(), String> {
    let state = app.state::<OverlayState>();
    let mut arm = lock(&state.arm);
    if arm_id != 0 && arm.current == arm_id {
        arm.ready = arm_id;
        state.ready.notify_all();
    }
    Ok(())
}

#[tauri::command]
pub fn get_freeze_frame(app: AppHandle) -> Result<FreezeInfo, String> {
    let state = app.state::<AppState>();
    let guard = lock(&state.freeze);
    let freeze = guard
        .as_ref()
        .ok_or_else(|| "no freeze frame is active".to_string())?;

    Ok(FreezeInfo {
        src: freeze.path.to_string_lossy().into_owned(),
        width: freeze.width,
        height: freeze.height,
    })
}

#[tauri::command]
pub async fn finish_region_capture(app: AppHandle, rect: Rect) -> Result<CaptureRecord, String> {
    run_blocking(move || {
        // Take the freeze before dismissing the window: the hide path also
        // touches state.
        let freeze = {
            let state = app.state::<AppState>();
            let taken = lock(&state.freeze).take();
            taken.ok_or_else(|| "no freeze frame is active".to_string())?
        };

        hide_overlay(&app);
        crate::restore_main_after_capture(&app);

        let img = image::open(&freeze.path)
            .map_err(|e| format!("could not reopen the freeze frame: {e}"))?
            .to_rgba8();

        let (iw, ih) = img.dimensions();
        let x = to_px(rect.x, iw.saturating_sub(1));
        let y = to_px(rect.y, ih.saturating_sub(1));
        let w = to_px(rect.width, iw);
        let h = to_px(rect.height, ih);

        let cropped = capture::crop(&img, x, y, w, h)?;
        finalize(&app, cropped, "region")
    })
    .await
}

/// The annotated twin of [`finish_region_capture`]: the overlay has already
/// cropped and rendered, so Rust only decodes and finalizes.
///
/// This path exists *because* it is expensive — a 4K annotated PNG is tens of
/// megabytes of base64 through the IPC — so the overlay only takes it when
/// shapes were actually drawn. A selection with nothing on it still goes through
/// the native crop above.
#[tauri::command]
pub async fn finish_region_capture_annotated(
    app: AppHandle,
    png: String,
) -> Result<CaptureRecord, String> {
    run_blocking(move || {
        // Same ordering as the native path: the freeze is consumed and the
        // window dismissed before any of the slow work starts, so the desktop
        // comes back the instant the user commits.
        {
            let state = app.state::<AppState>();
            if lock(&state.freeze).take().is_none() {
                return Err("no freeze frame is active".to_string());
            }
        }

        hide_overlay(&app);
        crate::restore_main_after_capture(&app);

        let img = decode_png(&decode_base64(&png)?)?;
        finalize(&app, img, "region")
    })
    .await
}

#[tauri::command]
pub async fn cancel_region_capture(app: AppHandle) -> Result<(), String> {
    run_blocking(move || {
        {
            let state = app.state::<AppState>();
            lock(&state.freeze).take();
        }

        hide_overlay(&app);
        crate::restore_main_after_capture(&app);
        Ok(())
    })
    .await
}

// -------------------------------------------------------------------------
// lifecycle
// -------------------------------------------------------------------------

/// Builds the overlay once, shortly after startup and off the main thread.
///
/// Everything after this point reuses that window; creating the webview and
/// loading the page *was* the region hotkey's latency.
pub(crate) fn prewarm(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        std::thread::sleep(PREWARM_DELAY);
        if app.get_webview_window(OVERLAY_LABEL).is_some() {
            return;
        }
        // A failed pre-warm is not fatal: `start_region_blocking` builds the
        // window on demand if it is missing.
        if let Err(e) = build_overlay(&app) {
            eprintln!("santi.sharex: could not pre-warm the overlay: {e}");
        }
    });
}

/// The blocking body of `start_region_capture`, also called straight from the
/// global-shortcut handler.
///
/// Must never run on the main thread: `WebviewWindowBuilder::build` blocks
/// waiting on the event loop and would deadlock.
pub(crate) fn start_region_blocking(app: &AppHandle) -> Result<(), String> {
    // One capture arms at a time. Every caller is already off the main thread —
    // the hotkey handler and the command both go through `spawn_blocking` — so
    // two presses land on two threads that would otherwise both grab the desktop
    // and write the *same* `freeze.png`. The overlay then fetches a half-written
    // file, fails to decode it, and cancels: not its own arm, which the second
    // press has already superseded, but the second press's. Serialising costs the
    // double-press path one arm's latency and nothing else; superseding still
    // happens, it just happens in order.
    let overlay_state = app.state::<OverlayState>();
    let _serial = lock(&overlay_state.starting);

    // Before the grab, and here rather than only in the hotkey dispatcher: the
    // main window's Capture-region button reaches this function directly, so a
    // preview still up from the last capture would otherwise be frozen into the
    // freeze frame — always-on-top, so it really is in those pixels — and stay
    // selectable inside the overlay for the rest of the capture (M2.9 §3).
    // `grab()` does the same for the fullscreen/window/monitor paths.
    crate::hide_preview_for_capture(app);

    // A second hotkey press while the overlay is already up. It is always-on-top,
    // so it has to be off the screen before the new grab or it freezes itself
    // into the frame. Superseding the arm id is not enough: that ends the old
    // capture, it does not clear the compositor. Only ever runs on the
    // double-press path, so the normal one pays nothing for it.
    if let Some(window) = app.get_webview_window(OVERLAY_LABEL) {
        if window.is_visible().unwrap_or(false) {
            hide_overlay(app);
            std::thread::sleep(OVERLAY_SETTLE);
        }
    }

    // Region capture deliberately does NOT hide the main window, whatever
    // `hide_window_on_capture` says.
    //
    // The freeze frame has to be the desktop the user was looking at when they
    // pressed the hotkey. Hiding santi.sharex first means they drag a selection over
    // a frozen image that is missing a window which was plainly on screen a moment
    // earlier — so selecting the santi.sharex window is impossible, and anything that
    // reflowed when it vanished is captured in the wrong place. The setting still
    // applies to fullscreen/window/monitor grabs (see `grab`), where santi.sharex
    // really would otherwise be in the shot; here the overlay covers the whole
    // desktop anyway, so hiding buys nothing and costs fidelity.

    let mut arm_id = 0u64;
    let result = arm_overlay(app, &mut arm_id);

    if result.is_err() {
        // Only unwind if this arm is still the live one. A second hotkey press
        // supersedes us, and cleaning up on its behalf would cancel *its*
        // capture and pop the main window back over the desktop it is selecting
        // from.
        if release_arm(app, arm_id) {
            {
                let state = app.state::<AppState>();
                lock(&state.freeze).take();
            }
            hide_overlay_window(app);
            crate::restore_main_after_capture(app);
        }
    }
    // On success the main window stays hidden until finish/cancel.
    result
}

/// Freeze the desktop, arm the overlay, wait for it, show it.
///
/// Writes the arm it claimed into `arm_id` so the caller can tell "we failed and
/// still own the overlay" from "we were superseded".
fn arm_overlay(app: &AppHandle, arm_id: &mut u64) -> Result<(), String> {
    // First, so that on the fallback path the page is already loading while the
    // grab and the PNG encode run. The window is invisible, so it cannot appear
    // in the shot.
    let window = overlay_window(app)?;

    let mut shot = capture::capture_virtual_desktop()?;

    // Drawn into the *freeze frame* rather than into the final crop, so the
    // overlay shows exactly what will be captured. The consequence is a frozen
    // cursor sitting under the live one while you drag, which is what ShareX
    // does too and is the honest rendering: that pixel really is what lands in
    // the file.
    if crate::cursor_wanted(app) {
        crate::cursor::overlay_onto(&mut shot);
    }

    // The composited image's real dimensions, not the union of the monitors'
    // reported ones — they can disagree and the image is what the overlay scales
    // against.
    let (width, height) = shot.image.dimensions();

    // Enumerated in the same instant as the grab, and shipped with the frame:
    // a list fetched after arming would describe a desktop that no longer
    // matches the frozen pixels. A failure here costs auto-detect, not the
    // capture.
    let windows = capture::list_window_rects().unwrap_or_default();

    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| format!("no cache directory: {e}"))?;
    fs::create_dir_all(&cache_dir)
        .map_err(|e| format!("could not create {}: {e}", cache_dir.display()))?;
    let path = cache_dir.join("freeze.png");

    let bytes = capture::encode_png_fast(&shot.image)?;
    fs::write(&path, &bytes).map_err(|e| format!("could not write the freeze frame: {e}"))?;

    let freeze = FreezeFrame {
        path,
        width,
        height,
        origin_x: shot.origin_x,
        origin_y: shot.origin_y,
    };

    let annotate = {
        let state = app.state::<AppState>();
        let settings = lock(&state.settings);
        settings.annotate_in_overlay
    };
    {
        let state = app.state::<AppState>();
        *lock(&state.freeze) = Some(freeze.clone());
    }

    // Moved and sized *before* arming, while still hidden: the overlay lays out
    // and paints against its final geometry, so there is nothing left to do
    // between "ready" and "show" but show. Both calls are invisible on a hidden
    // window.
    window
        .set_position(PhysicalPosition::new(freeze.origin_x, freeze.origin_y))
        .map_err(|e| e.to_string())?;
    window
        .set_size(PhysicalSize::new(freeze.width, freeze.height))
        .map_err(|e| e.to_string())?;

    let id = {
        let state = app.state::<OverlayState>();
        let id = state.next_arm.fetch_add(1, Ordering::SeqCst) + 1;
        let mut arm = lock(&state.arm);
        arm.current = id;
        arm.ready = 0;
        id
    };
    *arm_id = id;

    let payload = ArmPayload {
        arm_id: id,
        src: freeze.path.to_string_lossy().into_owned(),
        width: freeze.width,
        height: freeze.height,
        origin_x: freeze.origin_x,
        origin_y: freeze.origin_y,
        windows,
        annotate,
    };

    await_ready(app, id, &payload)?;

    // Ownership is re-checked on both sides of the show, and this is not
    // belt-and-braces — it is the whole safety of the pre-warmed window.
    //
    // `overlay_ready` is answered by the webview a beat before this thread wakes
    // from the condvar, so the user can already have pressed Escape (or the
    // hotkey again) in the gap. That path hides the window and puts the overlay
    // component back to `idle`, and `idle` paints nothing at all: showing here
    // afterwards would leave a borderless, always-on-top, undecorated sheet
    // covering the whole desktop with no frame in it and nothing armed behind
    // it — a full-screen lockout.
    //
    // A hide always clears `arm.current` *before* it touches the window
    // (`hide_overlay`), so: if it beat the pre-check we never show, and if it
    // landed between the two checks the post-check sees it and puts the window
    // back down. The lock is deliberately not held across `show()` —
    // `overlay_ready` is a synchronous command running on the main thread, and
    // `show()` waits on that same thread.
    if !owns_arm(app, id) {
        return Err("the region capture was superseded".to_string());
    }

    // Only now. Showing before the bitmap has decoded flashes an empty black
    // window across the whole desktop.
    window.show().map_err(|e| e.to_string())?;
    window.set_focus().map_err(|e| e.to_string())?;

    if !owns_arm(app, id) {
        hide_overlay_window(app);
        return Err("the region capture was superseded".to_string());
    }

    Ok(())
}

/// Whether `id` is still the arm on screen. See the two calls in `arm_overlay`.
fn owns_arm(app: &AppHandle, id: u64) -> bool {
    let state = app.state::<OverlayState>();
    let arm = lock(&state.arm);
    arm.current == id
}

/// Blocks until the overlay acknowledges `id`, we are superseded, or we run out
/// of patience.
fn await_ready(app: &AppHandle, id: u64, payload: &ArmPayload) -> Result<(), String> {
    let deadline = Instant::now() + READY_TIMEOUT;

    loop {
        // Re-emitted on every poll until acknowledged. On the fallback path the
        // window was only just built, so the first emit can easily land before
        // its listener exists. Re-arming before `ready` is free: by definition
        // the overlay has not painted anything yet.
        app.emit_to(OVERLAY_LABEL, ARM_EVENT, payload.clone())
            .map_err(|e| format!("could not arm the overlay: {e}"))?;

        let state = app.state::<OverlayState>();
        let mut arm = lock(&state.arm);
        loop {
            if arm.ready == id {
                return Ok(());
            }
            if arm.current != id {
                return Err("the region capture was superseded".to_string());
            }
            let (next, wait) = state
                .ready
                .wait_timeout(arm, READY_POLL)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            arm = next;
            if wait.timed_out() {
                break;
            }
        }
        drop(arm);

        if Instant::now() >= deadline {
            // Something is wrong with the pre-warmed webview — it never loaded,
            // or it crashed. Drop it so the next capture rebuilds a fresh one
            // rather than failing forever against the same corpse.
            if let Some(window) = app.get_webview_window(OVERLAY_LABEL) {
                let _ = window.destroy();
            }
            return Err("the region overlay did not respond in time".to_string());
        }
    }
}

/// The pre-warmed window, or a freshly built one if it is gone (startup failed,
/// or a timeout destroyed it). Slower, but region capture must not simply stop
/// working.
fn overlay_window(app: &AppHandle) -> Result<WebviewWindow, String> {
    match app.get_webview_window(OVERLAY_LABEL) {
        Some(window) => Ok(window),
        None => build_overlay(app),
    }
}

fn build_overlay(app: &AppHandle) -> Result<WebviewWindow, String> {
    WebviewWindowBuilder::new(
        app,
        OVERLAY_LABEL,
        // adapter-static never emits /overlay.html, so the second window is a
        // query param the shared +page.svelte branches on.
        //
        // The path is the bare query, *not* "index.html?w=overlay": Tauri joins
        // this onto the app base URL, and in `tauri dev` that base is the
        // SvelteKit dev server, which has no `/index.html` route and answers it
        // with a 404 page — a full-screen "Not found" over the user's desktop.
        // "?w=overlay" resolves to the root route in dev and, in a bundled app,
        // the asset protocol strips the query and maps "/" to index.html, so
        // both modes land on the same document.
        WebviewUrl::App("?w=overlay".into()),
    )
    .title("santi.sharex region capture")
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .shadow(false)
    // Hidden and unpositioned, and it stays that way until an arm has been
    // acknowledged. A pre-warmed borderless always-on-top window that leaks
    // visible is a full-screen lockout with no titlebar and no taskbar entry,
    // so `visible(false)` here and *every* exit path hides it again.
    .visible(false)
    .focused(false)
    .build()
    .map_err(|e| format!("could not create the overlay window: {e}"))
}

/// Ends whatever arm is live and hides the window. Never destroys it —
/// destroying is what put the webview build and the page load back on the
/// critical path.
pub(crate) fn hide_overlay(app: &AppHandle) {
    {
        let state = app.state::<OverlayState>();
        let mut arm = lock(&state.arm);
        arm.current = 0;
        arm.ready = 0;
        // Wakes any `await_ready` still blocked, which then sees it no longer
        // owns the overlay and unwinds.
        state.ready.notify_all();
    }
    hide_overlay_window(app);
}

fn hide_overlay_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = window.hide();
        let _ = app.emit_to(OVERLAY_LABEL, DISARM_EVENT, ());
    }
}

/// Clears the arm if `id` still owns it. `id == 0` means "we never claimed one",
/// which only counts as ownership while nothing else is in flight.
fn release_arm(app: &AppHandle, id: u64) -> bool {
    let state = app.state::<OverlayState>();
    let mut arm = lock(&state.arm);
    if arm.current != id {
        return false;
    }
    arm.current = 0;
    arm.ready = 0;
    state.ready.notify_all();
    true
}

// -------------------------------------------------------------------------
// internals
// -------------------------------------------------------------------------

/// The rect arrives from JS, so it can be fractional, negative, or NaN.
fn to_px(value: f64, max: u32) -> u32 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    value.round().min(max as f64) as u32
}

/// The frontend strips the `data:image/png;base64,` prefix off `toDataURL`, but
/// tolerate one arriving anyway rather than rejecting a perfectly good image.
fn decode_base64(png: &str) -> Result<Vec<u8>, String> {
    let payload = png
        .split_once("base64,")
        .map(|(_, rest)| rest)
        .unwrap_or(png);

    STANDARD
        .decode(payload.trim())
        .map_err(|e| format!("the annotated image is not valid base64: {e}"))
}

fn decode_png(bytes: &[u8]) -> Result<image::RgbaImage, String> {
    Ok(
        image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
            .map_err(|e| format!("the annotated image is not a readable PNG: {e}"))?
            .to_rgba8(),
    )
}
