//! Upload plumbing: the destination trait, the dispatcher, progress, cancel,
//! and the commands the frontend calls (M3 §6).
//!
//! # The one rule
//!
//! **Nothing uploads unless it was asked for.** There are exactly two entry
//! points into a transfer: [`upload_capture`], which the user reaches by
//! pressing Upload, and [`auto_upload_if_enabled`], which does nothing at all
//! unless `Settings::auto_upload` — off by default, and named-defaulted so it
//! cannot arrive `true` from an older `settings.json` — is on *and* a
//! destination has been configured. `Settings::destination` defaults to
//! `"none"`, and an id this build does not recognise normalises to `"none"`
//! rather than to a destination. Every other path in this file needs an
//! explicit capture id to do anything.
//!
//! # Secrets
//!
//! [`secrets::get`] is `pub(in crate::upload)`, so the plaintext of a password
//! or an API key is reachable from this module tree and nowhere else — not from
//! `lib.rs`, and therefore not from any `#[tauri::command]`. What crosses the
//! IPC is [`DestinationStatus`], which carries booleans. See `secrets.rs`.
//!
//! # Off the UI thread, and actually cancellable
//!
//! Every transfer runs on a `spawn_blocking` worker. Cancellation is not a flag
//! that makes the result get ignored: the payload is fed to the destination
//! through [`ProgressReader`], which checks the flag on every chunk and fails
//! the read, which ends the HTTP body or the FTP data connection mid-flight.
//! A 20 MB capture on slow upstream stops when the user says stop.

pub mod custom;
pub mod ftp;
pub mod imgur;
pub mod secrets;

use std::collections::HashMap;
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::store::{self, AppState, CaptureRecord, Settings, DESTINATION_NONE};
use crate::{lock, run_blocking};

use secrets::Secret;

// ---------------------------------------------------------------------------
// events
// ---------------------------------------------------------------------------

/// `{ id, sent, total }` — bytes of the *image* handed to the destination, not
/// bytes on the wire: a multipart envelope is a few hundred bytes of headers the
/// user did not ask about, and reporting them would make the bar arrive at 100%
/// before the request is done.
const PROGRESS_EVENT: &str = "upload://progress";
/// `{ id, url }`.
const DONE_EVENT: &str = "upload://done";
/// `{ id, message, cancelled }`. `cancelled` is an addition to the shape in M3
/// §6, so the UI can take its progress row down without raising a toast that
/// says the thing the user just asked for went wrong.
const ERROR_EVENT: &str = "upload://error";
/// The same event `editor.rs` emits after an in-place save: History replaces the
/// record it already has rather than prepending a new one. Reused deliberately —
/// an upload changes one field of an existing capture, which is exactly what
/// that event means.
const CAPTURE_UPDATED_EVENT: &str = "capture://updated";

/// Progress is emitted at most this often, plus once when the last byte goes
/// out. An 8 KB chunk on a fast link would otherwise put thousands of events a
/// second through the IPC to move a bar by a pixel.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(80);

/// What [`UploadCtx::check_cancel`] and [`ProgressReader`] fail with. Only ever
/// a message: whether a run was cancelled is decided by the flag, never by
/// matching on this string.
const CANCELLED: &str = "Upload cancelled";

// ---------------------------------------------------------------------------
// destinations
// ---------------------------------------------------------------------------

/// What a destination hands back. `url` is what gets copied and stored.
#[derive(Debug, Clone)]
pub struct UploadResult {
    pub url: String,
    pub deletion_url: Option<String>,
    /// A `.sxcu` may define a `ThumbnailURL` template, and M3 §6 names this
    /// field, so destinations that have one report it. Nothing consumes it yet:
    /// History shows the local thumbnail it already has, and swapping that for a
    /// remote image would put a network fetch behind every gallery row.
    /// `CaptureRecord` therefore has no column for it, and this stays the
    /// destination-side truth rather than a second link nobody asked for.
    #[allow(dead_code)]
    pub thumbnail_url: Option<String>,
}

/// One upload destination.
///
/// Implementors live beside this file — `imgur.rs`, `custom.rs`, `ftp.rs` — and
/// are the only things that touch the network. Both methods are synchronous and
/// are always called on a blocking worker.
pub trait Destination: Send {
    /// Send `bytes` (a PNG) under the file name `name`.
    ///
    /// Report progress and honour cancellation by writing the payload through
    /// `ctx.reader()` rather than handing `bytes` straight to the client; a
    /// destination that cannot stream must at least call `ctx.check_cancel()`
    /// before it starts and `ctx.progress(len, len)` when it finishes.
    fn upload(&self, ctx: &UploadCtx, bytes: &[u8], name: &str) -> Result<UploadResult, String>;

