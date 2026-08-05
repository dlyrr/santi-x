/**
 * The single frontend/backend boundary. Nothing else in `src/` should call
 * `invoke` or `listen` directly.
 *
 * Argument names below are the camelCase form of the Rust command parameters —
 * Tauri v2 converts `delete_file` on the Rust side to `deleteFile` over the
 * bridge, and a mismatch fails at runtime, not at compile time.
 */
import { invoke, convertFileSrc } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import type {
  ArmPayload,
  CaptureRecord,
  DestinationKind,
  DestinationStatus,
  FfmpegAvailability,
  FreezeInfo,
  HotkeyStatus,
  MonitorInfo,
  OcrResult,
  Rect,
  RecordOutcome,
  RecordSpec,
  RecordStatus,
  ScrollOutcome,
  ScrollProgress,
  Settings,
  UploadDone,
  UploadError,
  UploadProgress,
  WindowInfo
} from '$lib/types';

/**
 * The bridge types are re-exported here so a consumer only ever needs one
 * import: `import { copyCapture, type CaptureRecord } from '$lib/api'`.
 * `$lib/types` stays the single place they are declared.
 */
export type {
  ArmPayload,
  CaptureKind,
  CaptureRecord,
  CustomUploaderSettings,
  DestinationChoice,
  DestinationKind,
  DestinationStatus,
  FfmpegAttempt,
  FfmpegAvailability,
  FfmpegMissing,
  FfmpegRemedy,
  FreezeInfo,
  FtpSecurity,
  FtpSettings,
  HotkeyAction,
  HotkeyMechanism,
  Hotkeys,
  HotkeyStatus,
  MonitorInfo,
  OcrLine,
  OcrResult,
  Rect,
  RecordFormat,
  RecordOutcome,
  RecordSource,
  RecordSourceKind,
  RecordSpec,
  RecordStatus,
  ScrollOutcome,
  ScrollProgress,
  ScrollStopReason,
  SecretStatus,
  Settings,
  Theme,
  UploadDone,
  UploadError,
  UploadProgress,
  View,
  WindowInfo,
  WindowRect
} from '$lib/types';
export {
  canUploadNow,
  captureExtension,
  captureIsRecording,
  capturePlaysAsVideo,
  DESTINATION_ACCEPTS,
  DESTINATION_KINDS,
  DESTINATION_LABEL,
  destinationAcceptsCapture,
  EMPTY_CUSTOM_UPLOADER,
  FFMPEG_REMEDY_BROWSE,
  FFMPEG_SOURCE_LABEL,
  formatDuration,
  FTP_DEFAULTS,
  FTP_SECURITY,
  ftpSecurityOf,
  IMGUR_REGISTER_URL,
  isDestinationKind,
  isRecordFormat,
  isScrollStopReason,
  RECORD_FORMAT_LABEL,
  RECORD_FORMATS,
  RECORD_FPS,
  RECORD_GIF_FPS,
  RECORD_GIF_MAX_WIDTH,
  RECORD_PHASES,
  RECORD_SOURCES,
  RECORD_STOP_HOTKEY_DEFAULT,
  recordFormatOf,
  SCROLL_DELAY_MS,
  SCROLL_MAX_FRAMES,
  SCROLL_STEP,
  THEME_STORAGE_KEY
} from '$lib/types';

/* ------------------------------------------------------------------ errors */

/**
 * Rust commands return `Result<T, String>`, which arrives here as a bare
 * string rather than an `Error`. Normalise everything to a displayable line.
 */
export function errorMessage(e: unknown): string {
  if (typeof e === 'string') return e;
  if (e instanceof Error) return e.message;
  if (e && typeof e === 'object') {
    const msg = (e as { message?: unknown }).message;
    if (typeof msg === 'string') return msg;
    try {
      return JSON.stringify(e);
    } catch {
      return String(e);
    }
  }
  return String(e);
}

/* ------------------------------------------------------------------ assets */

/**
 * Convert an absolute filesystem path into a URL the webview may load.
 * Used for thumbnails and the freeze frame; requires the asset protocol scope
 * declared in `tauri.conf.json`.
 */
