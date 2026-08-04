//! The post-capture preview window (M2.9 §3).
//!
//! A ~300x200 thumbnail that appears bottom-right after every capture, the way
//! ShareX does it, and takes itself off screen a few seconds later.
//!
//! Two properties make this window different from every other one in santi.sharex,
//! and both of them are load-bearing:
//!
//! **It must never take focus.** It appears while the user is doing something
//! else — mid-sentence, mid-shortcut — so activating it would swallow whatever
//! they were typing. `focused(false)` only covers the *first* show: tao clears
//! its don't-activate marker once, so a second `show()` after a hide would fall
//! back to `SW_SHOW` and steal the foreground. `focusable(false)` is what makes
//! it structural — it puts `WS_EX_NOACTIVATE` on the window, so no show and no
//! click can ever activate it. Mouse input still arrives, which is all the
//! preview needs; keyboard focus is exactly what it must not have. Nothing here
//! calls `set_focus`.
//!
//! **It must never be left on screen.** It is always-on-top, borderless and
//! unfocusable: a preview that fails to hide is a sticker the user cannot click
//! away, cannot alt-tab to and cannot close. So every path ends hidden —
//! [`hide`] is called from the dismiss command, from every error path in
//! [`show_blocking`], from the next capture, and from `RunEvent::Exit`. On top
//! of that [`MAX_ON_SCREEN`] is a hard backstop for the case the webview never
//! runs its own timer at all (a load failure, a JS exception before it arms).
//!
//! Like the overlay (M2.5 §1) the window is built once and then hidden and
//! reused. It is never destroyed.

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use tauri::{
    AppHandle, Emitter, Manager, Monitor, PhysicalPosition, PhysicalSize, WebviewUrl,
    WebviewWindow, WebviewWindowBuilder,
};

use crate::lock;
use crate::store::CaptureRecord;

pub const PREVIEW_LABEL: &str = "preview";

/// Hands the preview the capture to show, payload a [`CaptureRecord`].
///
/// A capture that arrives while one is already up re-emits this rather than
/// opening a second window: same window, new content, restarted timer.
const SHOW_EVENT: &str = "preview://show";

/// Advisory: the window is going down, so drop the record and cancel the
/// countdown. Nothing depends on it being heard — the window is hidden either
/// way — it only stops the webview repainting a capture that is no longer on
/// screen.
const HIDE_EVENT: &str = "preview://hide";

/// Logical size and the gap from the work area's bottom-right corner.
const PREVIEW_WIDTH: f64 = 300.0;
const PREVIEW_HEIGHT: f64 = 200.0;
const PREVIEW_INSET: f64 = 24.0;

/// Only paid the first time a capture needs the window: the webview has just
/// been created and has neither loaded the page nor attached its listener.
/// Showing an empty box and filling it a beat later is worse than appearing
/// slightly late. Every later preview reuses the window and skips this.
const BUILD_WARMUP: Duration = Duration::from_millis(350);

/// Hard cap on how long the preview may stay up, whatever the webview is doing.
///
/// The webview owns the real ~4s countdown (and pauses it on hover, so it can
/// legitimately outlive that). This exists only for the case where it never
/// counts down at all — a page that failed to load, or a JS error thrown before
/// the timer is armed — which would otherwise leave an unfocusable always-on-top
/// window on screen forever. Deliberately far longer than any real hover.
const MAX_ON_SCREEN: Duration = Duration::from_secs(60);

// -------------------------------------------------------------------------
// state
// -------------------------------------------------------------------------

/// Which capture the preview is on. Managed state, registered in `lib.rs`.
#[derive(Default)]
pub struct PreviewState {
    current: Mutex<Current>,
    /// Signalled by every claim and every hide, so the backstop stops waiting
    /// the moment it stops being relevant.
    changed: Condvar,
    /// Serialises [`show_blocking`], so two captures in quick succession touch
    /// the window one after the other instead of racing to build it. Never held
    /// while `current` is.
    showing: Mutex<()>,
}

#[derive(Default)]
struct Current {
    /// Monotonic. Bumped by every show *and* every hide, so a show still in
    /// flight, and the backstop timer behind it, can both tell whether they are
    /// still the reason the window is up.
    generation: u64,
    record: Option<CaptureRecord>,
}

/// Claims the window for `record` and returns the generation that owns it.
fn claim(app: &AppHandle, record: &CaptureRecord) -> u64 {
    let state = app.state::<PreviewState>();
    let mut current = lock(&state.current);
    current.generation = current.generation.wrapping_add(1);
    current.record = Some(record.clone());
    state.changed.notify_all();
    current.generation
}