    /// Prove the configuration works, for Settings' "Test connection".
    ///
    /// Returns one line to show the user on success ("Connected to
    /// ftp.example.com as santi"), and the *real* error on failure — M3 §6 asks
    /// for the real error, not "test failed". No implementation may leave
    /// anything behind on the remote host.
    fn test(&self, ctx: &UploadCtx) -> Result<String, String>;
}

/// The destinations this build can dispatch to. Parsed from
/// `Settings::destination` and from the `kind` argument of the commands, both of
/// which are plain strings on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestinationKind {
    Imgur,
    Custom,
    Ftp,
}

impl DestinationKind {
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "imgur" => Some(Self::Imgur),
            "custom" => Some(Self::Custom),
            "ftp" => Some(Self::Ftp),
            _ => None,
        }
    }

    /// Also the credential-store namespace for this destination (`secrets.rs`)
    /// and the value `Settings::destination` holds. These three strings are the
    /// single spelling of each id in the Rust half — `store::DESTINATIONS`
    /// mirrors them, and `tests::every_settings_destination_can_be_dispatched`
    /// fails if the two lists ever disagree.
    pub fn id(self) -> &'static str {
        match self {
            Self::Imgur => "imgur",
            Self::Custom => custom::KIND,
            Self::Ftp => "ftp",
        }
    }

    /// For messages the user reads.
    pub fn label(self) -> &'static str {
        match self {
            Self::Imgur => "Imgur",
            Self::Custom => "Custom uploader",
            Self::Ftp => "FTP",
        }
    }
}

/// Builds the destination for `kind` out of the *non-secret* configuration.
///
/// Secrets are deliberately not read here: they are fetched inside `upload` /
/// `test` through `ctx.secret(...)`, so a plaintext credential is never held
/// across the construction of an unrelated object, and a destination that is
/// merely being described (Settings, `destination_status`) never reads one at
/// all.
///
/// Every destination is built the same way — `from_settings` reads the stored,
/// non-secret configuration and *re-validates* it, because `settings.json` is a
/// text file a user can edit and a bad value should be a sentence here rather
/// than a mystery half-way through a transfer.
fn destination_for(
    kind: DestinationKind,
    settings: &Settings,
) -> Result<Box<dyn Destination>, String> {
    match kind {
        DestinationKind::Imgur => Ok(Box::new(imgur::Imgur::from_settings(settings)?)),
        DestinationKind::Custom => Ok(Box::new(custom::CustomUploader::from_settings(settings)?)),
        DestinationKind::Ftp => Ok(Box::new(ftp::Ftp::from_settings(settings)?)),
    }
}

// ---------------------------------------------------------------------------
// shared HTTP client
// ---------------------------------------------------------------------------

/// The one place an HTTP client is configured, so Imgur and the custom uploader
/// cannot drift apart on timeouts or user agent.
///
/// Built per upload rather than cached: `reqwest::blocking` runs a Tokio runtime
/// on a thread of its own, and a santi.sharex that has uploaded once should not keep
/// one parked for the rest of the session. Uploads are seconds apart at worst.
///
/// **`timeout(None)` is deliberate.** reqwest's blocking client defaults to a
/// 30-second cap on the *whole* request, body included, which would kill a 20 MB
/// capture on slow upstream at exactly the moment it was working. The connect
/// phase still has a bound, and the user's escape hatch is Cancel — which
/// actually aborts the transfer, rather than a timeout that decides for them.
pub(in crate::upload) fn http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("santi.sharex/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(20))
        .timeout(None)
        .build()
        .map_err(|e| format!("could not start the HTTP client: {e}"))
}

// ---------------------------------------------------------------------------
// context
// ---------------------------------------------------------------------------

/// Everything a destination is allowed to reach: the payload, the cancel flag,
/// the progress channel and its own secrets. Not the settings — the destination
/// took what it needed from those in `from_settings` — and not the history.
pub struct UploadCtx {
    app: AppHandle,
    /// The capture id, or `test:<kind>` for a connection test. It is the key the
    /// UI matches progress and completion against, and the key
    /// [`cancel_upload`] cancels by.
    id: String,
    kind: DestinationKind,
    cancel: Arc<AtomicBool>,
    data: Arc<Vec<u8>>,
}