export function assetUrl(path: string): string {
  return convertFileSrc(path);
}

/**
 * `assetUrl` for a file whose *contents* can change under a fixed path.
 *
 * `save_edit` with `replace: true` rewrites the capture and its thumbnail in
 * place, so `record.path` and `record.thumb` are unchanged. An `<img>` whose
 * `src` attribute never changes is never re-fetched — the gallery would keep
 * showing the pre-edit picture until the window reloads. `size_bytes` is
 * rewritten by every save, so it is the version stamp.
 *
 * The asset protocol resolves the file from the URL *path* only, so the extra
 * query is invisible to Rust and to the scope check.
 */
export function versionedAssetUrl(path: string, version: number | string): string {
  const url = assetUrl(path);
  return `${url}${url.includes('?') ? '&' : '?'}v=${encodeURIComponent(String(version))}`;
}

/* ---------------------------------------------------------------- settings */

export async function getSettings(): Promise<Settings> {
  return invoke<Settings>('get_settings');
}

/** Persists and re-registers the global hotkeys; returns the stored settings. */
export async function saveSettings(settings: Settings): Promise<Settings> {
  return invoke<Settings>('save_settings', { settings });
}

/* ----------------------------------------------------------------- hotkeys */

/**
 * How each hotkey is currently bound — `plugin`, the `hook` fallback, or `none`
 * (M2.6 §1). Rust holds the last report, so this is safe to call at any time;
 * it is a snapshot of the most recent registration, not a re-registration.
 */
export async function getHotkeyStatus(): Promise<HotkeyStatus[]> {
  return invoke<HotkeyStatus[]>('get_hotkey_status');
}

/* ----------------------------------------------------------------- history */

export async function getHistory(): Promise<CaptureRecord[]> {
  return invoke<CaptureRecord[]>('get_history');
}

/** Returns the history *after* the deletion — replace the local list with it. */
export async function deleteCapture(
  id: string,
  deleteFile: boolean
): Promise<CaptureRecord[]> {
  return invoke<CaptureRecord[]>('delete_capture', { id, deleteFile });
}

export async function clearHistory(deleteFiles: boolean): Promise<CaptureRecord[]> {
  return invoke<CaptureRecord[]>('clear_history', { deleteFiles });
}

export async function copyCapture(id: string): Promise<void> {
  await invoke<void>('copy_capture', { id });
}

export async function revealCapture(id: string): Promise<void> {
  await invoke<void>('reveal_capture', { id });
}

export async function openCapture(id: string): Promise<void> {
  await invoke<void>('open_capture', { id });
}

export async function openSaveDir(): Promise<void> {
  await invoke<void>('open_save_dir');
}

/* --------------------------------------------------------------- discovery */

export async function listMonitors(): Promise<MonitorInfo[]> {
  return invoke<MonitorInfo[]>('list_monitors');
}

export async function listWindows(): Promise<WindowInfo[]> {
  return invoke<WindowInfo[]>('list_windows');
}

/* ----------------------------------------------------------------- capture */

export async function captureFullscreen(): Promise<CaptureRecord> {
  return invoke<CaptureRecord>('capture_fullscreen');
}

export async function captureMonitor(id: number): Promise<CaptureRecord> {
  return invoke<CaptureRecord>('capture_monitor', { id });
}

export async function captureActiveWindow(): Promise<CaptureRecord> {
  return invoke<CaptureRecord>('capture_active_window');
}

export async function captureWindow(id: number): Promise<CaptureRecord> {
  return invoke<CaptureRecord>('capture_window', { id });
}

/* ------------------------------------------------------ scrolling capture */

/**
 * Scroll the window with this `list_windows` id and stitch the frames into one
 * tall capture (M5 §4).
 *
 * Long-running by construction — `scrollMaxFrames` × `scrollDelayMs` is tens of
 * seconds — so the promise is not the only feedback: Rust emits
 * `scroll://progress` after every grab, and the UI must show it.
 *
 * It resolves for *every* stop reason, including a cancel and a lost overlap,
 * because each of those still finalizes the frames already stitched. Resolving
 * is therefore not the same as succeeding: read `incomplete` before saying so.
 * It rejects only when the run could not start or produced nothing.
 *
 * The run drives the real mouse pointer, so moving the mouse ends it — that is
 * the documented escape hatch, not a fault.
 */