fn owns(app: &AppHandle, generation: u64) -> bool {
    let state = app.state::<PreviewState>();
    let current = lock(&state.current).generation;
    current == generation
}

/// Blocks until `generation` stops owning the window, or until it has been up
/// for [`MAX_ON_SCREEN`]. `true` means it timed out and the preview is *still*
/// ours — i.e. nothing ever took it down and the backstop has to.
fn await_expiry(app: &AppHandle, generation: u64) -> bool {
    let deadline = Instant::now() + MAX_ON_SCREEN;
    let state = app.state::<PreviewState>();
    let mut current = lock(&state.current);

    while current.generation == generation {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return true;
        }
        let (next, _) = state
            .changed
            .wait_timeout(current, remaining)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        current = next;
    }
    false
}

// -------------------------------------------------------------------------
// commands
// -------------------------------------------------------------------------

/// Takes the preview off screen. The webview's own timer, its × button and its
/// click-to-edit all end here.
///
/// The name is the contract: the frontend invokes `hide_capture_preview`
/// (`hideCapturePreview` in `api.ts`), and its only fallback for a failed
/// invoke is `getCurrentWindow().hide()` — which is exactly the kind of quiet
/// half-working dismissal this window must not depend on. So the command keeps
/// the name the webview asks for.
#[tauri::command]
pub fn hide_capture_preview(app: AppHandle) -> Result<(), String> {
    hide(&app);
    Ok(())
}

/// The capture currently on screen, or `null`.
///
/// The catch-up path for the single [`SHOW_EVENT`] emit: a freshly built webview
/// can attach its listener after the emit has already gone out, and reading this
/// on mount is how it recovers the record instead of waiting for an event it
/// missed. `CapturePreview.svelte` does not call it today — it covers the same
/// case by dismissing itself if no record has arrived within its orphan timeout,
/// which is safe but loses the first preview. Kept so that window can be filled
/// rather than thrown away.
#[tauri::command]
pub fn get_preview_record(app: AppHandle) -> Option<CaptureRecord> {
    let state = app.state::<PreviewState>();
    let record = lock(&state.current).record.clone();
    record
}

// -------------------------------------------------------------------------
// lifecycle
// -------------------------------------------------------------------------

/// Puts `record` on screen, or replaces whatever is already there.
///
/// Returns immediately: everything below runs on a blocking thread, so a slow
/// or failing preview can never delay — or fail — the capture that triggered
/// it. Called from `finalize` when `Settings.show_capture_preview` is set.
pub(crate) fn show(app: &AppHandle, record: &CaptureRecord) {
    // Claimed synchronously, before the thread exists, so two captures land in
    // the order they happened and the later one always wins.
    let generation = claim(app, record);

    let app = app.clone();
    let record = record.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(e) = show_blocking(&app, &record, generation) {
            // Anything at all going wrong ends with the window down. It may
            // already be up and empty at this point — that is precisely the
            // state this must not be left in.
            eprintln!("santi.sharex: could not show the capture preview: {e}");
            hide(&app);
            return;
        }

        // The backstop. Runs on this same thread now that the serialiser is
        // released, so it costs nothing extra, and any dismiss — or the next
        // capture — wakes it early.
        if await_expiry(&app, generation) {
            eprintln!(
                "santi.sharex: the capture preview was still up after {}s and was hidden",
                MAX_ON_SCREEN.as_secs()
            );
            hide(&app);
        }
    });
}

fn show_blocking(app: &AppHandle, record: &CaptureRecord, generation: u64) -> Result<(), String> {
    let state = app.state::<PreviewState>();
    let _serial = lock(&state.showing);

    // Superseded while we waited for the serialiser: the newer capture owns the
    // window and is about to show its own record. Showing ours would flash the
    // wrong thumbnail and, worse, hand the backstop to the wrong generation.
    if !owns(app, generation) {
        return Ok(());
    }

    let (window, built) = preview_window(app)?;
    if built {
        std::thread::sleep(BUILD_WARMUP);
        if !owns(app, generation) {
            return Ok(());
        }
    }

    // Before positioning, so the webview is already laying out the new record
    // while the window moves. A listener that is not up yet costs nothing —
    // `get_preview_record` is how the webview catches up on mount.
    app.emit_to(PREVIEW_LABEL, SHOW_EVENT, record.clone())
        .map_err(|e| format!("could not hand the preview its capture: {e}"))?;

    place(app, &window)?;

    if !owns(app, generation) {
        return Ok(());
    }
    // `show`, never `set_focus`. See the module header.
    window
        .show()
        .map_err(|e| format!("could not show the preview window: {e}"))?;

    // A dismiss can land between the check above and the show — the command
    // runs on the main thread while this one blocks on it — and its `hide()`
    // would then have hidden a window that was not yet visible. Re-checking is
    // what stops that leaving the preview up with nothing left to take it down.
    if !owns(app, generation) {
        hide(app);
    }
    Ok(())
}

