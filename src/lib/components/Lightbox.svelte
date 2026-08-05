<script lang="ts">
  import { ask } from '@tauri-apps/plugin-dialog';
  import {
    cancelUpload,
    captureIsRecording,
    captureExtension,
    capturePlaysAsVideo,
    copyCapture,
    destinationAcceptsCapture,
    errorMessage,
    formatDuration,
    onUploadError,
    onUploadProgress,
    openCapture,
    openEditor,
    revealCapture,
    uploadCapture,
    versionedAssetUrl,
    type CaptureRecord
  } from '$lib/api';
  import { writeClipboardText } from '$lib/clipboard';
  import { history } from '$lib/stores/history.svelte';
  import { settings } from '$lib/stores/settings.svelte';
  import OcrPanel from '$lib/components/OcrPanel.svelte';
  import { toast } from '$lib/components/Toast.svelte';

  interface Props {
    records: CaptureRecord[];
    index: number;
    onclose: () => void;
    onnavigate: (index: number) => void;
  }

  let { records, index, onclose, onnavigate }: Props = $props();

  let scrimEl: HTMLDivElement | undefined = $state();
  let dialogEl: HTMLDivElement | undefined = $state();
  let imgBroken = $state(false);
  let ocrOpen = $state(false);

  /**
   * The capture whose upload is in flight, by id rather than a boolean: the
   * lightbox navigates between records while one is uploading, and the progress
   * shown must belong to the picture on screen, not to whichever one was on
   * screen when the transfer began.
   */
  let uploadingId = $state<string | null>(null);
  let sent = $state(0);
  let total = $state(0);
  let unlisteners: Array<() => void> = [];

  /**
   * Set from `upload://error`, the authority on it: a cancel rejects
   * `upload_capture` like any other failure, and the flag is the only honest
   * way to tell them apart.
   */
  let cancelled = false;

  const record = $derived(records[index] ?? records[records.length - 1] ?? null);
  const hasFile = $derived(!!record && record.saved && record.path !== '');

  /**
   * This record is a screen recording, so most of this dialog's actions do not
   * apply to it (M4 §5). Asked by kind; whether it *plays* as a video is a
   * separate question, because a GIF recording is an image to the webview and
   * animates in an `<img>` where a `<video>` would show nothing at all.
   */
  const isRecording = $derived(!!record && captureIsRecording(record));
  const playsAsVideo = $derived(!!record && capturePlaysAsVideo(record));

  const uploading = $derived(!!record && uploadingId === record.id);
  const percent = $derived(total > 0 ? Math.min(100, Math.round((sent / total) * 100)) : null);

  /**
   * A destination is *selected*, and it takes this record's format (M4 §5).
   * Readiness beyond that is not checked here: a missing credential comes back
   * as the upload's own error, which names what to fix. `?? 'none'` so a pre-M3
   * `settings.json` reads as "nowhere to send".
   *
   * The format half only ever subtracts an affordance — every destination has
   * always taken a PNG — so this cannot hide the Upload button for a still.
   */
  const destination = $derived(settings.current?.destination ?? 'none');
  const canUpload = $derived(
    destination !== 'none' && !!record && destinationAcceptsCapture(destination, record)
  );
  // Versioned: an edit saved over its original keeps the same path, and an
  // unchanged `src` is never re-fetched (see `versionedAssetUrl`).
  const fullSrc = $derived(record ? versionedAssetUrl(record.path, record.sizeBytes) : '');

  $effect(() => {
    if (records.length === 0) onclose();
  });

  // Reset the broken-image flag whenever we move to a different capture — or
  // when the one on screen is rewritten by the editor.
  $effect(() => {
    void fullSrc;
    imgBroken = false;
  });

  // The OCR panel describes one exact picture. Moving to another capture, or
  // the editor rewriting this one in place, would leave text on screen that no
  // longer belongs to the image behind it.
  $effect(() => {
    void fullSrc;
    ocrOpen = false;
  });

  $effect(() => {
    const previous = document.activeElement as HTMLElement | null;
    dialogEl?.focus();
    return () => previous?.focus?.();
  });

  // Closing the lightbox does not cancel the upload — Rust owns it and it keeps
  // going — but it must not leave listeners behind holding this dialog's state.
  $effect(() => stopWatching);

  function formatBytes(n: number): string {
    if (n < 1024) return `${n} B`;
    const units = ['KB', 'MB', 'GB'];
    let value = n / 1024;
    let i = 0;
    while (value >= 1024 && i < units.length - 1) {
      value /= 1024;
      i++;
    }
    return `${value.toFixed(value < 10 ? 1 : 0)} ${units[i]}`;
  }

  function go(delta: number) {
    if (records.length < 2) return;
    const next = (index + delta + records.length) % records.length;
    onnavigate(next);
  }

  function focusable(): HTMLElement[] {
    if (!dialogEl) return [];
    const nodes = dialogEl.querySelectorAll<HTMLElement>(
      'button:not([disabled]), [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
    );
    return Array.from(nodes).filter((el) => el.offsetParent !== null);
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      event.preventDefault();
      onclose();
      return;
    }
    if (event.key === 'ArrowLeft') {
      event.preventDefault();
      go(-1);
      return;
    }
    if (event.key === 'ArrowRight') {
      event.preventDefault();
      go(1);
      return;
    }
    if (event.key !== 'Tab') return;

    // Focus trap: the dialog itself is tabindex="-1", so without this Tab would
    // walk out into the (inert but still focusable) app behind the scrim.
    const items = focusable();
    if (items.length === 0) {
      event.preventDefault();
      return;
    }
    const first = items[0];
    const last = items[items.length - 1];
    const active = document.activeElement as HTMLElement | null;
    if (event.shiftKey && (active === first || active === dialogEl)) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    }
  }

  function onScrimPointer(event: MouseEvent) {
    if (event.target === scrimEl) onclose();
  }

  async function doCopy() {
    if (!record) return;
    try {
      await copyCapture(record.id);
      toast.success('Copied to clipboard');
    } catch (err) {
      toast.error(String(err));
    }
  }

  /** The editor is its own window, so the preview steps aside for it. */
  async function doEdit() {
    if (!record) return;
    // The button is already disabled for a recording; this is the second half of
    // "refuses it with a clear message rather than opening on a broken canvas"
    // (M4 §5), for every route that is not that button.
    if (isRecording) {
      toast.error('The editor works on still images. This capture is a screen recording.');
      return;
    }
    try {
      await openEditor(record.id);
      onclose();
    } catch (err) {
      toast.error(String(err));
    }
  }

  async function doOpen() {
    if (!record) return;
    try {
      await openCapture(record.id);
    } catch (err) {
      toast.error(String(err));
    }
  }

  async function doReveal() {
    if (!record) return;
    try {
      await revealCapture(record.id);
    } catch (err) {
      toast.error(String(err));
    }
  }

  function stopWatching() {
    for (const off of unlisteners) off();
    unlisteners = [];
  }

  function endUpload() {
    uploadingId = null;
    sent = 0;
    total = 0;
    stopWatching();
  }

  /**
   * The only thing in this dialog that puts the capture on the network, and it
   * happens because this button was pressed — never on open, never on navigate.
   *
   * Listeners go up before the command: `upload_capture` stays outstanding for
   * the whole transfer, so a subscription made after the await would never be
   * made at all. The record's `url` arrives on `capture://updated`, which the
   * history store adopts, and `records` re-renders with the link action.
   */
  async function doUpload() {
    if (!record || uploadingId) return;
    const id = record.id;

    uploadingId = id;
    cancelled = false;
    sent = 0;
    total = 0;
    try {
      unlisteners = await Promise.all([
        onUploadProgress((p) => {
          if (p.id !== id) return;
          sent = p.sent;
          total = p.total;
        }),
        onUploadError((e) => {
          if (e.id !== id) return;
          cancelled = e.cancelled;
        })
      ]);
      await uploadCapture(id);
      endUpload();
      toast.success('Uploaded');
    } catch (err) {
      const stopped = cancelled;
      endUpload();
      // Getting exactly what you asked for is not an error.
      if (stopped) toast.info('Upload cancelled');
      else toast.error(errorMessage(err));
    }
  }

  /** A real stop on the Rust side; the state clears when the command returns. */
  async function doCancelUpload() {
    if (!uploadingId) return;
    await cancelUpload(uploadingId);
  }

  /** See `CaptureCard.doCopyLink` for why this is not `navigator.clipboard`. */
  async function doCopyLink() {
    const url = record?.url;
    if (!url) return;
    try {
      await writeClipboardText(url);
      toast.success('Link copied');
    } catch (err) {
      toast.error(errorMessage(err));
    }
  }

  async function doDelete() {
    if (!record) return;
    const target = record;
    const removeFile = hasFile;
    const message = removeFile
      ? `Delete "${target.name}"? The file will be removed from disk.`
      : `Remove "${target.name}" from history?`;
    const confirmed = await ask(message, { title: 'Delete capture', kind: 'warning' });
    if (!confirmed) return;
    // The store reports failures through `error` rather than rejecting.
    const ok = await history.remove(target.id, removeFile);
    if (!ok) {
      toast.error(history.error ?? 'Could not delete the capture');
      return;
    }
    toast.success(removeFile ? 'Capture deleted' : 'Removed from history');
    if (records.length === 0) onclose();
    else if (index > records.length - 1) onnavigate(records.length - 1);
  }