export async function startScrollCapture(id: number): Promise<ScrollOutcome> {
  return invoke<ScrollOutcome>('start_scroll_capture', { id });
}

/**
 * Ask the run in flight to stop. It keeps and finalizes whatever was already
 * stitched, then resolves the original call with `reason: 'cancelled'` — a
 * cancel is "stop here", not "throw away the last thirty seconds". Infallible
 * on the Rust side, and a no-op when nothing is running.
 */
export async function cancelScrollCapture(): Promise<void> {
  await invoke<void>('cancel_scroll_capture');
}

/* -------------------------------------------------- screen recording (M4) */

/**
 * Whether recording can be offered at all, and what to say when it cannot
 * (M4 §1).
 *
 * Cheap: a successful probe is cached for the session on the Rust side, keyed by
 * the `Settings.ffmpegPath` it was resolved under, so changing that setting
 * re-resolves rather than being ignored. A *failure* is deliberately not cached
 * — the user's next move after reading the message is usually to run the command
 * it just gave them, and they should not have to restart the app for that to be
 * noticed. So there is no `refresh` flag: calling again is the refresh.
 *
 * **Never render a download affordance from this.** When `available` is false
 * the UI shows `missing`, the `remedies` verbatim and a Browse control;
 * fetching and executing an ~80MB binary is out of bounds (M4 §1).
 */
export async function recordAvailability(): Promise<FfmpegAvailability> {
  return invoke<FfmpegAvailability>('record_availability');
}

/**
 * The recorder's live state, or the resting `idle` value when nothing is being
 * recorded.
 *
 * The catch-up half of `record://status`, exactly as `getPreviewRecord` is for
 * `preview://show`: the first recording of a session builds the HUD webview,
 * which is therefore still loading when the first event goes out. Without this
 * read the first HUD of a session would sit blank.
 */
export async function recordingStatus(): Promise<RecordStatus> {
  return invoke<RecordStatus>('recording_status');
}

/**
 * Start recording (M4 §2–§5). Resolves as soon as frames are flowing, with the
 * opening status — **not** with the finished file.
 *
 * Everything after that arrives on events: `record://status` about four times a
 * second, then `capture://new` and `record://finished` for a recording that was
 * kept, `record://cancelled` for one that was discarded, and `capture://error`
 * when it failed. A caller that only wants to know it started can ignore all of
 * them; a caller that reports the outcome must subscribe, because the promise
 * has long since resolved by then.
 *
 * Rejects when the recording could not start: no ffmpeg, a missing encoder, a
 * minimized window, an area too small, or one already running.
 */
export async function startRecording(spec: RecordSpec): Promise<RecordStatus> {
  return invoke<RecordStatus>('start_recording', { spec });
}

/**
 * Finish the recording in flight and keep the file.
 *
 * One of the three routes that must each work when the other two do not — this,
 * the global stop hotkey and the tray item (M4 §4). It rejects when nothing is
 * recording, which is a real answer rather than a fault: all three routes can be
 * a moment behind a recording that has just ended, so treat that rejection as
 * "already stopped" rather than raising it at the user.
 */
export async function stopRecording(): Promise<void> {
  await invoke<void>('stop_recording');
}

/**
 * Finish the recording in flight and discard it. Nothing is written and nothing
 * lands in history; the ending arrives on `record://cancelled`.
 *
 * Same "rejects when idle" contract as `stopRecording`.
 */
export async function cancelRecording(): Promise<void> {
  await invoke<void>('cancel_recording');
}

