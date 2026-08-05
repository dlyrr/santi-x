//! M6: workflows — one hotkey, a whole chain.
//!
//! A workflow is `capture → actions → destination` bound to one trigger. **It
//! adds no capability.** Every step here is a call into something M1–M5 already
//! shipped: `crate::grab` and `overlay::start_region_blocking` for the capture,
//! `editor::open_editor_blocking` for the annotator, `ocr::ocr_capture`,
//! `scroll::start_scroll_capture`, `record::start_recording`,
//! `upload::upload_capture`. Nothing in this file re-implements any of them, and
//! a step that seems to need a new path is a step that is wrong.
//!
//! # The three things that are hard
//!
//! **Waiting for the editor.** `Annotate` opens a separate window and has to
//! block the rest of the chain until the user saves or cancels. Getting that
//! wrong uploads the *un-annotated* image, which for a redaction workflow is a
//! privacy failure rather than a bug — so the wait is on a real completion
//! signal ([`EDITOR_DONE_EVENT`], correlated by capture id) with the editor
//! window going away as a backstop that counts as a **cancel**. There is no
//! sleep-and-hope anywhere in it, and every ambiguous outcome resolves to "do
//! not upload".
//!
//! **Sequential, never concurrent.** Two workflows overlapping means two region
//! overlays, which is a full-screen lockout with no titlebar. [`WorkflowState`]
//! holds exactly one run and a second trigger is refused with a sentence, the
//! same answer a second recording gets.
//!
//! **Cancellable everywhere.** [`Halt::Cancelled`] is not an error — the user
//! changed their mind — and every terminal path runs [`clear_the_screen`], so a
//! cancel can never leave the overlay, the editor or the recorder HUD up.
//!
//! # Shape of the code
//!
//! [`plan`] turns a [`Workflow`] into the flat list of [`Step`]s the runner
//! walks, and [`drive`] walks it against an [`Engine`]. That split is what lets
//! the ordering, the cancel-stops-the-chain rule and the never-upload-after-a-
//! cancel rule be tested without taking a real screenshot; [`AppEngine`] is the
//! implementation that actually touches the app.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Listener, Manager};
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::store::{self, AppState, CaptureRecord};
use crate::{capture, editor, hotkeys, ocr, overlay, record, scroll, upload};

// ---------------------------------------------------------------------------
// events
// ---------------------------------------------------------------------------

/// `{ id, step, index, total }` before every step. A chain that runs silently
/// for eight seconds looks broken (M6 §2).
const PROGRESS_EVENT: &str = "workflow://progress";
/// Terminal, and the only two. `done` also carries a cancel — a workflow the
/// user backed out of finished, it did not fail.
const DONE_EVENT: &str = "workflow://done";
const ERROR_EVENT: &str = "workflow://error";
/// Emitted whenever the stored list changes, so every view holding it converges
/// without polling. Mirrors `settings://changed`.
const CHANGED_EVENT: &str = "workflows://changed";

/// The editor's answer to `Annotate`, emitted by the editor window on save, on
/// cancel and on close: `{ id, saved, newId? }`.
///
/// `id` is the capture the editor was opened on, so a late answer from an
/// earlier edit cannot resolve this workflow's wait. `newId` is set when the
/// user saved as a *new* capture, which is the record the rest of the chain has
/// to carry — uploading the original there would upload the un-annotated image.
const EDITOR_DONE_EVENT: &str = "editor://done";

/// The overlay's optional fast path for "the user pressed Escape".
///
/// Region capture is the one step with no distinct cancel signal of its own:
/// commit and cancel both take the freeze frame and hide the window, and only
/// commit goes on to [`crate::finalize`]. The runner therefore infers a cancel
/// from "the freeze went away and no capture landed within
/// [`COMMIT_GRACE`]" — correct, but it makes an Escape take a few seconds to
/// report. If the overlay emits this, the wait ends immediately instead.
/// Nothing breaks when it is absent.
const REGION_CANCELLED_EVENT: &str = "overlay://cancelled";

/// M4's terminal recording events, which are already exactly the completion
/// signal the `Record` capture step needs.
const RECORD_FINISHED_EVENT: &str = "record://finished";
const RECORD_CANCELLED_EVENT: &str = "record://cancelled";

// ---------------------------------------------------------------------------
// timings
// ---------------------------------------------------------------------------

/// How often a wait wakes to re-check the world. Every signal also notifies the
/// condvar, so this is the ceiling on latency, not the latency.
const POLL: Duration = Duration::from_millis(100);

/// How long the region wait keeps expecting a capture after the freeze frame
/// has been consumed. A commit spends this decoding the freeze PNG (or a
/// multi-megabyte base64 annotation), cropping, re-encoding and writing; a
/// cancel spends it doing nothing, and is then reported as the clean cancel it
/// is. Generous on purpose: the wrong answer in the short direction is "your
/// capture vanished".
const COMMIT_GRACE: Duration = Duration::from_secs(6);

/// How long "the editor window went away" waits for an [`EDITOR_DONE_EVENT`]
/// that may still be in flight behind it. An editor that saves and then closes
/// emits before it destroys, but the two travel over the IPC independently, and
/// resolving a save as a cancel would silently drop the annotation the user
/// just made.
const EDITOR_CLOSE_GRACE: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------------
// the shape (M6 §1)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workflow {
    pub id: String,
    pub name: String,
    /// Named default: a workflow arriving from a hand-edited file without the
    /// key is one the user wrote to use, not one they disabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub trigger: Trigger,
    pub capture: CaptureStep,
    #[serde(default)]
    pub actions: Vec<ActionStep>,
    /// A destination id (`"imgur"`, `"custom"`, `"ftp"`), or `None` to skip
    /// uploading. Deliberately **not** normalised on the way in the way
    /// `Settings::destination` is: an id this build cannot dispatch stays
    /// visible so the editor and the runner can both say so by name, rather
    /// than quietly becoming a workflow that uploads nowhere.
    #[serde(default)]
    pub destination: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "type")]
pub enum Trigger {
    Hotkey { accelerator: String },
    Manual,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "type")]
pub enum CaptureStep {
    Region,
    Fullscreen,
    ActiveWindow,
    Monitor { id: u32 },
    Window { id: u32 },
    Scrolling { window: u32 },
    /// `format` is `"mp4"` | `"gif"`, normalised through
    /// [`store::normalize_record_format`] on the way to the encoder.
    Record { format: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "type")]
pub enum ActionStep {
    /// Opens the editor and **waits for it**. See the module header.
    Annotate,
    SaveToDisk,
    CopyImage,
    Ocr { copy_text: bool },
    OpenFolder,
}

fn default_true() -> bool {
    true
}

impl CaptureStep {
    /// What the user calls this step, for progress rows and error sentences.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Region => "Capture region",
            Self::Fullscreen => "Capture fullscreen",
            Self::ActiveWindow => "Capture active window",
            Self::Monitor { .. } => "Capture monitor",
            Self::Window { .. } => "Capture window",
            Self::Scrolling { .. } => "Scrolling capture",
            Self::Record { .. } => "Record",
        }
    }
}

impl ActionStep {
    /// The `step` field of [`PROGRESS_EVENT`]. Stable — the UI keys off it.
    pub fn id(&self) -> &'static str {
        match self {
            Self::Annotate => "annotate",
            Self::SaveToDisk => "saveToDisk",
            Self::CopyImage => "copyImage",
            Self::Ocr { .. } => "ocr",
            Self::OpenFolder => "openFolder",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Annotate => "Annotate",
            Self::SaveToDisk => "Save to disk",
            Self::CopyImage => "Copy image",
            Self::Ocr { .. } => "Extract text",
            Self::OpenFolder => "Open folder",
        }
    }
}

// ---------------------------------------------------------------------------
// the plan
// ---------------------------------------------------------------------------

/// One step of a run, in the order the runner walks them.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Capture(CaptureStep),
    Action(ActionStep),
    Upload(String),
}

impl Step {
    /// The `step` field of [`PROGRESS_EVENT`].
    pub fn id(&self) -> &'static str {
        match self {
            Self::Capture(_) => "capture",
            Self::Action(action) => action.id(),
            Self::Upload(_) => "upload",
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Capture(capture) => capture.label().to_string(),
            Self::Action(action) => action.label().to_string(),
            Self::Upload(destination) => format!("Upload to {destination}"),
        }
    }
}