impl UploadCtx {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// `Err` once the user has cancelled. Call it before anything expensive —
    /// opening a connection, encoding a body — so a cancel that lands between
    /// steps still stops the run.
    pub fn check_cancel(&self) -> Result<(), String> {
        if self.cancelled() {
            Err(CANCELLED.to_string())
        } else {
            Ok(())
        }
    }

    /// Emit one progress tick. Prefer [`UploadCtx::reader`], which does this as
    /// the bytes actually go out; this is for destinations that can only report
    /// at boundaries.
    pub fn progress(&self, sent: u64, total: u64) {
        emit_progress(&self.app, &self.id, sent, total);
    }

    /// The payload, as a `Read` that reports progress and fails the moment the
    /// user cancels. `'static` and `Send`, so it can go straight into
    /// `reqwest::blocking::multipart::Part::reader_with_length`,
    /// `reqwest::blocking::Body::sized` or `suppaftp`'s `put_file`.
    ///
    /// Feeding a destination this rather than the raw `bytes` is what makes a
    /// cancel *abort the transfer* instead of discarding its result.
    pub fn reader(&self) -> ProgressReader {
        ProgressReader {
            data: Arc::clone(&self.data),
            pos: 0,
            app: self.app.clone(),
            id: self.id.clone(),
            cancel: Arc::clone(&self.cancel),
            total: self.data.len() as u64,
            last_emit: Instant::now(),
        }
    }

    /// This destination's secret, or a message telling the user where to set it.
    ///
    /// `pub(in crate::upload)`, like everything else that can see a plaintext
    /// credential: the destinations are inside this module tree, `lib.rs` is
    /// not, and so no `#[tauri::command]` can reach it. Hold the returned
    /// [`Secret`] only as long as the request that needs it — it zeroes itself
    /// on drop.
    pub(in crate::upload) fn secret(&self, field: &str) -> Result<Secret, String> {
        self.optional_secret(field)?.ok_or_else(|| {
            format!(
                "{} has no {field} saved — add it in Settings › Destinations.",
                self.kind.label()
            )
        })
    }

    /// For fields that are genuinely optional — an anonymous FTP login, a
    /// `.sxcu` with no authorization header.
    pub(in crate::upload) fn optional_secret(&self, field: &str) -> Result<Option<Secret>, String> {
        secrets::get(self.kind.id(), field)
    }
}

/// The payload, streamed. See [`UploadCtx::reader`].
pub struct ProgressReader {
    data: Arc<Vec<u8>>,
    pos: usize,
    app: AppHandle,
    id: String,
    cancel: Arc<AtomicBool>,
    total: u64,
    last_emit: Instant,
}

impl Read for ProgressReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::SeqCst) {
            // `io::Error::other`, never `ErrorKind::Interrupted`: `io::copy` and
            // several client internals *retry* an interrupted read, so a cancel
            // raised that way can be swallowed and the upload finish anyway.
            // This is the line that makes cancel abort the transfer rather than
            // discard its result.
            return Err(io::Error::other(CANCELLED));
        }

        let remaining = self.data.len().saturating_sub(self.pos);
        if remaining == 0 || buf.is_empty() {
            return Ok(0);
        }

        let n = remaining.min(buf.len());
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;

        let sent = self.pos as u64;
        if sent >= self.total || self.last_emit.elapsed() >= PROGRESS_INTERVAL {
            self.last_emit = Instant::now();
            emit_progress(&self.app, &self.id, sent, self.total);
        }
        Ok(n)
    }
}

// ---------------------------------------------------------------------------
// wire payloads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressPayload<'a> {
    id: &'a str,
    sent: u64,
    total: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DonePayload<'a> {
    id: &'a str,
    url: &'a str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorPayload<'a> {
    id: &'a str,
    message: &'a str,
    cancelled: bool,
}