/**
 * The HUD webview taking its own window off screen.
 *
 * Rust hides it on every ending, so this is not the normal path — it is the
 * backstop for the one state Rust cannot see: the window is visible and nothing
 * is recording. That window is always-on-top, borderless, `skip_taskbar` and
 * unfocusable, so a stranded one has no titlebar to close, no taskbar entry to
 * right-click and no way in with the keyboard. Hiding it from here is strictly
 * better than leaving it up.
 *
 * Only ever call it after confirming with `recordingStatus()` that nothing is
 * active: hiding the HUD out from under a live recording would leave a capture
 * running with nothing on screen saying so, which is the failure this window
 * exists to prevent.
 */
export async function hideRecorderWindow(): Promise<void> {
  await getCurrentWindow().hide();
}

/* ------------------------------------------------------------------ region */

/**
 * Freezes the desktop and arms the pre-warmed overlay window: Rust emits
 * `overlay://arm`, then shows the window once the overlay answers with
 * `overlayReady()` (M2.5 §1).
 */
export async function startRegionCapture(): Promise<void> {
  await invoke<void>('start_region_capture');
}

/**
 * The overlay has decoded the freeze frame and painted it — Rust may now
 * position and show the window. Pass back `ArmPayload.armId` untouched:
 * the call is idempotent and a stale `armId` is ignored, so an arm that was
 * superseded mid-decode cannot flash the wrong frame across the desktop.
 */
export async function overlayReady(armId: number): Promise<void> {
  await invoke<void>('overlay_ready', { armId });
}

export async function getFreezeFrame(): Promise<FreezeInfo> {
  const info = await invoke<FreezeInfo>('get_freeze_frame');
  // Rust hands back the freeze frame's location; depending on how it is built
  // that may already be a webview-loadable URL or still a raw path. Convert
  // only when it is a path, so this stays correct either way.
  return isLoadableUrl(info.src) ? info : { ...info, src: assetUrl(info.src) };
}

/** `rect` is in freeze-image pixels, not CSS pixels. */
export async function finishRegionCapture(rect: Rect): Promise<CaptureRecord> {
  return invoke<CaptureRecord>('finish_region_capture', { rect });
}

/**
 * The annotated twin of `finishRegionCapture`: `png` is the already-cropped
 * selection with its shapes drawn in, base64 with no data-URI prefix, and Rust
 * runs it through the same `finalize(app, img, "region")`.
 *
 * Only take this path when shapes were actually drawn — a 4K annotated PNG is
 * 15–25 MB of base64 through the IPC, where the plain rect path crops natively
 * from the image Rust already holds (M2.5 §3).
 */
export async function finishRegionCaptureAnnotated(png: string): Promise<CaptureRecord> {
  return invoke<CaptureRecord>('finish_region_capture_annotated', { png });
}

export async function cancelRegionCapture(): Promise<void> {
  await invoke<void>('cancel_region_capture');
}

function isLoadableUrl(src: string): boolean {
  return /^(https?|asset|tauri|blob|data):/i.test(src);
}

/* ------------------------------------------------------------------ editor */

/** Creates the `editor` window, or focuses the existing one and loads `id`. */
export async function openEditor(id: string): Promise<void> {
  await invoke<void>('open_editor', { id });
}

export async function getCapture(id: string): Promise<CaptureRecord> {
  return invoke<CaptureRecord>('get_capture', { id });
}

/**
 * Absolute path to the full-resolution PNG — the record's `path` is empty when
 * the capture was never written to disk, so the editor asks Rust instead of
 * reading the record. Feed the result through `assetUrl()` to load it.
 */
export async function getCaptureImage(id: string): Promise<string> {
  return invoke<string>('get_capture_image', { id });
}

/**
 * `png` is base64 with no data-URI prefix. `replace: true` overwrites the
 * original file and returns the updated record; `false` writes a new record of
 * kind `edit` and leaves the original alone.
 */
export async function saveEdit(
  id: string,
  png: string,
  replace: boolean
): Promise<CaptureRecord> {
  return invoke<CaptureRecord>('save_edit', { id, png, replace });
}

/** Same base64 encoding as `saveEdit`; writes the image to the clipboard. */
export async function copyEdit(png: string): Promise<void> {
  await invoke<void>('copy_edit', { png });
}

export async function closeEditor(): Promise<void> {
  await invoke<void>('close_editor');
}