/// The steps `workflow` will run, flattened.
///
/// The runner walks exactly this and `total` is exactly its length, so the
/// progress a user sees and the work that happens cannot drift apart.
pub fn plan(workflow: &Workflow) -> Vec<Step> {
    let mut steps = Vec::with_capacity(2 + workflow.actions.len());
    steps.push(Step::Capture(workflow.capture.clone()));
    steps.extend(workflow.actions.iter().cloned().map(Step::Action));
    if let Some(destination) = &workflow.destination {
        steps.push(Step::Upload(destination.clone()));
    }
    steps
}

// ---------------------------------------------------------------------------
// driving the plan
// ---------------------------------------------------------------------------

/// Why a run stopped early.
#[derive(Debug, Clone, PartialEq)]
pub enum Halt {
    /// The user changed their mind — a cancelled region selection, a cancelled
    /// annotation, an explicit Cancel. **Not an error**, and never followed by
    /// an upload.
    Cancelled,
    /// A step failed. Names which step and why, and stops the chain there: a
    /// half-finished result must not carry on to the destination (M6 §2).
    Failed { step: String, message: String },
}

/// What a completed run produced.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Done {
    pub capture_id: Option<String>,
    pub url: Option<String>,
}

/// Everything [`drive`] needs from the outside world.
///
/// Split out so the ordering rules can be tested against a recorder instead of
/// against a real screen grab: the interesting failures in this milestone are
/// "the chain carried on after a cancel" and "the upload ran before the
/// annotation", and neither needs a capture to demonstrate.
trait Engine {
    fn cancelled(&self) -> bool;
    fn progress(&mut self, step: &Step, index: usize, total: usize);
    /// Returns the capture id the rest of the chain works on.
    fn capture(&mut self, step: &CaptureStep) -> Result<String, Halt>;
    /// Returns the capture id *after* the action — `Annotate` can replace it,
    /// when the user saved the annotation as a new capture.
    fn action(&mut self, step: &ActionStep, capture: &str) -> Result<String, Halt>;
    fn upload(&mut self, destination: &str, capture: &str) -> Result<String, Halt>;
}

/// Walks the plan, checking for a cancel on both sides of every step.
///
/// The check *after* `progress` is not redundant: `progress` is the boundary a
/// cancel most often lands on, and one step of a workflow can be a five-minute
/// recording.
fn drive<E: Engine>(engine: &mut E, plan: &[Step]) -> Result<Done, Halt> {
    let total = plan.len();
    let mut capture: Option<String> = None;
    let mut url: Option<String> = None;

    for (index, step) in plan.iter().enumerate() {
        if engine.cancelled() {
            return Err(Halt::Cancelled);
        }
        engine.progress(step, index, total);
        if engine.cancelled() {
            return Err(Halt::Cancelled);
        }

        match step {
            Step::Capture(kind) => capture = Some(engine.capture(kind)?),
            Step::Action(action) => {
                let id = require_capture(&capture, action.label())?;
                capture = Some(engine.action(action, &id)?);
            }
            Step::Upload(destination) => {
                let id = require_capture(&capture, "Upload")?;
                url = Some(engine.upload(destination, &id)?);
            }
        }
    }

    Ok(Done {
        capture_id: capture,
        url,
    })
}

/// A plan whose first step is not a capture cannot happen through [`plan`], but
/// it must not be able to reach a destination with nothing to send either.
fn require_capture(capture: &Option<String>, step: &str) -> Result<String, Halt> {
    capture.clone().ok_or_else(|| Halt::Failed {
        step: step.to_string(),
        message: "there is no capture to work on".to_string(),
    })
}

// ---------------------------------------------------------------------------
// state
// ---------------------------------------------------------------------------

/// The workflows, the one run that may be in flight, and the signals the run is
/// waiting on. Managed state, registered in `lib.rs`.
#[derive(Default)]
pub struct WorkflowState {
    workflows: Mutex<Vec<Workflow>>,
    /// Why `workflows.json` could not be read, when it could not. Kept so the
    /// first write can put the unreadable file aside instead of overwriting it.
    load_error: Mutex<Option<String>>,
    run: Mutex<Option<Arc<Run>>>,
    /// `slot -> workflow id` for the hotkey registry. Rebuilt on every
    /// registration pass, so a slot can never fire a workflow that was edited
    /// out from under it.
    slots: Mutex<Vec<(u64, String)>>,
    next_slot: AtomicU64,
    signals: Mutex<Signals>,
    woke: Condvar,
}

/// The run in flight.
struct Run {
    workflow_id: String,
    cancel: AtomicBool,
    /// What the UI shows for a view that mounted mid-run.
    at: Mutex<(String, usize, usize)>,
}

/// Everything the runner blocks on, filled in by listeners and by the one hook
/// in [`crate::finalize`].
///
/// One set rather than one per run, because there is only ever one run; it is
/// cleared when a run claims the slot so a signal that arrived late for the
/// previous one cannot resolve this one's wait.
#[derive(Default)]
struct Signals {
    /// The last capture that came out of [`crate::finalize`].
    landed: Option<CaptureRecord>,
    /// A recording that finished or was discarded.
    recorded: Option<RecordSignal>,
    /// The editor's answer, keyed by the capture it was opened on.
    editor: Option<EditorSignal>,
    /// When the editor window went away without one.
    editor_gone: Option<Instant>,
    /// The overlay's optional "the user pressed Escape".
    region_cancelled: bool,
}

#[derive(Debug, Clone)]
struct RecordSignal {
    record: Option<CaptureRecord>,
    cancelled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditorSignal {
    /// The capture the editor was opened on.
    id: String,
    saved: bool,
    /// Set when the user saved as a new capture rather than replacing.
    #[serde(default)]
    new_id: Option<String>,
}

/// The payload of [`RECORD_FINISHED_EVENT`] / [`RECORD_CANCELLED_EVENT`], which
/// M4 emits as a `RecordOutcome`. Only the two fields the runner needs are read
/// back, so the two structs can gain fields independently.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecordOutcomeWire {
    #[serde(default)]
    record: Option<CaptureRecord>,
    #[serde(default)]
    cancelled: bool,
}

impl WorkflowState {
    pub fn new(app: &AppHandle) -> Self {
        let state = Self::default();
        match store::load_workflows(app) {
            Ok(workflows) => *crate::lock(&state.workflows) = workflows,
            // An empty list and a reported error, never a panic and never a
            // rewritten file (M6 §1).
            Err(e) => *crate::lock(&state.load_error) = Some(e),
        }
        state
    }
}

fn state(app: &AppHandle) -> tauri::State<'_, WorkflowState> {
    app.state::<WorkflowState>()
}

pub fn all(app: &AppHandle) -> Vec<Workflow> {
    crate::lock(&state(app).workflows).clone()
}

fn by_id(app: &AppHandle, id: &str) -> Option<Workflow> {
    crate::lock(&state(app).workflows)
        .iter()
        .find(|w| w.id == id)
        .cloned()
}

/// Why the stored list could not be read, if it could not. Reported once at
/// startup by `lib.rs`.
pub(crate) fn load_error(app: &AppHandle) -> Option<String> {
    crate::lock(&state(app).load_error).clone()
}

// ---------------------------------------------------------------------------
// signals
// ---------------------------------------------------------------------------

fn signal<F: FnOnce(&mut Signals)>(app: &AppHandle, fill: F) {
    let state = state(app);
    {
        let mut signals = crate::lock(&state.signals);
        fill(&mut signals);
    }
    state.woke.notify_all();
}

/// Called from [`crate::finalize`] for every capture the app produces.
///
/// This is the region step's completion signal: the overlay's commit paths end
/// in `finalize`, and its cancel path does not.
pub(crate) fn capture_landed(app: &AppHandle, record: &CaptureRecord) {
    // Nothing is waiting, so nothing needs remembering — and a run that starts
    // later must not find a stale capture sitting in the slot.
    if crate::lock(&state(app).run).is_none() {
        return;
    }
    let record = record.clone();
    signal(app, move |signals| signals.landed = Some(record));
}

/// The editor finished on its own terms — saved or cancelled.
///
/// Called directly rather than through [`EDITOR_DONE_EVENT`] because the editor
/// lives in this process: emitting an event only to listen for it in the same
/// binary adds a hop that can be lost. The event constant stays because the
/// *webview* half may also announce itself, and both land in the same slot.
pub(crate) fn editor_finished(app: &AppHandle, id: &str, saved: bool, new_id: Option<String>) {
    let answer = EditorSignal {
        id: id.to_string(),
        saved,
        new_id,
    };
    signal(app, move |signals| {
        signals.editor_gone = None;
        signals.editor = Some(answer);
    });
}