</script>

{#if record}
  <div
    class="scrim"
    bind:this={scrimEl}
    role="presentation"
    onclick={onScrimPointer}
  >
    <div
      class="dialog"
      bind:this={dialogEl}
      role="dialog"
      aria-modal="true"
      aria-label={record.name}
      tabindex="-1"
      onkeydown={onKeydown}
    >
      <header class="head">
        <div class="titles">
          <h2 class="name" title={record.name}>{record.name}</h2>
          <p class="meta">
            <span class="num">{record.width}&times;{record.height}</span>
            <span class="sep" aria-hidden="true">&middot;</span>
            <span class="kind">{record.kind}</span>
            {#if record.durationMs}
              <span class="sep" aria-hidden="true">&middot;</span>
              <span class="num">{formatDuration(record.durationMs)}</span>
            {/if}
            <span class="sep" aria-hidden="true">&middot;</span>
            <span class="num">{formatBytes(record.sizeBytes)}</span>
            <span class="sep" aria-hidden="true">&middot;</span>
            <span class="num">{new Date(record.createdAt).toLocaleString()}</span>
          </p>
        </div>
        <button type="button" class="btn btn-icon" aria-label="Close preview" onclick={onclose}>
          <svg
            viewBox="0 0 16 16"
            width="16"
            height="16"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            aria-hidden="true"
          >
            <path d="m4 4 8 8M12 4l-8 8" />
          </svg>
        </button>
      </header>

      <div class="stage">
        {#if records.length > 1}
          <button type="button" class="nav prev" aria-label="Previous capture" onclick={() => go(-1)}>
            <svg
              viewBox="0 0 16 16"
              width="18"
              height="18"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <path d="M10 3 5 8l5 5" />
            </svg>
          </button>
        {/if}

        {#if !hasFile}
          <div class="explain">
            <svg
              viewBox="0 0 24 24"
              width="26"
              height="26"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <rect x="3" y="4.5" width="18" height="15" rx="2.5" />
              <path d="M3 3l18 18" />
            </svg>
            <h3>Nothing on disk to preview</h3>
            <p>
              This capture was taken with <strong>Save to disk</strong> turned off, so only the
              thumbnail and its details were kept. Turn the setting on in Settings &rsaquo; Capture
              to keep future files.
            </p>
          </div>
        {:else if imgBroken}
          <div class="explain">
            <h3>File is missing</h3>
            <p class="path">{record.path}</p>
            <p>
              The {isRecording ? 'recording' : 'image'} was moved or deleted outside santi.sharex.
            </p>
          </div>
        {:else if playsAsVideo}
          <!-- A recording is played, not rendered as a picture (M4 §5). Keyed on
               `fullSrc` so navigating between records tears the element down
               instead of leaving the previous clip's decoder attached to a new
               `src`. `preload="metadata"` so the duration and first frame are
               there without pulling the whole file for a clip nobody plays. -->
          {#key fullSrc}
            <!-- svelte-ignore a11y_media_has_caption -->
            <video
              class="full"
              src={fullSrc}
              poster={record.thumb ? versionedAssetUrl(record.thumb, record.sizeBytes) : undefined}
              controls
              preload="metadata"
              onerror={() => (imgBroken = true)}
            ></video>
          {/key}
        {:else}
          <img
            class="full"
            src={fullSrc}
            alt={record.name}
            onerror={() => (imgBroken = true)}
          />
        {/if}

        {#if records.length > 1}
          <button type="button" class="nav next" aria-label="Next capture" onclick={() => go(1)}>
            <svg
              viewBox="0 0 16 16"
              width="18"
              height="18"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <path d="m6 3 5 5-5 5" />
            </svg>
          </button>
        {/if}
      </div>

      {#if uploading}
        <!-- Between the picture and the buttons, where the eye already is: a
             20 MB capture on slow upstream is otherwise a frozen dialog. -->
        <div class="upload-bar" role="status">
          <div class="track">
            <div
              class="bar"
              class:indeterminate={percent === null}
              style={percent === null ? undefined : `width: ${percent}%`}
            ></div>
          </div>
          <span class="upload-text num">
            {percent === null ? 'Uploading…' : `Uploading — ${percent}%`}
          </span>
        </div>
      {/if}

      <footer class="foot">
        <span class="counter num">{index + 1} of {records.length}</span>
        <div class="tools">
          <!-- Edit and Copy are image operations, so a recording is refused
               rather than half-supported (M4 §5): the editor would open on a
               canvas it cannot fill, and a clipboard "image" of a video is its
               first frame, which is a picture the user never took. -->
          <button
            type="button"
            class="btn"
            disabled={!hasFile || isRecording}
            title={isRecording
              ? 'The editor works on still images. This capture is a screen recording.'
              : undefined}
            onclick={doEdit}
          >
            Edit
          </button>
          <button
            type="button"
            class="btn"
            disabled={!hasFile || isRecording}
            title={isRecording
              ? 'A recording cannot go on the clipboard as an image. Use Show in folder and copy the file.'
              : undefined}
            onclick={doCopy}
          >
            Copy
          </button>
          <button type="button" class="btn" disabled={!hasFile} onclick={doOpen}>Open</button>
          <button type="button" class="btn" disabled={!hasFile} onclick={doReveal}>
            Show in folder
          </button>
          <!-- Not offered at all for a recording (M4 §5) — there is no page of
               text in a video, and a disabled button would still suggest there
               might be. Disabled without a file for the same reason as the rest:
               OCR reads the PNG off disk, and `saveToDisk` off means there is
               none. -->
          {#if !isRecording}
            <button
              type="button"
              class="btn"
              disabled={!hasFile}
              title="Read the text out of this capture"
              onclick={() => (ocrOpen = true)}
            >
              Extract text
            </button>
          {/if}

          <!-- Opt-in per capture (M3 §1): pressing this is the only reason a
               capture opened here ever leaves the machine. -->
          {#if uploading}
            <button type="button" class="btn" onclick={doCancelUpload}>Cancel upload</button>
          {:else}
            <button
              type="button"
              class="btn"
              disabled={!hasFile || !canUpload || uploadingId !== null}
              title={!hasFile
                ? 'This capture was not saved to disk, so there is no file to upload.'
                : canUpload
                  ? 'Send this capture to the configured destination'
                  : destination === 'none'
                    ? 'No upload destination is configured. Settings › Destinations.'
                    : `The configured destination does not accept ${captureExtension(record).toUpperCase() || 'this'} files.`}
              onclick={doUpload}
            >
              Upload
            </button>
          {/if}

          {#if record.url}
            <button
              type="button"
              class="btn"
              title={record.url}
              onclick={doCopyLink}
            >
              Copy link
            </button>
          {/if}

          <button type="button" class="btn btn-danger" onclick={doDelete}>Delete</button>
        </div>
      </footer>
    </div>
  </div>

  <!-- Outside the dialog on purpose: the lightbox traps Tab and reads the arrow
       keys on `dialogEl`, and the panel is the modal on top now. -->
  {#if ocrOpen && hasFile && !isRecording}
    <OcrPanel {record} onclose={() => (ocrOpen = false)} />
  {/if}
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 60;
    display: grid;
    place-items: center;
    padding: 32px;
    background: color-mix(in srgb, var(--bg) 84%, transparent);
    backdrop-filter: var(--blur);
  }

  .dialog {
    display: flex;
    flex-direction: column;
    width: min(1080px, 100%);
    max-height: 100%;
    background: var(--bg-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow);
    overflow: hidden;
  }

  .dialog:focus {
    outline: none;
  }

  .head {
    display: flex;
    align-items: flex-start;
    gap: 16px;
    padding: 16px 16px 12px 20px;
    border-bottom: 1px solid var(--border);
  }

  .titles {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .name {
    margin: 0;
    font-family: var(--font-display);
    font-size: 15px;
    font-weight: 600;
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .meta {
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    color: var(--text-faint);
  }

  .kind {
    text-transform: capitalize;
  }

  .sep {
    opacity: 0.6;
  }

  .num {
    font-variant-numeric: tabular-nums;
  }

  .stage {
    position: relative;
    flex: 1;
    min-height: 0;
    display: grid;
    place-items: center;
    padding: 20px;
    background: var(--bg-inset);
  }

  .full {
    max-width: 100%;
    max-height: min(64vh, 720px);
    object-fit: contain;
    border-radius: var(--radius);
    background: var(--bg-inset);
  }

  .explain {
    max-width: 380px;
    padding: 40px 8px;
    text-align: center;
    color: var(--text-dim);
  }

  .explain h3 {
    margin: 12px 0 6px;
    font-family: var(--font-display);
    font-size: 15px;
    font-weight: 600;
    color: var(--text);
  }

  .explain p {
    margin: 0;
    font-size: 12px;
    line-height: 1.6;
  }

  .explain .path {
    margin-bottom: 8px;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-faint);
    word-break: break-all;
  }

  .nav {
    position: absolute;
    top: 50%;
    transform: translateY(-50%);
    width: 34px;
    height: 34px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: 1px solid var(--border);
    border-radius: 50%;
    background: color-mix(in srgb, var(--bg-raised) 90%, transparent);
    color: var(--text-dim);
    cursor: pointer;
    transition:
      color 140ms ease,
      border-color 140ms ease,
      background-color 140ms ease;
  }

  .nav:hover {
    color: var(--text);
    border-color: var(--border-strong);
    background: var(--bg-raised);
  }

  .nav:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .prev {
    left: 12px;
  }

  .next {
    right: 12px;
  }

  .upload-bar {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 16px;
    border-top: 1px solid var(--border);
    background: var(--bg-inset);
  }

  .track {
    flex: 1;
    min-width: 0;
    height: 4px;
    overflow: hidden;
    border-radius: 999px;
    background: var(--border);
  }

  .bar {
    height: 100%;
    width: 0;
    border-radius: 999px;
    background: var(--accent);
    transition: width 160ms linear;
  }

  /* No known total: a sliding sliver says "working" rather than claiming a
     percentage the destination never reported. */
  .bar.indeterminate {
    width: 30%;
    animation: slide 1.1s ease-in-out infinite;
  }

  @keyframes slide {
    0% {
      transform: translateX(-100%);
    }
    100% {
      transform: translateX(340%);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .bar {
      transition: none;
    }

    .bar.indeterminate {
      width: 100%;
      animation: none;
      opacity: 0.5;
    }
  }

  .upload-text {
    flex: none;
    font-size: 12px;
    color: var(--text-dim);
  }

  .foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 12px 16px;
    border-top: 1px solid var(--border);
  }

  .counter {
    font-size: 12px;
    color: var(--text-faint);
  }

  .tools {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }

  .btn {
    height: 34px;
    padding: 0 12px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    font: inherit;
    font-size: 13px;
    color: var(--text);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    cursor: pointer;
    transition:
      color 140ms ease,
      background-color 140ms ease,
      border-color 140ms ease;
  }

  .btn:hover:not(:disabled) {
    border-color: var(--border-strong);
    background: var(--bg-inset);
  }

  .btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .btn:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }

  .btn-icon {
    width: 34px;
    padding: 0;
    flex: none;
    color: var(--text-dim);
  }

  .btn-danger:hover:not(:disabled) {
    color: var(--danger);
    border-color: var(--danger);
  }
</style>