/* --------------------------------------------------------------------- OCR */

/**
 * Read the text out of a capture with the offline Windows OCR engine (M5 §3).
 * Rust reads the file from disk, so this rejects for a record with no `path` —
 * `saveToDisk` was off and there is nothing to recognise. Check before offering
 * the action rather than letting the user discover it in an error.
 *
 * A recognition that finds nothing is a *success* with an empty `text` and no
 * lines, not a rejection. That is a real state the UI must name.
 */
export async function ocrCapture(id: string): Promise<OcrResult> {
  return invoke<OcrResult>('ocr_capture', { id });
}

/* ----------------------------------------------------------------- preview */

/**
 * Hide the post-capture preview window (M2.9 §3). Rust hides and keeps it, the
 * way it keeps the overlay — it is never destroyed.
 *
 * The fallback is not defensive padding. That window is borderless,
 * `skip_taskbar` and always-on-top, and it never takes focus, so one that is
 * still on screen has no titlebar to close and no taskbar entry to right-click:
 * it is a sticker the user cannot get rid of. If the command fails — it is gone,
 * the state is poisoned, anything — hiding the window from the webview side is
 * strictly better than leaving it up. Only a failure of *both* rejects.
 */
export async function hideCapturePreview(): Promise<void> {
  try {
    await invoke<void>('hide_capture_preview');
  } catch {
    await getCurrentWindow().hide();
  }
}

/**
 * The capture the preview window is currently on, or `null` when it is down.
 *
 * Rust emits `preview://show` exactly once per capture, immediately before it
 * shows the window. On the first capture this webview is still being built when
 * that emit goes out, so the listener is not up to hear it and the card would
 * sit blank. Reading the record back on mount is what makes the single emit
 * safe — it is the documented other half of that contract, not a retry.
 */
export async function getPreviewRecord(): Promise<CaptureRecord | null> {
  return invoke<CaptureRecord | null>('get_preview_record');
}

/* ------------------------------------------------------------- destinations */

/**
 * All three destinations, in a fixed order: which is active, whether each is
 * configured, and which of its credentials exist (M3 §6).
 *
 * **Booleans only.** No command anywhere returns a stored secret — `secrets::get`
 * is `pub(in crate::upload)`, so a `#[tauri::command]` cannot even reach one —
 * and no UI may be written that expects one (M3 §2).
 *
 * Call it again after anything that could change the answer: a secret written
 * or cleared, a `.sxcu` imported, a settings save that moves the active
 * destination. It is a cheap read of the credential store, not a network call.
 */
export async function destinationStatus(): Promise<DestinationStatus[]> {
  return invoke<DestinationStatus[]>('destination_status');
}

/**
 * Write a credential into Windows Credential Manager under
 * `santi.sharex/<kind>` (M3 §2). One-way: nothing reads it back out to here.
 *
 * `field` is a `SecretStatus.field` from `destinationStatus()` — never a name
 * typed out here. Rust owns that list, because a custom uploader's slots are
 * whichever headers its `.sxcu` declared.
 *
 * A blank `value` **clears** the credential rather than storing an empty one:
 * that is what emptying the field means, and an empty stored secret would show
 * as configured and then fail at upload. Rust also trims, because a key pasted
 * out of a web page usually arrives with a newline on it.
 *
 * The `value` exists in this process only for the length of this call. Do not
 * keep it in a `$state`, a draft, or anything that survives the await: clear the
 * input the moment this resolves.
 */
export async function setDestinationSecret(
  kind: DestinationKind,
  field: string,
  value: string
): Promise<void> {
  await invoke<void>('set_destination_secret', { kind, field, value });
}

/** Removes the credential entirely. Succeeds when there was none to remove. */
export async function clearDestinationSecret(
  kind: DestinationKind,
  field: string
): Promise<void> {
  await invoke<void>('clear_destination_secret', { kind, field });
}