/// The editor was dismissed without saving. Ends a waiting workflow as a cancel,
/// which is what stops a redaction chain uploading the image the user just
/// decided not to annotate.
pub(crate) fn editor_cancelled(app: &AppHandle) {
    // Deliberately routed through the "window went away" path rather than
    // synthesising an [`EditorSignal`]: the close command does not carry the
    // capture id, and the wait correlates answers by id — an answer with the
    // wrong id is *dropped*, so a synthesised one would be silently ignored and
    // the workflow would hang until the grace timer caught it anyway. This takes
    // the same path with none of the pretence.
    editor_window_closed(app);
}

/// The editor window went away. Counts as a **cancel** unless an
/// [`EDITOR_DONE_EVENT`] turns up within [`EDITOR_CLOSE_GRACE`], which is what
/// makes closing the editor with its X end a redaction workflow without
/// uploading. Wired from `lib.rs`'s window-event handler.
pub(crate) fn editor_window_closed(app: &AppHandle) {
    signal(app, |signals| {
        if signals.editor.is_none() && signals.editor_gone.is_none() {
            signals.editor_gone = Some(Instant::now());
        }
    });
}

/// Attaches the listeners a run waits on, once, for the life of the process.
///
/// They are up before any workflow can be triggered rather than being attached
/// per run: a listener registered at the moment a step starts is a listener that
/// can miss the answer to that step.
pub(crate) fn init(app: &AppHandle) {
    let handle = app.clone();
    app.listen(EDITOR_DONE_EVENT, move |event| {
        match serde_json::from_str::<EditorSignal>(event.payload()) {
            Ok(answer) => signal(&handle, move |signals| {
                signals.editor_gone = None;
                signals.editor = Some(answer);
            }),
            Err(e) => eprintln!("santi.sharex: unreadable {EDITOR_DONE_EVENT} payload: {e}"),
        }
    });

    let handle = app.clone();
    app.listen(REGION_CANCELLED_EVENT, move |_| {
        signal(&handle, |signals| signals.region_cancelled = true);
    });

    for name in [RECORD_FINISHED_EVENT, RECORD_CANCELLED_EVENT] {
        let handle = app.clone();
        app.listen(name, move |event| {
            let wire = serde_json::from_str::<RecordOutcomeWire>(event.payload()).unwrap_or(
                RecordOutcomeWire {
                    record: None,
                    // An unreadable outcome is not a recording we can hand on,
                    // and inventing a success would upload a file we never saw.
                    cancelled: true,
                },
            );
            signal(&handle, move |signals| {
                signals.recorded = Some(RecordSignal {
                    record: wire.record,
                    cancelled: wire.cancelled,
                })
            });
        });
    }
}

// ---------------------------------------------------------------------------
// starting and stopping
// ---------------------------------------------------------------------------

/// Claims the one run slot, or says why it could not.
///
/// Separate from everything that touches the app so the "sequential, never
/// concurrent" rule can be tested directly.
fn claim(state: &WorkflowState, workflow_id: &str) -> Result<Arc<Run>, String> {
    let mut slot = crate::lock(&state.run);
    if slot.is_some() {
        return Err(
            "A workflow is already running — let it finish or cancel it first.".to_string(),
        );
    }
    let run = Arc::new(Run {
        workflow_id: workflow_id.to_string(),
        cancel: AtomicBool::new(false),
        at: Mutex::new((String::new(), 0, 0)),
    });
    *slot = Some(Arc::clone(&run));
    *crate::lock(&state.signals) = Signals::default();
    Ok(run)
}

/// Frees the slot, but only if `run` is still the one holding it.
fn release(state: &WorkflowState, run: &Arc<Run>) {
    let mut slot = crate::lock(&state.run);
    if slot
        .as_ref()
        .map(|live| live.workflow_id == run.workflow_id && Arc::ptr_eq(live, run))
        .unwrap_or(false)
    {
        *slot = None;
    }
}

/// Starts `workflow` and returns immediately.
///
/// The run gets a dedicated thread rather than a `spawn_blocking` worker: it can
/// sit inside a five-minute recording, and parking a pool thread for that would
/// starve the captures and uploads that share it.
pub(crate) fn start(app: &AppHandle, workflow: Workflow) -> Result<(), String> {
    let run = claim(state(app).inner(), &workflow.id)?;

    let app = app.clone();
    // Held back for the failure path. The closure takes ownership of both, so a
    // spawn that never happens still needs handles to free the run slot —
    // otherwise a failed spawn leaves the workflow permanently "already
    // running" and it can never be started again.
    let failed_app = app.clone();
    let failed_run = Arc::clone(&run);
    std::thread::Builder::new()
        .name("santi-sharex-workflow".into())
        .spawn(move || {
            // Caught rather than allowed to unwind: the teardown below frees the
            // run slot and takes the overlay, the editor and the HUD off screen.
            // A panic that skipped it would leave a workflow that can never be
            // started again and, quite possibly, a borderless always-on-top
            // window covering the desktop.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                execute(&app, &workflow, &run)
            }))
            .unwrap_or_else(|_| {
                Err(Halt::Failed {
                    step: "workflow".to_string(),
                    message: "the workflow stopped unexpectedly (internal error)".to_string(),
                })
            });

            release(state(&app).inner(), &run);
            report(&app, &workflow, result);
        })
        .map_err(|e| {
            release(state(&failed_app).inner(), &failed_run);
            format!("could not start the workflow: {e}")
        })?;

    Ok(())
}

fn execute(app: &AppHandle, workflow: &Workflow, run: &Arc<Run>) -> Result<Done, Halt> {
    let steps = plan(workflow);
    let mut engine = AppEngine {
        app: app.clone(),
        run: Arc::clone(run),
        workflow: workflow.clone(),
    };
    drive(&mut engine, &steps)
}

/// Says how a run ended, and — on anything but a clean finish — puts the screen
/// back the way it was.
fn report(app: &AppHandle, workflow: &Workflow, result: Result<Done, Halt>) {
    match result {
        Ok(done) => {
            let _ = app.emit(
                DONE_EVENT,
                DonePayload {
                    id: &workflow.id,
                    name: &workflow.name,
                    capture_id: done.capture_id.as_deref(),
                    url: done.url.as_deref(),
                    cancelled: false,
                },
            );
        }
        Err(Halt::Cancelled) => {
            clear_the_screen(app);
            let _ = app.emit(
                DONE_EVENT,
                DonePayload {
                    id: &workflow.id,
                    name: &workflow.name,
                    capture_id: None,
                    url: None,
                    cancelled: true,
                },
            );
        }
        Err(Halt::Failed { step, message }) => {
            clear_the_screen(app);
            eprintln!("santi.sharex: workflow \"{}\" — {step}: {message}", workflow.name);
            let _ = app.emit(
                ERROR_EVENT,
                ErrorPayload {
                    id: &workflow.id,
                    name: &workflow.name,
                    step: &step,
                    message: &message,
                },
            );
        }
    }
}

/// Takes down anything a stopped run could have left up.
///
/// Every call is a no-op when the thing is not on screen, which is why this can
/// run unconditionally on both the cancel and the failure path. Nothing here
/// touches state a *different* feature owns: the overlay, the editor window and
/// the recording are the three the runner itself opened.
fn clear_the_screen(app: &AppHandle) {
    {
        let app_state = app.state::<AppState>();
        crate::lock(&app_state.freeze).take();
    }
    overlay::hide_overlay(app);
    crate::restore_main_after_capture(app);

    if let Some(window) = app.get_webview_window(editor::EDITOR_LABEL) {
        let _ = window.destroy();
    }

    // Discards a recording the workflow started; a no-op when nothing is
    // recording, and it is what takes the HUD down.
    record::request_cancel(app);
    // Stops a scrolling capture mid-run. The flag is cleared by the next run
    // before it starts, so setting it when nothing is running is harmless.
    scroll::cancel_scroll_capture(app.clone());
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressPayload<'a> {
    id: &'a str,
    name: &'a str,
    step: &'a str,
    label: String,
    index: usize,
    total: usize,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DonePayload<'a> {
    id: &'a str,
    name: &'a str,
    capture_id: Option<&'a str>,
    url: Option<&'a str>,
    cancelled: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorPayload<'a> {
    id: &'a str,
    name: &'a str,
    step: &'a str,
    message: &'a str,
}

// ---------------------------------------------------------------------------
// the engine that actually does the work
// ---------------------------------------------------------------------------

struct AppEngine {
    app: AppHandle,
    run: Arc<Run>,
    workflow: Workflow,
}