/// Ends whatever is on screen: invalidates the live generation (which retires
/// any show still in flight and any backstop behind it), forgets the record and
/// hides the window.
///
/// Safe to call at any time, including when the window was never built.
pub(crate) fn hide(app: &AppHandle) {
    {
        let state = app.state::<PreviewState>();
        let mut current = lock(&state.current);
        current.generation = current.generation.wrapping_add(1);
        current.record = None;
        state.changed.notify_all();
    }

    if let Some(window) = app.get_webview_window(PREVIEW_LABEL) {
        let _ = window.hide();
        let _ = app.emit_to(PREVIEW_LABEL, HIDE_EVENT, ());
    }
}

/// The preview window, built if this is the first capture that needs it. The
/// flag says whether it was built just now, which is the only case that pays
/// [`BUILD_WARMUP`].
fn preview_window(app: &AppHandle) -> Result<(WebviewWindow, bool), String> {
    if let Some(window) = app.get_webview_window(PREVIEW_LABEL) {
        return Ok((window, false));
    }
    Ok((build(app)?, true))
}

fn build(app: &AppHandle) -> Result<WebviewWindow, String> {
    WebviewWindowBuilder::new(
        app,
        PREVIEW_LABEL,
        // The bare query, *not* "index.html?w=preview": Tauri joins this onto
        // the app base URL, and under `tauri dev` that base is the SvelteKit dev
        // server, which has no /index.html route and answers it with a 404 page.
        // See the same note in overlay.rs and editor.rs.
        WebviewUrl::App("?w=preview".into()),
    )
    .title("santi.sharex capture preview")
    .inner_size(PREVIEW_WIDTH, PREVIEW_HEIGHT)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .shadow(false)
    // Built hidden and unpositioned: it is placed against the right monitor
    // before it is ever shown, so it cannot appear in the wrong corner first.
    .visible(false)
    // The pair that keeps the foreground window the user's. `focused(false)` is
    // the one-shot marker for this first show; `focusable(false)` is the
    // permanent `WS_EX_NOACTIVATE` that covers every show after a hide.
    .focused(false)
    .focusable(false)
    .build()
    .map_err(|e| format!("could not create the preview window: {e}"))
}

/// Bottom-right of the monitor the capture came from, inset and clear of the
/// taskbar.
fn place(app: &AppHandle, window: &WebviewWindow) -> Result<(), String> {
    let monitor = target_monitor(app)?;
    let scale = monitor.scale_factor();
    // The *work* area, not the monitor bounds — the difference is the taskbar,
    // and the preview sits in exactly the corner the taskbar lives in.
    let area = monitor.work_area();

    let width = ((PREVIEW_WIDTH * scale).round() as u32).max(1);
    let height = ((PREVIEW_HEIGHT * scale).round() as u32).max(1);
    let inset = (PREVIEW_INSET * scale).round() as i32;

    let right = area.position.x.saturating_add(area.size.width as i32);
    let bottom = area.position.y.saturating_add(area.size.height as i32);
    // Clamped to the work area's own top-left, so a display smaller than the
    // preview pushes it off the *outside* edge rather than off the screen.
    let x = (right - width as i32 - inset).max(area.position.x);
    let y = (bottom - height as i32 - inset).max(area.position.y);

    // Position first: moving between monitors of different DPI makes Windows
    // resize the window to its own suggestion, so the size has to be the last
    // word. Both are physical, so neither is reinterpreted on the way.
    window
        .set_position(PhysicalPosition::new(x, y))
        .map_err(|e| format!("could not position the preview window: {e}"))?;
    window
        .set_size(PhysicalSize::new(width, height))
        .map_err(|e| format!("could not size the preview window: {e}"))?;
    Ok(())
}

/// The monitor the capture came from, approximated by the one the pointer is
/// on.
///
/// Nothing in `CaptureRecord` carries a monitor, and for the two cases where it
/// would differ — a region drag or a window grab on a second display — the
/// pointer is on that display by definition, because the user just used it
/// there. A fullscreen grab spans everything and has no single answer anyway.
fn target_monitor(app: &AppHandle) -> Result<Monitor, String> {
    if let Ok(cursor) = app.cursor_position() {
        if let Ok(Some(monitor)) = app.monitor_from_point(cursor.x, cursor.y) {
            return Ok(monitor);
        }
    }
    app.primary_monitor()
        .map_err(|e| format!("could not read the monitor layout: {e}"))?
        .ok_or_else(|| "no monitor to place the capture preview on".to_string())
}