/**
 * Check a destination's setup without uploading a capture. Resolves with one
 * sentence for the user — **show it verbatim**, because how much was actually
 * verified differs per destination and the sentence is what says so:
 *
 * - `ftp` really connects, logs in and looks at the remote directory, and
 *   creates nothing. This is the only one that is networked, so it can take as
 *   long as a connect timeout.
 * - `imgur` checks a Client ID is saved and can go in a request header. It does
 *   **not** ask Imgur: the only request Imgur offers uploads a picture, and a
 *   test must leave nothing behind (M3 §6).
 * - `custom` re-validates the imported `.sxcu` and that every credential header
 *   has a value stored. It does **not** contact the server, for the same reason.
 *
 * Do not paraphrase any of them into "Connected" — a test that says it verified
 * something it never asked about is worse than no test.
 *
 * A rejection carries the **real** error — the FTP server's own refusal, the
 * missing credential, the DNS failure. Show it as it arrives: "Test failed"
 * tells the user nothing they can act on, and Rust has already guaranteed the
 * text contains no credential (M3 §2).
 */
export async function testDestination(kind: DestinationKind): Promise<string> {
  return invoke<string>('test_destination', { kind });
}

/**
 * Validate the text of a ShareX `.sxcu` against the supported subset (M3 §4)
 * and store it as `Settings.customUploader`. Resolves with the whole `Settings`
 * as saved; Rust also emits `settings://changed`, so the store adopts it either
 * way and the caller rarely needs the return value.
 *
 * Takes the file's **contents**, not a path — there is no filesystem plugin in
 * this app, so the webview reads the file it was handed (a plain
 * `<input type="file">`) and passes the text across. `custom::import` is what
 * splits out the credential-looking headers, into a type that is deliberately
 * not `Serialize`, so nothing comes back the other way.
 *
 * A file needing something unsupported — OAuth, `RegexList`, a `RequestBodyType`
 * outside the two, a destination that is not an image uploader — is **rejected
 * here** rather than accepted and mysteriously broken at upload time. Every
 * rejection names the offending field; surface it verbatim.
 *
 * Importing does not switch the active destination: choosing where captures go
 * is the user's decision, and picking a file is not that decision (M3 §1).
 *
 * There is no matching "forget it" command, and none is needed. Clear each
 * `SecretStatus.field` from `destinationStatus()` with
 * `clearDestinationSecret('custom', …)`, **then** save
 * `customUploader: EMPTY_CUSTOM_UPLOADER`. That order matters: the config is
 * what names the credentials, so dropping it first would orphan them in
 * Credential Manager with nothing left to find them by.
 */
export async function importCustomUploader(contents: string): Promise<Settings> {
  return invoke<Settings>('import_custom_uploader', { contents });
}

/* ------------------------------------------------------------------ upload */

/**
 * Send one capture to the active destination, and resolve with the record as it
 * is *after* the upload — `url` and `deletionUrl` filled in (M3 §6).
 *
 * **This is the only thing in `src/` that puts a capture on the network.** The
 * one other caller is Rust's own `auto_upload_if_enabled`, behind
 * `Settings.autoUpload`. Nothing calls this speculatively, on hover, or on
 * mount.
 *
 * The transfer runs on a `spawn_blocking` worker, so awaiting it does not wedge
 * the app — but it does mean the promise is outstanding for the whole upload.
 * Subscribe to `onUploadProgress` **before** calling, or the bar has nothing to
 * move; `capture://updated` carries the same record to the history store, so a
 * view backed by that store needs nothing from the return value.
 *
 * Rejects with the real reason: no destination, a missing credential, the
 * server's own refusal, "already uploading", or `Upload cancelled`. Read
 * `UploadError.cancelled` from `onUploadError` rather than matching the string
 * to tell a cancel from a failure.
 */
export async function uploadCapture(id: string): Promise<CaptureRecord> {
  return invoke<CaptureRecord>('upload_capture', { id });
}

/**
 * Ask the upload in flight for `id` to stop, and mean it: the payload reader
 * checks the flag on every chunk, so the HTTP body or the FTP data connection
 * ends mid-flight rather than the result being quietly discarded.
 *
 * Infallible, and a no-op when that capture is not uploading — the UI is often a
 * moment behind a transfer that just finished, and that is not an error. The
 * outcome still arrives on `upload://error` with `cancelled: true`.
 */