/// Every step reports its failures the same way: named, so the user is told
/// which link of the chain broke.
fn failed(step: &str, message: impl Into<String>) -> Halt {
    Halt::Failed {
        step: step.to_string(),
        message: message.into(),
    }
}

impl Engine for AppEngine {
    fn cancelled(&self) -> bool {
        self.run.cancel.load(Ordering::SeqCst)
    }

    fn progress(&mut self, step: &Step, index: usize, total: usize) {
        *crate::lock(&self.run.at) = (step.id().to_string(), index, total);
        let _ = self.app.emit(
            PROGRESS_EVENT,
            ProgressPayload {
                id: &self.workflow.id,
                name: &self.workflow.name,
                step: step.id(),
                label: step.label(),
                index,
                total,
            },
        );
    }

    fn capture(&mut self, step: &CaptureStep) -> Result<String, Halt> {
        let label = step.label();
        match step {
            CaptureStep::Region => {
                // The overlay's one other caller is the region hotkey, and this
                // is a second one driving it programmatically — so it goes
                // through exactly the same entry point, arm/ready handshake and
                // Escape ladder rather than round it (M6 §5).
                overlay::start_region_blocking(&self.app).map_err(|e| failed(label, e))?;
                self.await_region(label)
            }
            CaptureStep::Fullscreen => {
                self.grab(label, "fullscreen", capture::capture_virtual_desktop)
            }
            CaptureStep::ActiveWindow => {
                self.grab(label, "window", capture::capture_focused_window)
            }
            CaptureStep::Monitor { id } => {
                let id = *id;
                self.grab(label, "monitor", move || capture::capture_monitor_by_id(id))
            }
            CaptureStep::Window { id } => {
                let id = *id;
                self.grab(label, "window", move || capture::capture_window_by_id(id))
            }
            CaptureStep::Scrolling { window } => {
                let outcome = tauri::async_runtime::block_on(scroll::start_scroll_capture(
                    self.app.clone(),
                    *window,
                ))
                .map_err(|e| failed(label, e))?;
                Ok(outcome.record.id)
            }
            CaptureStep::Record { format } => self.record(label, format),
        }
    }

    fn action(&mut self, step: &ActionStep, capture: &str) -> Result<String, Halt> {
        let label = step.label();
        match step {
            ActionStep::Annotate => self.annotate(label, capture),
            ActionStep::SaveToDisk => {
                let record = self.record_by_id(label, capture)?;
                // Nothing here can write a file that was never kept: a capture
                // taken with "save to disk" off only ever existed in the
                // clipboard and as a 480px thumbnail, so the full-resolution
                // pixels are gone by the time this step runs. Saying so is the
                // only honest answer, and the workflow editor refuses the
                // combination up front (M6 §4).
                if !record.saved || record.path.is_empty() {
                    return Err(failed(
                        label,
                        format!(
                            "\"{}\" was not written to disk, because Settings › Capture has \"Save captures to disk\" turned off.",
                            record.name
                        ),
                    ));
                }
                if !Path::new(&record.path).exists() {
                    return Err(failed(
                        label,
                        format!("{} is no longer on disk", record.name),
                    ));
                }
                Ok(capture.to_string())
            }
            ActionStep::CopyImage => {
                let record = self.record_by_id(label, capture)?;
                let path = self.file_of(label, &record)?;
                let img = image::open(&path)
                    .map_err(|e| failed(label, format!("could not reopen {}: {e}", record.name)))?
                    .to_rgba8();
                crate::copy_rgba_to_clipboard(&self.app, &img).map_err(|e| failed(label, e))?;
                Ok(capture.to_string())
            }
            ActionStep::Ocr { copy_text } => {
                let result = tauri::async_runtime::block_on(ocr::ocr_capture(
                    self.app.clone(),
                    capture.to_string(),
                ))
                .map_err(|e| failed(label, e))?;
                if *copy_text && !result.text.is_empty() {
                    self.app
                        .clipboard()
                        .write_text(result.text.clone())
                        .map_err(|e| {
                            failed(label, format!("could not copy the text: {e}"))
                        })?;
                }
                Ok(capture.to_string())
            }
            ActionStep::OpenFolder => {
                let record = self.record_by_id(label, capture)?;
                // The same fork `finalize` takes for `open_folder_after`: reveal
                // the file when there is one, otherwise open the folder it would
                // have gone in.
                let opened = if record.saved && !record.path.is_empty() {
                    tauri_plugin_opener::reveal_item_in_dir(&record.path)
                } else {
                    let dir = {
                        let app_state = self.app.state::<AppState>();
                        let settings = crate::lock(&app_state.settings);
                        settings.save_dir.clone()
                    };
                    tauri_plugin_opener::open_path(dir, None::<&str>)
                };
                opened.map_err(|e| failed(label, e.to_string()))?;
                Ok(capture.to_string())
            }
        }
    }

    fn upload(&mut self, destination: &str, capture: &str) -> Result<String, Halt> {
        let label = "Upload";
        // M3 §1 binds here, and a workflow must not become a way around it. The
        // workflow names a destination, which is the user's explicit per-run
        // consent — so it does not need `auto_upload` — but it is consent to
        // *that* destination. Uploading somewhere else because Settings says so
        // would make the summary line ("Region → Annotate → Imgur") a lie, and
        // that line is the whole thing standing between a user and a hotkey
        // that sends their screen to the wrong place.
        let settings = {
            let app_state = self.app.state::<AppState>();
            let settings = crate::lock(&app_state.settings);
            settings.clone()
        };
        let statuses = upload::destination_status(self.app.clone());
        let status = statuses
            .iter()
            .find(|s| s.kind == destination)
            .ok_or_else(|| {
                failed(
                    label,
                    format!("\"{destination}\" is not a destination this build can upload to."),
                )
            })?;
        if !status.configured {
            return Err(failed(
                label,
                format!(
                    "{} is not set up yet — finish configuring it in Settings › Destinations.",
                    status.label
                ),
            ));
        }
        if settings.destination != destination {
            return Err(failed(
                label,
                format!(
                    "This workflow uploads to {}, but Settings › Destinations is set to \"{}\". Change one of them so they agree.",
                    status.label, settings.destination
                ),
            ));
        }

        let record = tauri::async_runtime::block_on(upload::upload_capture(
            self.app.clone(),
            capture.to_string(),
        ))
        .map_err(|e| failed(label, e))?;

        record
            .url
            .ok_or_else(|| failed(label, "the destination returned no link"))
    }
}

impl AppEngine {
    /// M1's capture pipeline, unchanged: `grab` hides the preview and the main
    /// window, takes the shot, draws the cursor if it is wanted and hands the
    /// result to `finalize`, which is what honours the save, clipboard, preview
    /// and auto-upload settings.
    fn grab<F>(&self, label: &str, kind: &str, grabber: F) -> Result<String, Halt>
    where
        F: FnOnce() -> Result<capture::Shot, String>,
    {
        crate::grab(&self.app, kind, grabber)
            .map(|record| record.id)
            .map_err(|e| failed(label, e))
    }

    fn record_by_id(&self, label: &str, id: &str) -> Result<CaptureRecord, Halt> {
        store::capture_by_id(&self.app, id).map_err(|e| failed(label, e))
    }

    fn file_of(&self, label: &str, record: &CaptureRecord) -> Result<String, Halt> {
        if record.path.is_empty() {
            return Err(failed(
                label,
                format!("\"{}\" was never written to disk", record.name),
            ));
        }
        if !Path::new(&record.path).exists() {
            return Err(failed(
                label,
                format!("{} is no longer on disk", record.name),
            ));
        }
        Ok(record.path.clone())
    }

    /// Waits out a region selection.
    ///
    /// `start_region_blocking` returns once the overlay is *on screen*, not once
    /// the user has committed, so the wait is here. The freeze frame is the
    /// state that says the overlay is still live: both commit paths take it
    /// before they do any slow work, and only a commit goes on to
    /// [`crate::finalize`] — so "the freeze went and nothing landed" is a
    /// cancel, and it is a clean end to the workflow rather than an error.
    fn await_region(&self, label: &str) -> Result<String, Halt> {
        let state = state(&self.app);
        let mut grace_until: Option<Instant> = None;

        loop {
            if self.cancelled() {
                return Err(Halt::Cancelled);
            }

            {
                let mut signals = crate::lock(&state.signals);
                if let Some(record) = signals.landed.take() {
                    return Ok(record.id);
                }
                if signals.region_cancelled {
                    signals.region_cancelled = false;
                    return Err(Halt::Cancelled);
                }
            }

            let armed = {
                let app_state = self.app.state::<AppState>();
                let frozen = crate::lock(&app_state.freeze).is_some();
                frozen
            };
            if armed {
                grace_until = None;
            } else {
                let deadline = *grace_until.get_or_insert_with(|| Instant::now() + COMMIT_GRACE);
                if Instant::now() >= deadline {
                    return Err(Halt::Cancelled);
                }
            }

            self.sleep_on_signals();
            // The overlay went away without a capture and without a cancel
            // event. Nothing is on screen, so there is nothing to clean up —
            // the loop above decides between "still encoding" and "cancelled".
            let _ = label;
        }
    }

