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
  FreezeInfo,
  HotkeyStatus,
  MonitorInfo,
  Rect,
  Settings,
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
  FreezeInfo,
  HotkeyMechanism,
  Hotkeys,
  HotkeyStatus,
  MonitorInfo,
  Rect,
  Settings,
  Theme,
  View,
  WindowInfo,
  WindowRect
} from '$lib/types';
export { THEME_STORAGE_KEY } from '$lib/types';

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

export type { UnlistenFn };
