<script lang="ts">
  import { ask } from '@tauri-apps/plugin-dialog';
  import {
    cancelUpload,
    captureExtension,
    captureIsRecording,
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
    record: CaptureRecord;
    onopen: (record: CaptureRecord) => void;
    ondeleted?: (id: string) => void;
  }

  let { record, onopen, ondeleted }: Props = $props();

  let thumbBroken = $state(false);
  let ocrOpen = $state(false);

  let uploading = $state(false);
  let sent = $state(0);
  let total = $state(0);

  /**
   * Live only while this card's own upload is in flight.
   *
   * History renders one card per record — 82 of them here — so subscribing on
   * mount would mean hundreds of standing listeners for events almost none of
   * them will ever hear. They go up before `upload_capture` is called and come
   * down when it ends, which is also what makes the id filter cheap rather than
   * a hot path.
   */
  let unlisteners: Array<() => void> = [];

  /**
   * Set from `upload://error`, which is the authority on it. A cancel rejects
   * `upload_capture` like any other failure, and the only honest way to tell
   * the two apart is this flag — Rust is explicit that the message must not be
   * matched on.
   */
  let cancelled = false;

  const hasFile = $derived(record.saved && record.path !== '');

  /**
   * This record is a screen recording, so the image actions on this card do not
   * apply to it (M4 §5) — the same question the lightbox asks about the same
   * record. Asked by `kind`, never by extension: a GIF recording is still a
   * recording, even though the webview happily animates it in an `<img>`.
   *
   * Without this, Edit / Copy / Extract text are live buttons that end in one of
   * Rust's `could not reopen …` errors, because every one of them is
   * `image::open` on an MP4. A refusal the button itself makes, with a sentence
   * saying why, is the whole difference.
   */
  const isRecording = $derived(captureIsRecording(record));

  /**
   * A destination is *selected*. Not the same as it being ready — a missing
   * credential surfaces as the upload's own error, which names what to fix,
   * where a disabled button with no explanation would not.
   *
   * `?? 'none'` matters: a `settings.json` written before M3 has no
   * `destination` at all, and an absent field must read as "nowhere to send",
   * never as "somewhere".
   */
  const destination = $derived(settings.current?.destination ?? 'none');

  /**
   * …and that it will take this record's format (M4 §5). Still images are
   * unaffected — every destination has always taken a PNG — so this only ever
   * subtracts the button for a format the destination is going to refuse.
   */
  const canUpload = $derived(destination !== 'none' && destinationAcceptsCapture(destination, record));
  const percent = $derived(total > 0 ? Math.min(100, Math.round((sent / total) * 100)) : null);
  // Versioned: an edit saved over its original keeps the same thumbnail path,
  // and an unchanged `src` is never re-fetched (see `versionedAssetUrl`).
  const thumbSrc = $derived(
    record.thumb ? versionedAssetUrl(record.thumb, record.sizeBytes) : ''
  );

  // A rewritten capture is a new picture in the same slot, so the broken-image
  // flag must not survive it — and neither may text extracted from the picture
  // it replaced.
  $effect(() => {
    void thumbSrc;
    thumbBroken = false;
    ocrOpen = false;
  });

  // A card destroyed mid-upload — the list re-sorted, the view navigated away —
  // must not leave three listeners behind holding a closure over a dead record.
  // The upload itself keeps running; it is Rust's, not this component's.
  $effect(() => stopWatching);

  function relativeTime(ms: number): string {
    const diff = Date.now() - ms;
    if (diff < 45_000) return 'just now';
    const mins = Math.round(diff / 60_000);
    if (mins < 60) return `${mins}m ago`;
    const hours = Math.round(mins / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.round(hours / 24);
    if (days < 7) return `${days}d ago`;
    return new Date(ms).toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
  }

  function open() {
    onopen(record);
  }

  function onCardKey(event: KeyboardEvent) {
    // Enter/Space on an action button bubbles up here too — only act on the card itself.
    if (event.target !== event.currentTarget) return;
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      open();
    }
  }

  async function doCopy(event: MouseEvent) {
    event.stopPropagation();
    // The button is already disabled for a recording; this is the second half of
    // the same guard, for a keyboard or programmatic route that gets past it.
    if (isRecording) {
      toast.error(
        'A recording cannot go on the clipboard as an image. Use Show in folder and copy the file.'
      );
      return;
    }
    try {
      await copyCapture(record.id);
      toast.success('Copied to clipboard');
    } catch (err) {
      toast.error(String(err));
    }
  }

  async function doEdit(event: MouseEvent) {
    event.stopPropagation();
    if (isRecording) {
      toast.error('The editor works on still images. This capture is a screen recording.');
      return;
    }
    try {
      await openEditor(record.id);
    } catch (err) {
      toast.error(String(err));
    }
  }

  async function doOpen(event: MouseEvent) {
    event.stopPropagation();
    try {
      await openCapture(record.id);
    } catch (err) {
      toast.error(String(err));
    }
  }

  async function doReveal(event: MouseEvent) {
    event.stopPropagation();
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
    uploading = false;
    sent = 0;
    total = 0;
    stopWatching();
  }

  /**
   * The one action in this card that puts the capture on the network, and it
   * only ever happens because the user pressed this button.
   *
   * The listeners go up *before* the command: `upload_capture` is outstanding
   * for the whole transfer, so a subscription made after the await would never
   * be made at all.
   *
   * The record's own `url` arrives on `capture://updated`, which the history
   * store adopts, so the resolved record is not needed here.
   */
  async function doUpload(event: MouseEvent) {
    event.stopPropagation();
    if (uploading) return;
    const id = record.id;

    uploading = true;
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
      // A cancel is not a failure — the user asked for it, and a red toast for
      // getting what you asked for is a bug.
      if (stopped) toast.info('Upload cancelled');
      // Verbatim otherwise: it is the only thing that says whether to retry,
      // fix a credential, or wait out a rate limit.
      else toast.error(errorMessage(err));
    }
  }

  /**
   * A real stop, not a UI reset: Rust fails the payload read mid-chunk, so the
   * request ends on the wire. The card stays in its uploading state until
   * `upload_capture` returns, so a cancel that races the final byte still tells
   * the truth about which one won.
   */
  async function doCancelUpload(event: MouseEvent) {
    event.stopPropagation();
    await cancelUpload(record.id);
  }

  /**
   * Through `writeClipboardText`, not `navigator.clipboard` directly: the async
   * Clipboard API rejects when the document does not have focus, and a card in
   * a window that just lost focus is exactly where this button gets pressed.
   */
  async function doCopyLink(event: MouseEvent) {
    event.stopPropagation();
    const url = record.url;
    if (!url) return;
    try {
      await writeClipboardText(url);
      toast.success('Link copied');
    } catch (err) {
      toast.error(errorMessage(err));
    }
  }

  // No round trip of its own: the panel runs `ocr_capture` itself, so it can own
  // the working state instead of the card holding a spinner it cannot explain.
  function doExtractText(event: MouseEvent) {
    event.stopPropagation();
    if (isRecording) return;
    ocrOpen = true;
  }

  async function doDelete(event: MouseEvent) {
    event.stopPropagation();
    const message = hasFile
      ? `Delete "${record.name}"? The file will be removed from disk.`
      : `Remove "${record.name}" from history?`;
    const confirmed = await ask(message, { title: 'Delete capture', kind: 'warning' });
    if (!confirmed) return;
    // The store reports failures through `error` rather than rejecting.
    const ok = await history.remove(record.id, hasFile);
    if (!ok) {
      toast.error(history.error ?? 'Could not delete the capture');
      return;
    }
    ondeleted?.(record.id);
    toast.success(hasFile ? 'Capture deleted' : 'Removed from history');
  }