    /// Opens the editor and blocks the chain until it answers.
    ///
    /// The rules this holds, in the order they matter:
    ///
    /// * A **cancel ends the workflow** without uploading. For a redaction
    ///   workflow, carrying on would publish the thing the user was redacting.
    /// * An editor window **closed by its X is a cancel**. It is the likeliest
    ///   way out and the one nothing else would tell us about.
    /// * A **save as new capture** hands the rest of the chain the new id.
    ///   Uploading the original there would upload the un-annotated image.
    /// * An answer for a **different capture** is ignored: it belongs to an
    ///   earlier edit, not to this workflow.
    fn annotate(&self, label: &str, capture: &str) -> Result<String, Halt> {
        {
            // Anything left over from a previous edit is not an answer to this
            // one, and must not be able to resolve it instantly.
            let state = state(&self.app);
            let mut signals = crate::lock(&state.signals);
            signals.editor = None;
            signals.editor_gone = None;
            signals.landed = None;
        }

        editor::open_editor_blocking(&self.app, capture).map_err(|e| failed(label, e))?;

        let state = state(&self.app);
        loop {
            if self.cancelled() {
                return Err(Halt::Cancelled);
            }

            let answer = {
                let mut signals = crate::lock(&state.signals);
                match signals.editor.take() {
                    // An answer about some other capture is not ours. Dropped
                    // rather than kept: the editor only ever holds one document.
                    Some(answer) if answer.id != capture => None,
                    Some(answer) => Some(answer),
                    None => {
                        // The window went away and stayed away. Treated as a
                        // cancel, after a grace long enough for a save that
                        // emitted just before closing to overtake it.
                        match signals.editor_gone {
                            Some(gone) if gone.elapsed() >= EDITOR_CLOSE_GRACE => {
                                signals.editor_gone = None;
                                return Err(Halt::Cancelled);
                            }
                            _ => None,
                        }
                    }
                }
            };

            if let Some(answer) = answer {
                if !answer.saved {
                    return Err(Halt::Cancelled);
                }
                // `newId` when the editor said so; otherwise the record
                // `finalize` produced for a "save as new", if there was one;
                // otherwise the same capture, edited in place.
                if let Some(new_id) = answer.new_id {
                    return Ok(new_id);
                }
                let landed = crate::lock(&state.signals).landed.take();
                return Ok(landed.map(|record| record.id).unwrap_or_else(|| capture.to_string()));
            }

            self.sleep_on_signals();
        }
    }

    /// Records, then waits for M4 to say how it ended.
    ///
    /// There is no source in a `Record` step (M6 §1 types it as
    /// `Record { format }`), so it records the primary monitor — the whole
    /// screen, which is what a one-hotkey "record this" means and the only
    /// reading that needs no picker in front of it.
    fn record(&self, label: &str, format: &str) -> Result<String, Halt> {
        let monitors = capture::list_monitors().map_err(|e| failed(label, e))?;
        let monitor = monitors
            .iter()
            .find(|m| m.is_primary)
            .or_else(|| monitors.first())
            .ok_or_else(|| failed(label, "no display to record"))?;

        let spec = record::RecordSpec {
            source: record::RecordSource::Monitor { id: monitor.id },
            format: Some(store::normalize_record_format(format)),
            fps: None,
            capture_cursor: None,
            hw_encode: None,
        };
        tauri::async_runtime::block_on(record::start_recording(self.app.clone(), spec))
            .map_err(|e| failed(label, e))?;

        let state = state(&self.app);
        loop {
            if self.cancelled() {
                return Err(Halt::Cancelled);
            }
            let ended = crate::lock(&state.signals).recorded.take();
            if let Some(signal) = ended {
                if signal.cancelled {
                    return Err(Halt::Cancelled);
                }
                return signal
                    .record
                    .map(|record| record.id)
                    .ok_or_else(|| failed(label, "the recording produced no file"));
            }
            self.sleep_on_signals();
        }
    }

    /// Parks until a signal lands or [`POLL`] elapses. Never longer: a cancel
    /// sets a flag the loops above re-read, and this is what bounds how long it
    /// takes to notice.
    fn sleep_on_signals(&self) {
        let state = state(&self.app);
        let signals = crate::lock(&state.signals);
        let _ = state
            .woke
            .wait_timeout(signals, POLL)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
}

// ---------------------------------------------------------------------------
// hotkeys (M6 §3)
// ---------------------------------------------------------------------------

/// The workflow behind a hotkey row, so Settings › Hotkeys can name it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRef {
    pub id: String,
    pub name: String,
}

/// One workflow's hotkey, ready for `lib.rs`'s registration pass.
pub(crate) struct HotkeyBinding {
    /// `"workflow:<id>"` — the `HotkeyStatus::action` the UI keys the row off.
    pub action_id: String,
    pub accelerator: String,
    /// What [`crate::Action::Workflow`] carries. Rebuilt every pass, so it can
    /// only ever name a workflow that exists right now.
    pub slot: u64,
    pub workflow: WorkflowRef,
}

/// The `action` prefix a workflow hotkey row uses.
pub const HOTKEY_ACTION_PREFIX: &str = "workflow:";

/// Every enabled workflow with a hotkey trigger, and a fresh slot for each.
///
/// Called by `lib.rs` on every registration pass, which is also what unregisters
/// a workflow that was disabled or deleted: the pass clears the plugin's
/// registrations and the hook's table and rebuilds both from this list.
pub(crate) fn hotkey_bindings(app: &AppHandle) -> Vec<HotkeyBinding> {
    let state = state(app);
    let workflows = crate::lock(&state.workflows).clone();

    let mut slots = crate::lock(&state.slots);
    slots.clear();

    let mut bindings = Vec::new();
    for workflow in workflows {
        if !workflow.enabled {
            continue;
        }
        let Trigger::Hotkey { accelerator } = &workflow.trigger else {
            continue;
        };
        let accelerator = accelerator.trim();
        if accelerator.is_empty() {
            continue;
        }

        let slot = state.next_slot.fetch_add(1, Ordering::SeqCst) + 1;
        slots.push((slot, workflow.id.clone()));
        bindings.push(HotkeyBinding {
            action_id: format!("{HOTKEY_ACTION_PREFIX}{}", workflow.id),
            accelerator: accelerator.to_string(),
            slot,
            workflow: WorkflowRef {
                id: workflow.id.clone(),
                name: workflow.name.clone(),
            },
        });
    }
    bindings
}

/// Runs whatever workflow owns `slot`. The hotkey path's entry point.
///
/// A slot that no longer maps to anything is silently ignored: it belongs to a
/// registration pass that has since been replaced, and the key that reached it
/// is one the user just unbound.
pub(crate) fn start_by_slot(app: &AppHandle, slot: u64) -> Result<(), String> {
    let id = {
        let state = state(app);
        let slots = crate::lock(&state.slots);
        slots
            .iter()
            .find(|(candidate, _)| *candidate == slot)
            .map(|(_, id)| id.clone())
    };
    let Some(id) = id else {
        return Ok(());
    };
    let Some(workflow) = by_id(app, &id) else {
        return Ok(());
    };
    start(app, workflow)
}

/// Whether two accelerators name the same physical combo.
///
/// Compared as parsed combos, not as strings: `CmdOrCtrl+Shift+1` and
/// `Ctrl+Shift+1` are the same key press, and a conflict check that missed that
/// would let a workflow take a built-in's hotkey by spelling it differently.
/// Strings that neither parse fall back to a case-insensitive comparison, which
/// is the best available answer for something the hook could not bind either.
pub(crate) fn same_accelerator(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim(), b.trim());
    if a.is_empty() || b.is_empty() {
        return false;
    }
    match (hotkeys::parse(a), hotkeys::parse(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a.eq_ignore_ascii_case(b),
    }
}