/// Whether a destination's secrets are set — **booleans, never values** (M3 §2).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretStatus {
    /// The field id to pass back to `set_destination_secret` /
    /// `clear_destination_secret`.
    pub field: String,
    /// What to call it on screen.
    pub label: String,
    /// Whether an upload to this destination fails without it.
    pub required: bool,
    /// Whether one is stored. This is the whole of what the frontend learns.
    pub set: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationStatus {
    pub kind: String,
    pub label: String,
    /// Everything this destination needs is present, so an upload has a chance
    /// of working.
    pub configured: bool,
    /// This is the destination uploads currently go to.
    pub active: bool,
    pub secrets: Vec<SecretStatus>,
}

fn emit_progress(app: &AppHandle, id: &str, sent: u64, total: u64) {
    let _ = app.emit(PROGRESS_EVENT, ProgressPayload { id, sent, total });
}

fn emit_error(app: &AppHandle, id: &str, message: &str, cancelled: bool) {
    let _ = app.emit(
        ERROR_EVENT,
        ErrorPayload {
            id,
            message,
            cancelled,
        },
    );
}

// ---------------------------------------------------------------------------
// in-flight registry
// ---------------------------------------------------------------------------

/// One cancel flag per upload in flight, keyed by the id the UI knows.
/// Registered in `lib.rs` with `app.manage`.
#[derive(Default)]
pub struct UploadState {
    in_flight: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

/// Removes the id from the registry on every exit path, including a panic
/// unwinding out of a destination. A leaked entry would make that capture
/// permanently un-uploadable until a restart.
struct InFlightGuard<'a> {
    state: &'a UploadState,
    id: String,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        lock(&self.state.in_flight).remove(&self.id);
    }
}