</script>

<div
  class="card"
  class:uploading
  role="button"
  tabindex="0"
  aria-label="Open {record.name}"
  onclick={open}
  onkeydown={onCardKey}
>
  <div class="thumb">
    {#if thumbSrc && !thumbBroken}
      <img src={thumbSrc} alt="" loading="lazy" onerror={() => (thumbBroken = true)} />
    {:else}
      <div class="fallback" aria-hidden="true">
        <svg
          viewBox="0 0 24 24"
          width="22"
          height="22"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
        >
          <rect x="3" y="4.5" width="18" height="15" rx="2.5" />
          <path d="m4 17 5-5 4 4 2.5-2.5L20 17" />
          <path d="M3 3l18 18" />
        </svg>
        <span>No preview</span>
      </div>
    {/if}

    <div class="actions">
      <!-- Edit and Copy are image operations, so a recording is refused rather
           than left to fail in Rust (M4 §5): the editor would open on a canvas
           it cannot fill, and a clipboard "image" of a video is its first frame
           at best. -->
      <button
        type="button"
        class="act"
        title={isRecording ? 'The editor works on still images. This capture is a screen recording.' : 'Edit'}
        aria-label="Edit {record.name}"
        disabled={!hasFile || isRecording}
        onclick={doEdit}
      >
        <svg
          viewBox="0 0 16 16"
          width="16"
          height="16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M11.4 2.6a1.25 1.25 0 0 1 1.77 0l.23.23a1.25 1.25 0 0 1 0 1.77L6.1 12l-2.85.75L4 9.9z" />
          <path d="m10.5 3.5 2 2" />
        </svg>
      </button>

      <button
        type="button"
        class="act"
        title={isRecording
          ? 'A recording cannot go on the clipboard as an image. Use Show in folder and copy the file.'
          : 'Copy to clipboard'}
        aria-label="Copy {record.name} to clipboard"
        disabled={!hasFile || isRecording}
        onclick={doCopy}
      >
        <svg
          viewBox="0 0 16 16"
          width="16"
          height="16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <rect x="5.75" y="5.75" width="8.5" height="8.5" rx="2" />
          <path d="M10.25 3.75v-.5a1.5 1.5 0 0 0-1.5-1.5h-5a1.5 1.5 0 0 0-1.5 1.5v5a1.5 1.5 0 0 0 1.5 1.5h.5" />
        </svg>
      </button>

      <button
        type="button"
        class="act"
        title="Open in default viewer"
        aria-label="Open {record.name}"
        disabled={!hasFile}
        onclick={doOpen}
      >
        <svg
          viewBox="0 0 16 16"
          width="16"
          height="16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M9.75 2.25h4v4" />
          <path d="M13.75 2.25 7.5 8.5" />
          <path d="M12.75 9.5v2.75a1.5 1.5 0 0 1-1.5 1.5h-7.5a1.5 1.5 0 0 1-1.5-1.5v-7.5a1.5 1.5 0 0 1 1.5-1.5H6.5" />
        </svg>
      </button>

      <button
        type="button"
        class="act"
        title="Show in folder"
        aria-label="Show {record.name} in folder"
        disabled={!hasFile}
        onclick={doReveal}
      >
        <svg
          viewBox="0 0 16 16"
          width="16"
          height="16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M2 5.25A1.75 1.75 0 0 1 3.75 3.5h2.4c.47 0 .92.19 1.24.53l.86.89h4A1.75 1.75 0 0 1 14 6.67v5.58A1.75 1.75 0 0 1 12.25 14h-8.5A1.75 1.75 0 0 1 2 12.25z" />
        </svg>
      </button>

      <!-- Uploading is opt-in per capture (M3 §1): this button is the only way
           a capture in History leaves the machine, and while one is in flight it
           becomes the Cancel for that same upload rather than sitting beside it. -->
      {#if uploading}
        <button
          type="button"
          class="act"
          title="Cancel upload"
          aria-label="Cancel uploading {record.name}"
          onclick={doCancelUpload}
        >
          <svg
            viewBox="0 0 16 16"
            width="16"
            height="16"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <rect x="4.75" y="4.75" width="6.5" height="6.5" rx="1.5" />
          </svg>
        </button>
      {:else}
        <button
          type="button"
          class="act"
          title={!hasFile
            ? 'That capture was not saved to disk, so there is no file to upload.'
            : canUpload
              ? 'Upload to the configured destination'
              : destination === 'none'
                ? 'No upload destination is configured. Settings › Destinations.'
                : `The configured destination does not accept ${captureExtension(record).toUpperCase() || 'this'} files.`}
          aria-label="Upload {record.name}"
          disabled={!hasFile || !canUpload}
          onclick={doUpload}
        >
          <svg
            viewBox="0 0 16 16"
            width="16"
            height="16"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <path d="M8 2.75v7.5" />
            <path d="m5 5.75 3-3 3 3" />
            <path d="M2.75 11.25v1a1.5 1.5 0 0 0 1.5 1.5h7.5a1.5 1.5 0 0 0 1.5-1.5v-1" />
          </svg>
        </button>
      {/if}

      {#if record.url}
        <button
          type="button"
          class="act"
          title="Copy link"
          aria-label="Copy the link to {record.name}"
          onclick={doCopyLink}
        >
          <svg
            viewBox="0 0 16 16"
            width="16"
            height="16"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <path d="M6.75 9.25a2.5 2.5 0 0 0 3.54 0l1.96-1.96a2.5 2.5 0 0 0-3.54-3.54l-.9.9" />
            <path d="M9.25 6.75a2.5 2.5 0 0 0-3.54 0L3.75 8.71a2.5 2.5 0 0 0 3.54 3.54l.9-.9" />
          </svg>
        </button>
      {/if}

      <!-- OCR reads the PNG off disk, so this is dead without one, exactly like
           the four actions above it. Not offered at all for a recording (M4 §5)
           — there is no page of text in a video, and a disabled button would
           still suggest there might be. -->
      {#if !isRecording}
        <button
          type="button"
          class="act"
          title="Extract text"
          aria-label="Extract text from {record.name}"
          disabled={!hasFile}
          onclick={doExtractText}
        >
          <svg
            viewBox="0 0 16 16"
            width="16"
            height="16"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <path d="M2.5 5.75v-2.25a1 1 0 0 1 1-1h2.25" />
            <path d="M10.25 2.5h2.25a1 1 0 0 1 1 1v2.25" />
            <path d="M13.5 10.25v2.25a1 1 0 0 1-1 1h-2.25" />
            <path d="M5.75 13.5H3.5a1 1 0 0 1-1-1v-2.25" />
            <path d="M5.25 6.5h5.5" />
            <path d="M5.25 9.5h3.5" />
          </svg>
        </button>
      {/if}

      <button
        type="button"
        class="act danger"
        title="Delete"
        aria-label="Delete {record.name}"
        onclick={doDelete}
      >
        <svg
          viewBox="0 0 16 16"
          width="16"
          height="16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M2.75 4.5h10.5" />
          <path d="M6.25 4.5V3.4A1.4 1.4 0 0 1 7.65 2h.7a1.4 1.4 0 0 1 1.4 1.4v1.1" />
          <path d="m4.4 4.5.58 8.1A1.5 1.5 0 0 0 6.48 14h3.04a1.5 1.5 0 0 0 1.5-1.4l.58-8.1" />
        </svg>
      </button>
    </div>

    {#if !record.saved}
      <span class="badge" title="This capture was not written to disk">Not saved</span>
    {/if}

    {#if record.url && !uploading}
      <span class="badge badge-right" title={record.url}>Uploaded</span>
    {/if}

    <!-- A 20 MB capture on slow upstream is tens of seconds of nothing without
         this. The bar is indeterminate when the destination streams without a
         known length, which is a real state rather than 0%. -->
    {#if uploading}
      <div class="progress" role="status" aria-label="Uploading {record.name}">
        <div class="track">
          <div
            class="bar"
            class:indeterminate={percent === null}
            style={percent === null ? undefined : `width: ${percent}%`}
          ></div>
        </div>
        <span class="progress-text num">
          {percent === null ? 'Uploading…' : `${percent}%`}
        </span>
      </div>
    {/if}
  </div>

  <div class="foot">
    <span class="name" title={record.name}>{record.name}</span>
    <span class="meta">
      <span class="num">{record.width}&times;{record.height}</span>
      <!-- The thumbnail of a recording is a frame from the middle of the clip,
           which is to say it looks exactly like a screenshot. The run time is
           the one thing in this row that says otherwise. -->
      {#if record.durationMs}
        <span class="dot" aria-hidden="true">&middot;</span>
        <span class="num">{formatDuration(record.durationMs)}</span>
      {/if}
      <span class="dot" aria-hidden="true">&middot;</span>
      <span>{relativeTime(record.createdAt)}</span>
    </span>
  </div>
</div>

<!-- Portals itself out to <body>, so the card's hover lift cannot become the
     containing block for its fixed scrim. -->
{#if ocrOpen && hasFile && !isRecording}
  <OcrPanel {record} onclose={() => (ocrOpen = false)} />
{/if}

<style>
  .card {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--surface);
    overflow: hidden;
    cursor: pointer;
    text-align: left;
    transition:
      border-color 140ms ease,
      background-color 140ms ease,
      transform 140ms ease;
  }

  .card:hover {
    border-color: var(--border-strong);
    transform: translateY(-1px);
  }

  .card:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .thumb {
    position: relative;
    aspect-ratio: 16 / 10;
    background: var(--bg-inset);
    border-bottom: 1px solid var(--border);
    overflow: hidden;
  }

  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }

  .fallback {
    width: 100%;
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 6px;
    color: var(--text-faint);
    font-size: 12px;
  }

  .badge {
    position: absolute;
    left: 8px;
    top: 8px;
    padding: 2px 7px;
    border-radius: 999px;
    font-size: 11px;
    line-height: 16px;
    color: var(--text-dim);
    background: color-mix(in srgb, var(--bg) 80%, transparent);
    border: 1px solid var(--border);
    backdrop-filter: var(--blur);
  }

  .badge-right {
    left: auto;
    right: 8px;
    color: var(--success);
    border-color: color-mix(in srgb, var(--success) 45%, transparent);
  }

  /* Pinned to the bottom edge; the action row lifts clear of it below, so the
     bar never covers the Cancel button it belongs to. */
  .progress {
    position: absolute;
    inset: auto 0 0 0;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px;
    background: color-mix(in srgb, var(--bg) 82%, transparent);
    backdrop-filter: var(--blur);
  }

  .track {
    flex: 1;
    min-width: 0;
    height: 3px;
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

  /* No known total: a sliding sliver says "working" without claiming a
     percentage the destination never reported. */
  .bar.indeterminate {
    width: 35%;
    animation: slide 1.1s ease-in-out infinite;
  }

  @keyframes slide {
    0% {
      transform: translateX(-100%);
    }
    100% {
      transform: translateX(290%);
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

  .progress-text {
    flex: none;
    font-size: 11px;
    color: var(--text-dim);
  }

  .actions {
    position: absolute;
    inset: auto 0 0 0;
    z-index: 2;
    display: flex;
    gap: 4px;
    padding: 8px;
    justify-content: flex-end;
    background: linear-gradient(
      to top,
      color-mix(in srgb, var(--bg) 88%, transparent),
      transparent
    );
    opacity: 0;
    transition: opacity 140ms ease;
  }

  .card:hover .actions,
  .card:focus-within .actions,
  /* An upload in flight must keep its Cancel reachable without hovering —
     a cancel you have to find is not a cancel. */
  .card.uploading .actions {
    opacity: 1;
  }

  /* Room for the progress strip pinned below it. */
  .card.uploading .actions {
    padding-bottom: 36px;
  }

  .act {
    width: 28px;
    height: 28px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: color-mix(in srgb, var(--bg-raised) 92%, transparent);
    color: var(--text-dim);
    cursor: pointer;
    transition:
      color 140ms ease,
      background-color 140ms ease,
      border-color 140ms ease;
  }

  .act:hover:not(:disabled) {
    color: var(--text);
    border-color: var(--border-strong);
    background: var(--bg-raised);
  }

  .act.danger:hover:not(:disabled) {
    color: var(--danger);
    border-color: var(--danger);
  }

  .act:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
    opacity: 1;
  }

  .act:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .foot {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 10px 12px 12px;
    min-width: 0;
  }

  .name {
    font-size: 13px;
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .meta {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    color: var(--text-faint);
  }

  .num {
    font-variant-numeric: tabular-nums;
  }

  .dot {
    opacity: 0.6;
  }
</style>