/// What already owns `accelerator`, if anything. `claims` is `(label,
/// accelerator)` for everything registered ahead of it — the built-ins first,
/// then the workflows in list order.
pub(crate) fn accelerator_owner(accelerator: &str, claims: &[(String, String)]) -> Option<String> {
    claims
        .iter()
        .find(|(_, claimed)| same_accelerator(accelerator, claimed))
        .map(|(label, _)| label.clone())
}

// ---------------------------------------------------------------------------
// validation (M6 §4, enforced here so the UI cannot be the only guard)
// ---------------------------------------------------------------------------

pub const SEVERITY_ERROR: &str = "error";
pub const SEVERITY_WARNING: &str = "warning";

/// One thing wrong with a workflow, in the words the editor shows.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    /// `"name" | "trigger" | "capture" | "actions" | "destination"`.
    pub field: String,
    /// [`SEVERITY_ERROR`] blocks saving; [`SEVERITY_WARNING`] does not.
    pub severity: String,
    pub message: String,
}

fn error(field: &str, message: impl Into<String>) -> Issue {
    Issue {
        field: field.to_string(),
        severity: SEVERITY_ERROR.to_string(),
        message: message.into(),
    }
}

fn warning(field: &str, message: impl Into<String>) -> Issue {
    Issue {
        field: field.to_string(),
        severity: SEVERITY_WARNING.to_string(),
        message: message.into(),
    }
}

/// Everything validation needs to know about the rest of the app, resolved once
/// so the rules themselves stay pure.
pub(crate) struct Context {
    /// `(label, accelerator)` for every binding that is *not* this workflow.
    pub claims: Vec<(String, String)>,
    /// `Settings::destination`.
    pub active_destination: String,
    /// Destination ids that are ready to upload to.
    pub configured: Vec<String>,
    /// `Settings::save_to_disk`.
    pub save_to_disk: bool,
}