fn begin<'a>(state: &'a UploadState, id: &str) -> Result<(Arc<AtomicBool>, InFlightGuard<'a>), String> {
    let mut in_flight = lock(&state.in_flight);
    if in_flight.contains_key(id) {
        return Err("that capture is already uploading".to_string());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    in_flight.insert(id.to_string(), Arc::clone(&cancel));
    drop(in_flight);

    Ok((
        cancel,
        InFlightGuard {
            state,
            id: id.to_string(),
        },
    ))
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

/// Upload one capture to the configured destination. **The explicit path** — the
/// Upload action on a card, in the lightbox, or on the capture preview.
///
/// Returns the updated record so the caller can show the link without waiting
/// for an event; History also gets `capture://updated`.
#[tauri::command]
pub async fn upload_capture(app: AppHandle, id: String) -> Result<CaptureRecord, String> {
    run_blocking(move || upload_blocking(&app, &id)).await
}

/// Stop the upload with this id. Prompt: the payload reader checks the flag on
/// every chunk, so the transfer ends within one buffer's worth of bytes.
///
/// An id that is not uploading is not an error — the UI may well be a moment
/// behind the transfer that just finished.
#[tauri::command]
pub fn cancel_upload(app: AppHandle, id: String) {
    let state = app.state::<UploadState>();
    let in_flight = lock(&state.in_flight);
    if let Some(cancel) = in_flight.get(&id) {
        cancel.store(true, Ordering::SeqCst);
    }
}

/// Settings' "Test connection". Reports the destination's real error.
#[tauri::command]
pub async fn test_destination(app: AppHandle, kind: String) -> Result<String, String> {
    run_blocking(move || {
        let kind = DestinationKind::from_id(&kind)
            .ok_or_else(|| format!("\"{kind}\" is not a destination santi.sharex knows about"))?;

        let settings = settings_snapshot(&app);
        let destination = destination_for(kind, &settings)?;

        let state = app.state::<UploadState>();
        let id = format!("test:{}", kind.id());
        let (cancel, _guard) = begin(state.inner(), &id)?;

        let ctx = UploadCtx {
            app: app.clone(),
            id,
            kind,
            cancel,
            // A test has no payload: `Destination::test` must not leave anything
            // on the far end, so there is nothing for `ctx.reader()` to stream.
            data: Arc::new(Vec::new()),
        };
        destination.test(&ctx)
    })
    .await
}

/// Store one secret for a destination (M3 §2). Write-only by design: there is no
/// command that reads one back.
///
/// A blank value *clears* the credential rather than storing an empty one — that
/// is what a user emptying the field in Settings means, and an empty stored
/// secret would present as configured and then fail at upload.
#[tauri::command]
pub fn set_destination_secret(kind: String, field: String, value: String) -> Result<(), String> {
    let kind = require_kind(&kind)?;
    let field = require_field(&field)?;

    // `trim`, because a key pasted out of a web page arrives with a newline on
    // it more often than not, and an `Authorization` header with a trailing
    // newline fails in a way nobody has ever debugged quickly.
    let value = value.trim();
    if value.is_empty() {
        return secrets::clear(kind.id(), &field);
    }
    secrets::set(kind.id(), &field, value)
}

#[tauri::command]
pub fn clear_destination_secret(kind: String, field: String) -> Result<(), String> {
    let kind = require_kind(&kind)?;
    let field = require_field(&field)?;
    secrets::clear(kind.id(), &field)
}

/// Import a ShareX `.sxcu` file (M3 §4).
///
/// `custom::import` is the validator — it refuses anything outside the supported
/// subset, naming the field — and hands back the configuration to store plus the
/// header values that turned out to be credentials. This is the caller it means:
/// the config goes into `settings.json`, the credentials go into Windows
/// Credential Manager, and the two never swap places.
///
/// Importing does **not** switch the active destination. Choosing where captures
/// go is the user's decision (M3 §1), and a file drop is not that decision.
#[tauri::command]
pub async fn import_custom_uploader(app: AppHandle, contents: String) -> Result<Settings, String> {
    run_blocking(move || {
        let mut settings = settings_snapshot(&app);

        // `install` validates, writes the header credentials to the credential
        // store and removes the previous uploader's — it is handed the config
        // being replaced for exactly that reason. It returns only the
        // non-secret half, which is the only half that is persisted here.
        settings.custom_uploader = custom::install(&contents, &settings.custom_uploader)?;

        store::persist_settings(&app, &settings)?;
        {
            let state = app.state::<AppState>();
            *lock(&state.settings) = settings.clone();
        }
        let _ = app.emit("settings://changed", settings.clone());
        Ok(settings)
    })
    .await
}

/// Which destinations are set up, and which of their secrets exist — as
/// booleans. The only thing the frontend ever learns about a stored credential.
#[tauri::command]
pub fn destination_status(app: AppHandle) -> Vec<DestinationStatus> {
    let settings = settings_snapshot(&app);
    [
        DestinationKind::Imgur,
        DestinationKind::Custom,
        DestinationKind::Ftp,
    ]
    .into_iter()
    .map(|kind| status_for(kind, &settings))
    .collect()
}

fn require_kind(kind: &str) -> Result<DestinationKind, String> {
    DestinationKind::from_id(kind)
        .ok_or_else(|| format!("\"{kind}\" is not a destination santi.sharex knows about"))
}

/// The field name is part of a credential key, so an empty one would address a
/// slot shared with every other empty one.
fn require_field(field: &str) -> Result<String, String> {
    let field = field.trim();
    if field.is_empty() {
        return Err("a credential needs a field name".to_string());
    }
    Ok(field.to_string())
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

/// Which secrets a destination has, and whether an upload fails without each.
///
/// The custom uploader's list comes from the imported `.sxcu`: header names the
/// import decided were credentials, whose values went to the credential store
/// (M3 §4). The names are not secret — they are what Settings labels its
/// write-only fields with.
fn secret_fields(kind: DestinationKind, settings: &Settings) -> Vec<(String, String, bool)> {
    match kind {
        DestinationKind::Imgur => vec![(
            imgur::CLIENT_ID_FIELD.to_string(),
            "Client ID".to_string(),
            true,
        )],
        // Anonymous FTP is a real configuration, so a missing password is not by
        // itself a broken destination.
        DestinationKind::Ftp => vec![(
            ftp::PASSWORD_FIELD.to_string(),
            "Password".to_string(),
            false,
        )],
        // The field *is* the header name — the same key `CustomUploader` reads
        // back through `ctx.secret`, and the same one `custom::install` writes
        // at import. `secrets.rs` folds its case, so `X-Api-Key` and `x-api-key`
        // cannot become two credentials.
        DestinationKind::Custom => settings
            .custom_uploader
            .secret_headers
            .iter()
            .map(|header| (header.clone(), header.clone(), true))
            .collect(),
    }
}

fn status_for(kind: DestinationKind, settings: &Settings) -> DestinationStatus {
    let secrets: Vec<SecretStatus> = secret_fields(kind, settings)
        .into_iter()
        .map(|(field, label, required)| {
            let set = secrets::exists(kind.id(), &field);
            SecretStatus {
                field,
                label,
                required,
                set,
            }
        })
        .collect();

    let required_secrets_present = secrets.iter().all(|s| !s.required || s.set);
    let config_present = match kind {
        // Imgur's only configuration is its client ID, which is a secret.
        DestinationKind::Imgur => true,
        DestinationKind::Custom => !settings.custom_uploader.request_url.trim().is_empty(),
        DestinationKind::Ftp => !settings.ftp.host.trim().is_empty(),
    };

    DestinationStatus {
        kind: kind.id().to_string(),
        label: kind.label().to_string(),
        configured: config_present && required_secrets_present,
        active: settings.destination == kind.id(),
        secrets,
    }
}

// ---------------------------------------------------------------------------
// the transfer
// ---------------------------------------------------------------------------

fn settings_snapshot(app: &AppHandle) -> Settings {
    let state = app.state::<AppState>();
    let settings = lock(&state.settings).clone();
    settings
}

/// The whole upload, start to finish, on a blocking worker.
///
/// Registration comes first so the cancel flag outlives the transfer that used
/// it: a cancel landing while a destination unwinds turns whatever error it
/// produced ("connection reset by peer", "body write aborted") into the one the
/// user is expecting. That decision is made from the flag, never by matching on
/// the message, so a destination that words its own errors still reads right.
///
/// "Already uploading" is the one failure that does *not* emit `upload://error`:
/// nothing started, and an error event carrying an id that is mid-upload would
/// take down the progress row of the transfer that is still running.
fn upload_blocking(app: &AppHandle, id: &str) -> Result<CaptureRecord, String> {
    let state = app.state::<UploadState>();
    let (cancel, _guard) = begin(state.inner(), id)?;

    match upload_inner(app, id, &cancel) {
        Ok(record) => Ok(record),
        Err(e) => {
            // One place emits the failure, so no path can fail silently and no
            // path can report twice.
            let cancelled = cancel.load(Ordering::SeqCst);
            let message = if cancelled { CANCELLED.to_string() } else { e };
            emit_error(app, id, &message, cancelled);
            Err(message)
        }
    }
}

fn upload_inner(
    app: &AppHandle,
    id: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<CaptureRecord, String> {
    let settings = settings_snapshot(app);
    if settings.destination == DESTINATION_NONE {
        return Err(
            "No upload destination is set up yet — choose one in Settings › Destinations."
                .to_string(),
        );
    }
    let kind = DestinationKind::from_id(&settings.destination).ok_or_else(|| {
        format!(
            "\"{}\" is not a destination this build can upload to",
            settings.destination
        )
    })?;

    let record = store::capture_by_id(app, id)?;
    // Uploading needs the full-resolution file. A capture taken with "save to
    // disk" off only ever existed in the clipboard and as a 480px thumbnail —
    // the same wall `get_capture_image` hits, and worth the same sentence.
    if record.path.is_empty() {
        return Err(format!(
            "\"{}\" was never written to disk, so there is nothing to upload",
            record.name
        ));
    }
    if !Path::new(&record.path).exists() {
        return Err(format!("{} is no longer on disk", record.name));
    }
    let bytes = std::fs::read(&record.path)
        .map_err(|e| format!("could not read {}: {e}", record.name))?;

    let destination = destination_for(kind, &settings)?;
    let total = bytes.len() as u64;
    let ctx = UploadCtx {
        app: app.clone(),
        id: id.to_string(),
        kind,
        cancel: Arc::clone(cancel),
        data: Arc::new(bytes),
    };

    // A zero-byte tick first, so the UI can put a determinate bar up before the
    // connection is even open.
    ctx.progress(0, total);
    ctx.check_cancel()?;

    let payload = Arc::clone(&ctx.data);
    let result = destination.upload(&ctx, payload.as_slice(), &record.name)?;

    // Deliberately **no** `check_cancel` here. A destination returns `Ok` only
    // once the whole body went out and the far end accepted it, so a cancel that
    // lands in the gap between the last byte and this line did not stop
    // anything: the file is on the server either way. Failing the run at this
    // point would report "Upload cancelled", store no link and copy nothing —
    // leaving an orphan the user has no address for and no deletion link to,
    // which is exactly the "cancel discards the result" behaviour M3 §6 rules
    // out. Cancel aborts the *transfer* (see `ProgressReader`); a cancel that
    // lost the race is an upload that happened, and it is reported as one.
    finish(app, record, &result, settings.copy_url_after_upload)
}

/// Everything that happens once the bytes are up: the record gains its link, the
/// link goes on the clipboard, History is told.
fn finish(
    app: &AppHandle,
    mut record: CaptureRecord,
    result: &UploadResult,
    copy_url: bool,
) -> Result<CaptureRecord, String> {
    record.url = Some(result.url.clone());
    record.deletion_url = result.deletion_url.clone();

    {
        let state = app.state::<AppState>();
        let mut history = lock(&state.history);
        // The record may have been deleted while the upload was in flight. That
        // is not a failure of the upload — the file is on the server either way
        // — so the link is still reported and copied, it just has nowhere to be
        // stored.
        if let Some(slot) = history.iter_mut().find(|r| r.id == record.id) {
            *slot = record.clone();
            store::persist_history(app, &mut history)?;
        }
    }

    if copy_url {
        if let Err(e) = app.clipboard().write_text(result.url.clone()) {
            // The upload worked; a clipboard that would not take the link is
            // worth saying out loud but must not turn success into failure.
            let _ = app.emit(
                "capture://error",
                format!("Uploaded, but the link could not be copied: {e}"),
            );
        }
    }

    let _ = app.emit(CAPTURE_UPDATED_EVENT, record.clone());
    let _ = app.emit(
        DONE_EVENT,
        DonePayload {
            id: &record.id,
            url: &result.url,
        },
    );
    Ok(record)
}

// ---------------------------------------------------------------------------
// auto-upload
// ---------------------------------------------------------------------------

/// Whether the one implicit upload path in the app is allowed to fire, as a
/// pure function of the settings and the record (M3 §1).
///
/// Split out of [`auto_upload_if_enabled`] deliberately. This is the single most
/// consequential predicate in santi.sharex — it is what decides, with nobody
/// watching, whether a capture leaves the machine — and inside a function that
/// needs an `AppHandle` and a Tokio runtime it could only ever be *reviewed*.
/// Here `tests::auto_upload_needs_the_switch_and_a_destination_and_a_file` can
/// pin every way it must answer `false`, which is the answer that matters.
///
/// Three conditions, all required:
///
/// * `auto_upload` is on. Off by default, named-defaulted, and reachable only
///   through Settings' confirmation.
/// * A destination is set. `Settings::destination` is `"none"` on a fresh
///   install, and `store::normalize_destination` turns an id this build does not
///   recognise into `"none"` rather than into some destination.
/// * The capture is on disk. With `save_to_disk` off a capture only ever existed
///   in the clipboard and as a 480px thumbnail, and no destination wants that.
fn should_auto_upload(settings: &Settings, record: &CaptureRecord) -> bool {
    settings.auto_upload && settings.destination != DESTINATION_NONE && !record.path.is_empty()
}

/// The only implicit upload path there is, and it does nothing at all unless the
/// user turned it on *and* set a destination up (M3 §1) — see
/// [`should_auto_upload`], which is that decision on its own so it can be
/// tested rather than trusted.
///
/// Called at the end of `finalize`, fire-and-forget on a worker of its own: a
/// capture must not wait on a network round trip, and an upload that fails must
/// not fail the capture. Failures go out on `upload://error` like every other
/// upload's.
pub(crate) fn auto_upload_if_enabled(app: &AppHandle, record: &CaptureRecord) {
    if !should_auto_upload(&settings_snapshot(app), record) {
        return;
    }

    let app = app.clone();
    let id = record.id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _ = upload_blocking(&app, &id);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_ids_round_trip() {
        for kind in [
            DestinationKind::Imgur,
            DestinationKind::Custom,
            DestinationKind::Ftp,
        ] {
            assert_eq!(DestinationKind::from_id(kind.id()), Some(kind));
        }
        assert_eq!(DestinationKind::from_id("none"), None);
        assert_eq!(DestinationKind::from_id("sftp"), None);
    }

    /// `store::DESTINATIONS` is what normalises `settings.json`; this enum is
    /// what dispatches. A destination in one and not the other is either a
    /// setting that can never be used or a dispatch that can never be reached.
    #[test]
    fn every_settings_destination_can_be_dispatched() {
        for id in store::DESTINATIONS {
            assert!(
                DestinationKind::from_id(id).is_some(),
                "{id} is a valid setting but cannot be dispatched"
            );
        }
        assert_eq!(DestinationKind::from_id(store::DESTINATION_NONE), None);
    }

    /// The status that crosses the IPC must never contain a value — only
    /// whether one is set.
    #[test]
    fn destination_status_carries_booleans_only() {
        let settings = Settings::default();
        let status = status_for(DestinationKind::Imgur, &settings);
        let json = serde_json::to_string(&status).expect("must serialize");
        assert!(json.contains("\"set\""));
        assert!(!json.contains("\"value\""));

        // A default install has nothing configured and nothing active, which is
        // the state in which nothing can leave the machine.
        assert!(!status.active);
        assert_eq!(settings.destination, store::DESTINATION_NONE);
    }

    /// The custom uploader's secret fields are whatever the import decided were
    /// credentials — names only, and every one of them required.
    #[test]
    fn custom_uploader_secret_fields_come_from_the_imported_file() {
        let mut settings = Settings::default();
        settings.custom_uploader.secret_headers = vec!["Authorization".into(), "X-Api-Key".into()];

        let fields = secret_fields(DestinationKind::Custom, &settings);
        assert_eq!(fields.len(), 2);
        // The field is the credential key the uploader reads back — for a
        // custom uploader that is the header name itself.
        assert_eq!(fields[0].0, "Authorization");
        assert_eq!(fields[0].1, "Authorization");
        assert!(fields.iter().all(|(_, _, required)| *required));
    }

    fn saved_record() -> CaptureRecord {
        CaptureRecord {
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
            url: None,
            deletion_url: None,
        }
    }

    /// **M3 §1, and the most important test in this file.**
    ///
    /// The only path that can put a capture on the network without the user
    /// pressing anything. Every way it must answer "no" is pinned here, because
    /// each one is a way a capture could leave the machine unasked:
    ///
    /// * a default install, where the switch is off *and* there is no
    ///   destination — the state every existing `settings.json` loads into;
    /// * the switch on but no destination, which is what a user who turned it on
    ///   before configuring anything is in;
    /// * a destination chosen but the switch off, which is the normal
    ///   configured state and by far the most common one;
    /// * a capture that was never written to disk, which has nothing to send.
    ///
    /// The single `true` case needs all three conditions at once.
    #[test]
    fn auto_upload_needs_the_switch_and_a_destination_and_a_file() {
        let record = saved_record();

        // A fresh install, and an existing settings.json, which loads to this.
        let fresh = Settings::default();
        assert!(!fresh.auto_upload);
        assert_eq!(fresh.destination, DESTINATION_NONE);
        assert!(!should_auto_upload(&fresh, &record));

        // The switch on, with nowhere to send anything.
        let mut armed_but_nowhere = Settings::default();
        armed_but_nowhere.auto_upload = true;
        assert!(!should_auto_upload(&armed_but_nowhere, &record));

        // A destination set up, with the switch off — the ordinary state of a
        // configured install, and the one that must stay silent.
        let mut configured = Settings::default();
        configured.destination = DestinationKind::Imgur.id().to_string();
        assert!(!should_auto_upload(&configured, &record));

        // Both, but the capture only ever existed in the clipboard.
        let mut on = Settings::default();
        on.auto_upload = true;
        on.destination = DestinationKind::Imgur.id().to_string();
        let mut unsaved = saved_record();
        unsaved.path = String::new();
        unsaved.saved = false;
        assert!(!should_auto_upload(&on, &unsaved));

        // All three, which is the only way through.
        assert!(should_auto_upload(&on, &record));
    }

    /// A destination id this build cannot dispatch normalises to `"none"` before
    /// it ever reaches [`should_auto_upload`], so a `settings.json` written by a
    /// newer build cannot arm auto-upload against a destination this one would
    /// have to guess at.
    #[test]
    fn an_unknown_destination_cannot_arm_auto_upload() {
        let mut settings = Settings::default();
        settings.auto_upload = true;
        settings.destination = store::normalize_destination("s3");
        assert_eq!(settings.destination, DESTINATION_NONE);
        assert!(!should_auto_upload(&settings, &saved_record()));
    }

    /// FTP's password is optional (anonymous logins exist), so a host with no
    /// password saved still counts as configured — and no host does not.
    #[test]
    fn ftp_is_configured_by_its_host() {
        let mut settings = Settings::default();
        assert!(!status_for(DestinationKind::Ftp, &settings).configured);

        settings.ftp.host = "ftp.example.com".into();
        assert!(status_for(DestinationKind::Ftp, &settings).configured);
    }
}