export async function cancelUpload(id: string): Promise<void> {
  await invoke<void>('cancel_upload', { id });
}

/* ------------------------------------------------------------------ events */

export function onCaptureNew(cb: (record: CaptureRecord) => void): Promise<UnlistenFn> {
  return listen<CaptureRecord>('capture://new', (e) => cb(e.payload));
}

/**
 * An existing record changed in place (an edit saved over its original). The
 * payload replaces the record with the same `id`; it is never a new row.
 */
export function onCaptureUpdated(cb: (record: CaptureRecord) => void): Promise<UnlistenFn> {
  return listen<CaptureRecord>('capture://updated', (e) => cb(e.payload));
}

/**
 * A second `open_editor` arrived while the editor window was already up (M2 §1).
 * Rust focuses the live window and emits this to it instead of building another;
 * the payload is the capture id to load. Only the `editor` window receives it,
 * and it is the editor's job to prompt before discarding a dirty document.
 */
export function onEditorLoad(cb: (id: string) => void): Promise<UnlistenFn> {
  return listen<string>('editor://load', (e) => cb(e.payload));
}

/**
 * A capture has begun and the pre-warmed overlay must draw this freeze frame
 * (M2.5 §1). Only the `overlay` window receives it.
 *
 * `src` is normalised the way `getFreezeFrame()` does it, so the callback
 * always gets a webview-loadable URL whether Rust sent a path or a URL. The
 * overlay must answer with `overlayReady(payload.armId)` once it has painted —
 * Rust is holding the window hidden until then.
 */
export function onOverlayArm(cb: (payload: ArmPayload) => void): Promise<UnlistenFn> {
  return listen<ArmPayload>('overlay://arm', (e) => {
    const payload = e.payload;
    cb(isLoadableUrl(payload.src) ? payload : { ...payload, src: assetUrl(payload.src) });
  });
}

/**
 * The overlay window has been hidden by Rust and owns nothing any more: drop the
 * decoded freeze frame and go back to idle. Only the `overlay` window receives it.
 *
 * Advisory — an arm is never shown until the overlay has acknowledged that arm's
 * own frame, so ignoring this cannot show a stale desktop. What it buys is the
 * paths the overlay did not initiate itself (a superseded double-press, a capture
 * that failed after arming): without it the window sits `armed` behind the
 * scenes, holding a virtual-desktop-sized `ImageBitmap` for the rest of the
 * session.
 */
export function onOverlayDisarm(cb: () => void): Promise<UnlistenFn> {
  return listen<null>('overlay://disarm', () => cb());
}

/**
 * A capture has landed and the preview window should show it (M2.9 §3). Only
 * the `preview` window receives it; Rust positions and shows the window itself,
 * so the payload is the whole job.
 *
 * A second event while a preview is up replaces its content and restarts the
 * countdown — it is never a queue and never a second window.
 */
export function onCapturePreview(cb: (record: CaptureRecord) => void): Promise<UnlistenFn> {
  return listen<CaptureRecord>('preview://show', (e) => cb(e.payload));
}

/**
 * Rust has taken the preview window off screen on its own — the next capture
 * claimed it, a settings save switched the feature off, or the app is exiting.
 * Only the `preview` window receives it.
 *
 * Advisory: the window is hidden either way, so nothing depends on this being
 * heard. What it buys is the webview not sitting there holding a capture that
 * is no longer on screen, still counting down towards a dismissal of a window
 * that is already gone.
 */
export function onPreviewHidden(cb: () => void): Promise<UnlistenFn> {
  return listen<null>('preview://hide', () => cb());
}

/**
 * A scrolling run has captured another frame, or moved on to stitching or
 * saving (M5 §4). Emitted to every window, so the capture view hears it whether
 * or not it started the run.
 *
 * This is the whole reason the feature is usable: without it a minute-long run
 * is a frozen button. Only `frames` is guaranteed — treat the rest as hints.
 */
export function onScrollProgress(cb: (progress: ScrollProgress) => void): Promise<UnlistenFn> {
  return listen<ScrollProgress>('scroll://progress', (e) => cb(e.payload));
}