/// The rules. Every one of them is a run-time failure caught at edit time
/// instead (M6 §4).
pub(crate) fn validate_with(workflow: &Workflow, ctx: &Context) -> Vec<Issue> {
    let mut issues = Vec::new();

    if workflow.name.trim().is_empty() {
        issues.push(error("name", "Give the workflow a name."));
    }

    if let Trigger::Hotkey { accelerator } = &workflow.trigger {
        let accelerator = accelerator.trim();
        if accelerator.is_empty() {
            issues.push(error("trigger", "Record a hotkey, or set the trigger to Manual."));
        } else if let Some(owner) = accelerator_owner(accelerator, &ctx.claims) {
            issues.push(error(
                "trigger",
                format!("{accelerator} is already used by {owner}."),
            ));
        }
    }

    let recording = matches!(workflow.capture, CaptureStep::Record { .. });
    let mut saved_at: Option<usize> = None;

    for (index, action) in workflow.actions.iter().enumerate() {
        match action {
            // There is no page of text in an MP4 and no canvas to draw on, and
            // the underlying paths refuse both by name (M4 §5) — so the editor
            // says so before the hotkey is ever pressed.
            ActionStep::Ocr { .. } if recording => issues.push(error(
                "actions",
                "Extract text cannot read a recording — there is no text in a video.",
            )),
            ActionStep::Annotate if recording => issues.push(error(
                "actions",
                "Annotate works on still images, not on a recording.",
            )),
            ActionStep::CopyImage if recording => issues.push(error(
                "actions",
                "Copy image works on still images, not on a recording.",
            )),
            ActionStep::SaveToDisk => {
                saved_at = Some(index);
                if !ctx.save_to_disk && !recording {
                    issues.push(warning(
                        "actions",
                        "Settings › Capture has \"Save captures to disk\" off, so there will be no file for this step to keep.",
                    ));
                }
            }
            ActionStep::Annotate => {
                if let Some(saved) = saved_at {
                    if saved < index {
                        issues.push(warning(
                            "actions",
                            "Save to disk runs before Annotate, so the saved file will not have the annotation in it. Put Annotate first.",
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    if let Some(destination) = &workflow.destination {
        let destination = destination.trim();
        if destination.is_empty() || destination == store::DESTINATION_NONE {
            issues.push(error(
                "destination",
                "Pick a destination, or set this workflow not to upload.",
            ));
        } else if !store::DESTINATIONS.contains(&destination) {
            issues.push(error(
                "destination",
                format!("\"{destination}\" is not a destination this build can upload to."),
            ));
        } else if !ctx.configured.iter().any(|id| id == destination) {
            issues.push(error(
                "destination",
                format!("{destination} is not set up yet — finish configuring it in Settings › Destinations."),
            ));
        } else if ctx.active_destination != destination {
            issues.push(error(
                "destination",
                format!(
                    "Settings › Destinations is set to \"{}\", so this workflow cannot upload to {destination}.",
                    ctx.active_destination
                ),
            ));
        }
    }

    issues
}

/// Resolves a [`Context`] out of the live app.
fn context(app: &AppHandle, exclude: &str) -> Context {
    let settings = {
        let app_state = app.state::<AppState>();
        let settings = crate::lock(&app_state.settings);
        settings.clone()
    };

    let mut claims: Vec<(String, String)> = crate::builtin_hotkeys(&settings)
        .into_iter()
        .map(|(label, accelerator)| (label.to_string(), accelerator))
        .collect();
    for workflow in all(app) {
        if workflow.id == exclude || !workflow.enabled {
            continue;
        }
        if let Trigger::Hotkey { accelerator } = &workflow.trigger {
            claims.push((workflow.name.clone(), accelerator.clone()));
        }
    }

    let configured = upload::destination_status(app.clone())
        .into_iter()
        .filter(|status| status.configured)
        .map(|status| status.kind)
        .collect();

    Context {
        claims,
        active_destination: settings.destination,
        configured,
        save_to_disk: settings.save_to_disk,
    }
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_workflows(app: AppHandle) -> Vec<Workflow> {
    all(&app)
}

#[tauri::command]
pub fn validate_workflow(app: AppHandle, workflow: Workflow) -> Vec<Issue> {
    validate_with(&workflow, &context(&app, &workflow.id))
}

/// Creates or replaces one workflow, and re-registers every hotkey.
///
/// A workflow with an error is refused rather than stored: a stored workflow
/// with a conflicting hotkey is a workflow that silently does nothing, and a
/// stored workflow with an unconfigured destination is one that fails at the
/// moment it is used instead of the moment it is written (M6 §2).
#[tauri::command]
pub fn save_workflow(app: AppHandle, mut workflow: Workflow) -> Result<Vec<Workflow>, String> {
    if workflow.id.trim().is_empty() {
        workflow.id = store::next_id();
    }
    workflow.name = workflow.name.trim().to_string();

    let issues = validate_with(&workflow, &context(&app, &workflow.id));
    if let Some(blocker) = issues.iter().find(|i| i.severity == SEVERITY_ERROR) {
        return Err(blocker.message.clone());
    }

    {
        let state = state(&app);
        let mut workflows = crate::lock(&state.workflows);
        match workflows.iter_mut().find(|w| w.id == workflow.id) {
            Some(slot) => *slot = workflow,
            None => workflows.push(workflow),
        }
    }
    commit(&app)
}

#[tauri::command]
pub fn delete_workflow(app: AppHandle, id: String) -> Result<Vec<Workflow>, String> {
    {
        let state = state(&app);
        let mut workflows = crate::lock(&state.workflows);
        workflows.retain(|w| w.id != id);
    }
    // The hotkey goes with it: `commit` re-runs the registration pass, which
    // rebuilds both mechanisms from what is left.
    commit(&app)
}

#[tauri::command]
pub fn set_workflow_enabled(
    app: AppHandle,
    id: String,
    enabled: bool,
) -> Result<Vec<Workflow>, String> {
    {
        let state = state(&app);
        let mut workflows = crate::lock(&state.workflows);
        let slot = workflows
            .iter_mut()
            .find(|w| w.id == id)
            .ok_or_else(|| format!("no workflow with id {id}"))?;
        slot.enabled = enabled;
    }
    commit(&app)
}

/// Run one now. Returns as soon as the run has started; everything after that
/// arrives on [`PROGRESS_EVENT`] and then on [`DONE_EVENT`] or [`ERROR_EVENT`].
#[tauri::command]
pub fn run_workflow(app: AppHandle, id: String) -> Result<(), String> {
    let workflow = by_id(&app, &id).ok_or_else(|| format!("no workflow with id {id}"))?;
    start(&app, workflow)
}

/// Stop the run in flight. Not an error when there is none — the UI can be a
/// moment behind the run that just finished.
#[tauri::command]
pub fn cancel_workflow(app: AppHandle) {
    let state = state(&app);
    let run = crate::lock(&state.run).clone();
    if let Some(run) = run {
        run.cancel.store(true, Ordering::SeqCst);
    }
    // Wakes whichever wait is parked so the cancel is noticed now rather than
    // at the next poll — including a region wait, which would otherwise sit
    // there until the user dismissed the overlay themselves.
    state.woke.notify_all();

    // The overlay, the editor and the HUD come down here rather than only in
    // the runner's teardown: a wait that is inside `start_region_blocking` or
    // the recorder has not reached a cancel check yet, and leaving a
    // full-screen always-on-top window up until it does is the lockout this
    // rule exists to prevent.
    clear_the_screen(&app);
}

/// What is running, for a view that mounted mid-run.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStatus {
    pub running: bool,
    pub id: String,
    pub step: String,
    pub index: usize,
    pub total: usize,
}

#[tauri::command]
pub fn workflow_status(app: AppHandle) -> RunStatus {
    let state = state(&app);
    let run = crate::lock(&state.run).clone();
    match run {
        Some(run) => {
            let (step, index, total) = crate::lock(&run.at).clone();
            RunStatus {
                running: true,
                id: run.workflow_id.clone(),
                step,
                index,
                total,
            }
        }
        None => RunStatus {
            running: false,
            id: String::new(),
            step: String::new(),
            index: 0,
            total: 0,
        },
    }
}

/// Writes the list, re-registers the hotkeys and tells the UI.
fn commit(app: &AppHandle) -> Result<Vec<Workflow>, String> {
    let workflows = all(app);

    // A file that could not be read is put aside before the first write rather
    // than overwritten. It is the user's only copy of workflows this build
    // could not parse, and "your workflows are gone" is a worse outcome than a
    // stray file next to `settings.json`.
    let broken = crate::lock(&state(app).load_error).take();
    if broken.is_some() {
        store::quarantine_workflows(app);
    }

    store::persist_workflows(app, &workflows)?;

    let settings = {
        let app_state = app.state::<AppState>();
        let settings = crate::lock(&app_state.settings);
        settings.clone()
    };
    crate::register_hotkeys(app, &settings);

    let _ = app.emit(CHANGED_EVENT, workflows.clone());
    Ok(workflows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn workflow(actions: Vec<ActionStep>, destination: Option<&str>) -> Workflow {
        Workflow {
            id: "wf-1".into(),
            name: "Redact".into(),
            enabled: true,
            trigger: Trigger::Hotkey {
                accelerator: "CmdOrCtrl+Shift+9".into(),
            },
            capture: CaptureStep::Region,
            actions,
            destination: destination.map(str::to_string),
        }
    }

    /// A recorder standing in for the app. `cancel_at` makes [`Engine::cancelled`]
    /// start answering `true` once that many steps have been *attempted*, which
    /// is how "cancel at every step" is exercised without a screen.
    struct Recorder {
        seen: RefCell<Vec<String>>,
        cancel_at: Option<usize>,
        fail_at: Option<usize>,
        attempts: RefCell<usize>,
    }

    impl Recorder {
        fn new() -> Self {
            Self {
                seen: RefCell::new(Vec::new()),
                cancel_at: None,
                fail_at: None,
                attempts: RefCell::new(0),
            }
        }

        fn cancelling_after(steps: usize) -> Self {
            Self {
                cancel_at: Some(steps),
                ..Self::new()
            }
        }

        fn failing_at(step: usize) -> Self {
            Self {
                fail_at: Some(step),
                ..Self::new()
            }
        }

        fn note(&self, what: &str) -> Result<(), Halt> {
            let mut attempts = self.attempts.borrow_mut();
            if self.fail_at == Some(*attempts) {
                return Err(failed(what, "no"));
            }
            *attempts += 1;
            self.seen.borrow_mut().push(what.to_string());
            Ok(())
        }

        fn steps(&self) -> Vec<String> {
            self.seen.borrow().clone()
        }
    }

    impl Engine for Recorder {
        fn cancelled(&self) -> bool {
            match self.cancel_at {
                Some(n) => *self.attempts.borrow() >= n,
                None => false,
            }
        }

        fn progress(&mut self, _step: &Step, _index: usize, _total: usize) {}

        fn capture(&mut self, _step: &CaptureStep) -> Result<String, Halt> {
            self.note("capture")?;
            Ok("cap-1".to_string())
        }

        fn action(&mut self, step: &ActionStep, capture: &str) -> Result<String, Halt> {
            self.note(step.id())?;
            // The annotate step is the one allowed to change the id, and the
            // rest of the chain has to carry the new one.
            if matches!(step, ActionStep::Annotate) {
                return Ok("cap-2".to_string());
            }
            Ok(capture.to_string())
        }

        fn upload(&mut self, destination: &str, capture: &str) -> Result<String, Halt> {
            self.note("upload")?;
            Ok(format!("https://example.test/{destination}/{capture}"))
        }
    }

    // -- ordering ----------------------------------------------------------

    #[test]
    fn the_plan_is_capture_then_actions_in_order_then_the_destination() {
        let w = workflow(
            vec![
                ActionStep::Annotate,
                ActionStep::SaveToDisk,
                ActionStep::Ocr { copy_text: true },
            ],
            Some("imgur"),
        );

        assert_eq!(
            plan(&w),
            vec![
                Step::Capture(CaptureStep::Region),
                Step::Action(ActionStep::Annotate),
                Step::Action(ActionStep::SaveToDisk),
                Step::Action(ActionStep::Ocr { copy_text: true }),
                Step::Upload("imgur".to_string()),
            ]
        );

        // And a workflow that does not upload has no upload step at all — not a
        // skipped one, which is the difference between "does not send" and
        // "sends when something changes".
        let local = workflow(vec![ActionStep::SaveToDisk], None);
        assert_eq!(plan(&local).len(), 2);
        assert!(!plan(&local).iter().any(|s| matches!(s, Step::Upload(_))));
    }

    #[test]
    fn the_runner_walks_the_plan_in_order() {
        let w = workflow(
            vec![ActionStep::Annotate, ActionStep::CopyImage],
            Some("imgur"),
        );
        let mut engine = Recorder::new();
        let done = drive(&mut engine, &plan(&w)).expect("a clean run");

        assert_eq!(
            engine.steps(),
            vec!["capture", "annotate", "copyImage", "upload"]
        );
        // Annotate saved as a new capture, and the upload followed *that* — the
        // whole point of the step.
        assert_eq!(done.capture_id.as_deref(), Some("cap-2"));
        assert!(done.url.is_some());
    }

    // -- cancelling --------------------------------------------------------

    /// The rule that matters most: whatever step the user cancels on, the chain
    /// stops there and the destination is never reached.
    #[test]
    fn a_cancel_at_any_step_stops_the_chain_before_the_destination() {
        let w = workflow(
            vec![
                ActionStep::Annotate,
                ActionStep::SaveToDisk,
                ActionStep::Ocr { copy_text: false },
            ],
            Some("imgur"),
        );
        let steps = plan(&w);
        let names = ["capture", "annotate", "saveToDisk", "ocr", "upload"];

        for stop in 0..steps.len() {
            let mut engine = Recorder::cancelling_after(stop);
            let result = drive(&mut engine, &steps);

            assert_eq!(
                result,
                Err(Halt::Cancelled),
                "cancelling before {} should stop the run",
                names[stop]
            );
            assert_eq!(
                engine.steps(),
                names[..stop].to_vec(),
                "cancelling before {} ran the wrong steps",
                names[stop]
            );
            assert!(
                !engine.steps().iter().any(|s| s == "upload"),
                "a cancelled workflow uploaded anyway"
            );
        }
    }

    /// A cancelled region selection is the user changing their mind, and ends
    /// the workflow cleanly rather than as an error.
    #[test]
    fn a_cancel_is_not_an_error() {
        let w = workflow(vec![], Some("imgur"));
        let mut engine = Recorder::cancelling_after(0);
        match drive(&mut engine, &plan(&w)) {
            Err(Halt::Cancelled) => {}
            other => panic!("a cancel must not be reported as a failure: {other:?}"),
        }
    }

    /// A failing step names itself and stops the chain — it does not carry on
    /// to the destination with a half-finished result.
    #[test]
    fn a_failing_step_stops_the_chain_and_names_itself() {
        let w = workflow(vec![ActionStep::Annotate, ActionStep::SaveToDisk], Some("ftp"));
        // Fail the second step (the annotation).
        let mut engine = Recorder::failing_at(1);
        let result = drive(&mut engine, &plan(&w));

        match result {
            // The step ID, not its label: the frontend maps it through
            // WORKFLOW_STEP_LABEL (`annotate` -> "Annotate"), so sending the
            // label here would miss the map and render raw.
            Err(Halt::Failed { step, .. }) => assert_eq!(step, "annotate"),
            other => panic!("expected a named failure, got {other:?}"),
        }
        assert_eq!(engine.steps(), vec!["capture"]);
        assert!(!engine.steps().iter().any(|s| s == "upload"));
    }

    // -- one at a time -----------------------------------------------------

    #[test]
    fn a_second_trigger_is_refused_while_one_is_running() {
        let state = WorkflowState::default();

        let first = claim(&state, "wf-1").expect("the first run claims the slot");
        let second = claim(&state, "wf-2");
        assert!(
            second.is_err(),
            "two workflows ran at once — two region overlays is a lockout"
        );
        // The same workflow triggered twice is refused too: the second press is
        // not a request to run it again, it is a press that arrived early.
        assert!(claim(&state, "wf-1").is_err());

        release(&state, &first);
        assert!(
            claim(&state, "wf-2").is_ok(),
            "the slot must free up when a run ends"
        );
    }

    #[test]
    fn releasing_a_run_that_no_longer_owns_the_slot_leaves_it_alone() {
        let state = WorkflowState::default();
        let first = claim(&state, "wf-1").expect("claim");
        release(&state, &first);
        let second = claim(&state, "wf-2").expect("claim");

        // A late teardown from the first run must not free the second's slot.
        release(&state, &first);
        assert!(claim(&state, "wf-3").is_err());
        release(&state, &second);
    }

    // -- hotkeys -----------------------------------------------------------

    #[test]
    fn a_workflow_cannot_take_a_built_in_hotkey_however_it_is_spelled() {
        let claims = vec![
            ("Capture region".to_string(), "CmdOrCtrl+Shift+1".to_string()),
            ("Capture fullscreen".to_string(), "CmdOrCtrl+Shift+2".to_string()),
            ("Stop recording".to_string(), "CmdOrCtrl+Shift+4".to_string()),
        ];

        // The same combo, spelled the way the recorder writes it and the way a
        // hand-edited file might.
        assert_eq!(
            accelerator_owner("Ctrl+Shift+1", &claims).as_deref(),
            Some("Capture region")
        );
        assert_eq!(
            accelerator_owner("CmdOrCtrl+Shift+1", &claims).as_deref(),
            Some("Capture region")
        );
        assert_eq!(
            accelerator_owner("ctrl+SHIFT+1", &claims).as_deref(),
            Some("Capture region")
        );
        // Modifiers must match exactly — a superset is a different combo.
        assert_eq!(accelerator_owner("Ctrl+Alt+Shift+1", &claims), None);
        assert_eq!(accelerator_owner("CmdOrCtrl+Shift+9", &claims), None);
        assert_eq!(accelerator_owner("", &claims), None);
    }

    #[test]
    fn a_conflicting_workflow_hotkey_is_an_edit_time_error_that_names_the_owner() {
        let mut w = workflow(vec![], None);
        w.trigger = Trigger::Hotkey {
            accelerator: "Ctrl+Shift+1".into(),
        };

        let ctx = Context {
            claims: vec![("Capture region".to_string(), "CmdOrCtrl+Shift+1".to_string())],
            active_destination: store::DESTINATION_NONE.to_string(),
            configured: vec![],
            save_to_disk: true,
        };

        let issues = validate_with(&w, &ctx);
        let trigger = issues
            .iter()
            .find(|i| i.field == "trigger")
            .expect("the conflict must be reported");
        assert_eq!(trigger.severity, SEVERITY_ERROR);
        assert!(
            trigger.message.contains("Capture region"),
            "the message must name what already owns it: {}",
            trigger.message
        );
    }

    #[test]
    fn a_free_hotkey_and_a_configured_destination_validate_clean() {
        let mut w = workflow(vec![ActionStep::Annotate], Some("imgur"));
        w.trigger = Trigger::Hotkey {
            accelerator: "CmdOrCtrl+Shift+9".into(),
        };

        let ctx = Context {
            claims: vec![("Capture region".to_string(), "CmdOrCtrl+Shift+1".to_string())],
            active_destination: "imgur".to_string(),
            configured: vec!["imgur".to_string()],
            save_to_disk: true,
        };
        assert!(validate_with(&w, &ctx).is_empty());
    }

    // -- validation --------------------------------------------------------

    #[test]
    fn an_unconfigured_destination_fails_at_edit_time() {
        let w = workflow(vec![], Some("ftp"));
        let ctx = Context {
            claims: vec![],
            active_destination: "ftp".to_string(),
            configured: vec![],
            save_to_disk: true,
        };
        let issues = validate_with(&w, &ctx);
        assert!(issues
            .iter()
            .any(|i| i.field == "destination" && i.severity == SEVERITY_ERROR));
    }

    #[test]
    fn ocr_on_a_recording_and_annotate_after_save_are_caught() {
        let mut w = workflow(
            vec![ActionStep::Ocr { copy_text: true }],
            None,
        );
        w.capture = CaptureStep::Record {
            format: "mp4".into(),
        };
        let ctx = Context {
            claims: vec![],
            active_destination: store::DESTINATION_NONE.to_string(),
            configured: vec![],
            save_to_disk: true,
        };
        assert!(validate_with(&w, &ctx)
            .iter()
            .any(|i| i.field == "actions" && i.severity == SEVERITY_ERROR));

        // Annotate after SaveToDisk is a warning, not a refusal: the saved file
        // just predates the annotation, which is a thing a user may well mean.
        let ordered = workflow(vec![ActionStep::SaveToDisk, ActionStep::Annotate], None);
        let issues = validate_with(&ordered, &ctx);
        assert!(issues
            .iter()
            .any(|i| i.field == "actions" && i.severity == SEVERITY_WARNING));
        assert!(!issues.iter().any(|i| i.severity == SEVERITY_ERROR));
    }

    // -- storage -----------------------------------------------------------

    #[test]
    fn a_workflow_round_trips_through_its_stored_form() {
        let w = workflow(
            vec![
                ActionStep::Annotate,
                ActionStep::Ocr { copy_text: true },
                ActionStep::OpenFolder,
            ],
            Some("imgur"),
        );

        let json = serde_json::to_string(&w).expect("must serialize");
        assert!(json.contains("\"type\":\"annotate\""), "{json}");
        assert!(json.contains("\"copyText\":true"), "{json}");
        assert!(json.contains("\"type\":\"hotkey\""), "{json}");

        let back: Workflow = serde_json::from_str(&json).expect("must round trip");
        assert_eq!(back, w);
    }

    /// A `workflows.json` this build cannot parse must fail as an error the
    /// caller can report — never a panic, and never a silent empty list that
    /// the next write turns into a real one.
    #[test]
    fn a_malformed_file_is_an_error_not_a_panic() {
        assert!(serde_json::from_str::<Vec<Workflow>>("{ not json").is_err());
        assert!(serde_json::from_str::<Vec<Workflow>>(r#"[{"id":"1"}]"#).is_err());
        // An unknown step is the same answer: this build cannot run it, so it
        // must not pretend the workflow is something else.
        assert!(serde_json::from_str::<Vec<Workflow>>(
            r#"[{"id":"1","name":"x","enabled":true,"trigger":{"type":"manual"},
                 "capture":{"type":"teleport"},"actions":[]}]"#
        )
        .is_err());

        // And the empty file — the state a fresh install is in — is a real,
        // empty list.
        assert_eq!(
            serde_json::from_str::<Vec<Workflow>>("[]").unwrap().len(),
            0
        );
    }

    #[test]
    fn a_stored_workflow_without_the_optional_keys_still_loads() {
        let raw = r#"[{
          "id": "wf-1",
          "name": "Quick region",
          "trigger": { "type": "hotkey", "accelerator": "CmdOrCtrl+Shift+9" },
          "capture": { "type": "region" }
        }]"#;
        let workflows: Vec<Workflow> = serde_json::from_str(raw).expect("must parse");
        assert_eq!(workflows.len(), 1);
        // Enabled by default, no actions, and — the one that matters — no
        // destination, so it cannot upload.
        assert!(workflows[0].enabled);
        assert!(workflows[0].actions.is_empty());
        assert_eq!(workflows[0].destination, None);
    }
}