/**
 * The recorder's whole state, and the only event the HUD needs (M4 §4). Emitted
 * to **every** window — on start, about four times a second while a recording
 * runs, and once more as the resting `idle` value when it ends.
 *
 * That last emit is what takes the HUD's content down; Rust hides the window
 * itself on every ending, so nothing here is load-bearing for the window's
 * visibility. `elapsedMs` is the authority for the clock — a consumer may
 * interpolate between events so the seconds tick smoothly, but it must not keep
 * counting past an `idle`.
 */
export function onRecordStatus(cb: (status: RecordStatus) => void): Promise<UnlistenFn> {
  return listen<RecordStatus>('record://status', (e) => cb(e.payload));
}

/**
 * A recording was kept: the file is written, thumbnailed and in history. The
 * record itself also arrives on `capture://new`, which the history store adopts,
 * so this exists for the part that is not the record — the frame counts, and
 * `truncated`, which says ffmpeg had to be killed and the clip may be short.
 */
export function onRecordFinished(cb: (outcome: RecordOutcome) => void): Promise<UnlistenFn> {
  return listen<RecordOutcome>('record://finished', (e) => cb(e.payload));
}

/**
 * A recording was discarded, by the HUD's Cancel or the same command elsewhere.
 * `record` is `null` — a cancel by definition kept nothing — so this is not a
 * failure and must not be reported as one.
 */
export function onRecordCancelled(cb: (outcome: RecordOutcome) => void): Promise<UnlistenFn> {
  return listen<RecordOutcome>('record://cancelled', (e) => cb(e.payload));
}

export function onCaptureError(cb: (message: string) => void): Promise<UnlistenFn> {
  return listen<string>('capture://error', (e) => cb(e.payload));
}

export function onSettingsChanged(cb: (settings: Settings) => void): Promise<UnlistenFn> {
  return listen<Settings>('settings://changed', (e) => cb(e.payload));
}

/**
 * Every hotkey was re-registered — at startup, after a settings save, or after a
 * hook install failed and downgraded the rows it was going to own. The payload
 * is the complete list, so replace the local one rather than merging into it.
 */
export function onHotkeyStatus(cb: (statuses: HotkeyStatus[]) => void): Promise<UnlistenFn> {
  return listen<HotkeyStatus[]>('hotkeys://status', (e) => cb(e.payload));
}

/**
 * Bytes sent for one upload (M3 §6). Emitted to every window, so the capture
 * preview hears progress for an upload the history view started and the other
 * way round — always filter on `payload.id` before touching local state.
 *
 * `total` is `0` when the destination streams without a known length; that is
 * an indeterminate upload, not a zero-byte one.
 */
export function onUploadProgress(cb: (progress: UploadProgress) => void): Promise<UnlistenFn> {
  return listen<UploadProgress>('upload://progress', (e) => cb(e.payload));
}

/**
 * An upload finished and the destination returned `url`. The record itself gains
 * its `url` on the Rust side and arrives separately on `capture://updated`, so
 * a view backed by the history store does not have to patch anything from here.
 *
 * The clipboard write, when `Settings.copyUrlAfterUpload` is on, has already
 * happened in Rust — do not do it again from the UI.
 */
export function onUploadDone(cb: (done: UploadDone) => void): Promise<UnlistenFn> {
  return listen<UploadDone>('upload://done', (e) => cb(e.payload));
}

/**
 * An upload failed, or was cancelled — one event for both, told apart by
 * `payload.cancelled` and never by matching the message.
 *
 * `message` is a finished sentence, shown as it arrives: it is the only thing
 * that says whether to retry, fix a credential, or wait out a rate limit, and
 * Rust guarantees no credential is ever interpolated into it (M3 §2). A
 * cancelled upload is not a failure — take the progress row down without
 * raising a red toast for something the user just asked for.
 */
export function onUploadError(cb: (error: UploadError) => void): Promise<UnlistenFn> {
  return listen<UploadError>('upload://error', (e) => cb(e.payload));
}

export type { UnlistenFn };
