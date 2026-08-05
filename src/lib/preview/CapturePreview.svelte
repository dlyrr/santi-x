<script lang="ts">
  /**
   * The post-capture preview (M2.9 §3): a small always-on-top card in the
   * bottom-right corner of the monitor the capture came from.
   *
   * The window it lives in is borderless, `skip_taskbar`, and never takes
   * focus, so a preview that fails to hide is a sticker the user cannot click
   * away — no titlebar to close, no taskbar entry to right-click. Every exit
   * from here therefore ends in `dismiss()`, and `dismiss()` ends in Rust
   * hiding the window: the timer, the click, the ×, Escape, a thumbnail that
   * will not load, and a window that was shown before this listener was live.
   */
  import Icon from '$lib/components/Icon.svelte';
  import {
    canUploadNow,
    cancelUpload,
    destinationStatus,
    errorMessage,
    getPreviewRecord,
    hideCapturePreview,
    onCapturePreview,
    onCaptureUpdated,
    onPreviewHidden,
    onUploadError,
    onUploadProgress,
    openEditor,
    uploadCapture,
    versionedAssetUrl,
    type CaptureRecord
  } from '$lib/api';
  import { settings } from '$lib/stores/settings.svelte';

  /** How long a preview stays up when nothing interrupts it. */
  const AUTO_DISMISS_MS = 4000;

  /**
   * How often the progress line is recomputed. The line is decoration; the
   * `setTimeout` below is the authority for the dismissal itself, so a throttled
   * interval in this never-focused window can cost a stuttering bar but never a
   * preview that outlives its countdown.
   */
  const TICK_MS = 50;

  /**
   * Last resort for a window that is up with nothing to show.
   *
   * `getPreviewRecord()` below is the real catch-up for a missed
   * `preview://show`, so this no longer fires on the ordinary first capture —
   * it covers only the case where Rust has no record either and the window is
   * on screen regardless. Hiding an already-hidden window is a no-op, so the
   * normal path pays nothing.
   */
  const ORPHAN_TIMEOUT_MS = 1500;

  let record = $state<CaptureRecord | null>(null);
  let remaining = $state(AUTO_DISMISS_MS);
  let hovering = $state(false);
  let thumbBroken = $state(false);

  /**
   * Whether the active destination is genuinely ready to receive an upload —
   * chosen *and* holding the credential it needs (M3 §6).
   *
   * The stricter test, not `destination !== 'none'` as History uses, because
   * this card is on screen for four seconds: a button that fails a moment after
   * the window has gone is worse than no button. Re-read on every capture,
   * since this window outlives any number of settings changes.
   */
  let destReady = $state(false);

  let uploading = $state(false);
  let sent = $state(0);
  let total = $state(0);

  /** The last upload's outcome, in one short line. Empty until there is one. */
  let uploadNote = $state('');
  let uploadFailed = $state(false);

  /**
   * Live only while this window's own upload is in flight. Named apart from the
   * `unlisteners` inside the mount effect below, which belongs to the
   * `preview://` channel and lives for the whole session.
   */
  let uploadUnlisteners: Array<() => void> = [];

  let deadline = 0;
  let alarm: ReturnType<typeof setTimeout> | null = null;
  let ticker: ReturnType<typeof setInterval> | null = null;

  /**
   * Bumped by every `show`. A dismissal that started before a second capture
   * arrived must not clear the record that capture just installed.
   */
  let generation = 0;

  // Versioned: an edit saved over its original keeps the same thumbnail path,
  // and an `<img>` whose src never changes is never re-fetched — the preview
  // would show the pre-edit picture. `size_bytes` is rewritten by every save.
  // An <img> is safe here because nothing in this window is exported from a
  // canvas; `new Image()` on an asset URL stays forbidden everywhere (M2.5 §0).
  const thumbSrc = $derived(
    record && record.thumb ? versionedAssetUrl(record.thumb, record.sizeBytes) : ''
  );

  const fraction = $derived(Math.max(0, Math.min(1, remaining / AUTO_DISMISS_MS)));
  const percent = $derived(total > 0 ? Math.min(100, Math.round((sent / total) * 100)) : null);

  $effect(() => {
    let cancelled = false;
    const unlisteners: Array<() => void> = [];

    const track = (pending: Promise<() => void>) =>
      void pending
        .then((un) => {
          if (cancelled) un();
          else unlisteners.push(un);
        })
        .catch(() => {});

    // Both subscribed before the catch-up query below, so an event that lands
    // while that round trip is in flight is heard rather than raced past.
    track(onCapturePreview(show));
    track(onPreviewHidden(clear));
    // The record gains its `url` on the Rust side and comes back on
    // `capture://updated` — the same channel the history store adopts. Without
    // this the copy on screen keeps `url: null` for as long as the card is up,
    // which is exactly the window in which auto-upload finishes: Rust uploads
    // the capture the instant it lands, and the card that is on screen for it
    // would show nothing at all, because the Upload button is suppressed under
    // auto-upload and the "Uploaded" branch tests a field that never changed.
    track(onCaptureUpdated(adopt));

    // Rust emits `preview://show` once, just before it shows the window, and on
    // the first capture this webview is still loading when that emit goes out.
    // Reading the record back is the documented other half of that contract —
    // without it the first preview of a session is lost every time.
    void getPreviewRecord()
      .then((current) => {
        // `!record` guards the race: a `preview://show` that arrived while this
        // was in flight is the fresher of the two and owns the countdown.
        if (!cancelled && current && !record) show(current);
      })
      .catch(() => {});

    const orphan = setTimeout(() => {
      if (!record) void hideCapturePreview().catch(() => {});
    }, ORPHAN_TIMEOUT_MS);

    return () => {
      cancelled = true;
      for (const unlisten of unlisteners) unlisten();
      clearTimeout(orphan);
      stopCountdown();
      stopWatchingUpload();
    };
  });

  // This window outlives any single capture, so the pre-paint cache in
  // localStorage would keep it wearing whatever theme was current when it was
  // built. The store stamps the live theme and follows `settings://changed`.
  $effect(() => {
    void settings.load();
  });

  function show(next: CaptureRecord): void {
    generation += 1;
    thumbBroken = false;
    // A new capture is a different record: an upload started for the last one
    // keeps running in Rust, but this card stops claiming to be about it.
    resetUpload();
    record = next;
    void refreshDestination();
    // A second capture replaces the content and restarts the clock rather than
    // opening another window — but not while the pointer is resting on the card.
    if (hovering) {
      stopCountdown();
      remaining = AUTO_DISMISS_MS;
    } else {
      startCountdown();
    }
  }

  /**
   * An existing record changed in place: an upload gave it a `url`, or the
   * editor saved over it. Deliberately *not* a `show()` — the capture on screen
   * is the same one, so the countdown, the hover hold and any upload state
   * belong to it and must survive. Only the record is swapped, which is enough
   * for the thumbnail (versioned on `sizeBytes`) and the uploaded state to
   * follow.
   *
   * An update for some other capture is ignored: `capture://updated` is emitted
   * to every window, and a History upload of a month-old shot must not repaint
   * the card describing the capture that just happened.
   */
  function adopt(next: CaptureRecord): void {
    if (record?.id !== next.id) return;
    record = next;
  }

  /**
   * Rust took the window off screen without going through `dismiss()` — the
   * next capture claimed it, a settings save switched the preview off, or the
   * app is exiting.
   *
   * Bumping the generation is the load-bearing part: it retires any `dismiss()`
   * still in flight, so that dismissal cannot land later and blank a card a
   * subsequent `show` has since installed.
   */
  function clear(): void {
    generation += 1;
    stopCountdown();
    resetUpload();
    record = null;
    hovering = false;
  }

  function stopWatchingUpload(): void {
    for (const off of uploadUnlisteners) off();
    uploadUnlisteners = [];
  }

  function resetUpload(): void {
    stopWatchingUpload();
    uploading = false;
    uploadFailed = false;
    uploadNote = '';
    sent = 0;
    total = 0;
  }

  /**
   * Whether an upload can actually succeed right now. A failure here means no
   * button, not a broken one — the affordance appears only when M3 §6's "a
   * destination is configured" is genuinely true.
   */
  async function refreshDestination(): Promise<void> {
    try {
      destReady = canUploadNow(await destinationStatus());
    } catch {
      destReady = false;
    }
  }

  /**
   * Upload the capture that is on screen, because the user pressed the button
   * on it. Nothing here fires on show, on hover, or on the countdown expiring.
   *
   * The countdown stops for the duration: a card that vanishes mid-transfer
   * takes its Cancel with it.
   */
  async function upload(): Promise<void> {
    const current = record;
    if (!current || uploading) return;
    const id = current.id;

    stopCountdown();
    uploading = true;
    uploadFailed = false;
    uploadNote = '';
    sent = 0;
    total = 0;

    // Set from `upload://error`: a cancel rejects the command like any other
    // failure, and the flag is the only honest way to tell them apart.
    let cancelled = false;

    const finish = (failed: boolean, note: string) => {
      // A capture that landed while this was in flight already reset the card;
      // its state must not be overwritten by the previous one's outcome.
      if (record?.id !== id) return;
      stopWatchingUpload();
      uploading = false;
      uploadFailed = failed;
      uploadNote = note;
      // The result gets its own full countdown, so it is on screen long enough
      // to read rather than inheriting whatever was left of the first one.
      if (!hovering) startCountdown();
    };

    try {
      uploadUnlisteners = await Promise.all([
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
      finish(
        false,
        settings.current?.copyUrlAfterUpload === false ? 'Uploaded' : 'Uploaded — link copied'
      );
    } catch (err) {
      // A cancel is not a failure, so it is not painted as one.
      if (cancelled) finish(false, 'Upload cancelled');
      else finish(true, errorMessage(err));
    }
  }

  async function abortUpload(): Promise<void> {
    const current = record;
    if (!current) return;
    // Infallible on the Rust side and a no-op when nothing is running; the
    // outcome still lands through `upload()`'s own await.
    await cancelUpload(current.id);
  }

  function startCountdown(): void {
    stopCountdown();
    remaining = AUTO_DISMISS_MS;
    deadline = Date.now() + AUTO_DISMISS_MS;
    alarm = setTimeout(() => void dismiss(), AUTO_DISMISS_MS);
    ticker = setInterval(() => {
      remaining = Math.max(0, deadline - Date.now());
    }, TICK_MS);
  }

  function stopCountdown(): void {
    if (alarm) clearTimeout(alarm);
    if (ticker) clearInterval(ticker);
    alarm = null;
    ticker = null;
  }

  /** Hovering freezes the countdown where it stands; leaving starts a fresh one. */
  function hold(): void {
    hovering = true;
    stopCountdown();
  }

  function release(): void {
    hovering = false;
    // Never while an upload is running: the countdown would take the Cancel off
    // screen part-way through the transfer it belongs to.
    if (record && !uploading) startCountdown();
  }

  async function dismiss(): Promise<void> {
    const gen = generation;
    stopCountdown();
    let hidden = false;
    try {
      await hideCapturePreview();
      hidden = true;
    } catch {
      // `hideCapturePreview` already falls back to hiding the window directly,
      // so reaching here means both paths failed. Keep the card rendered: a
      // visible window showing a real capture is recoverable — the user can
      // click it — where a visible blank one is not.
    }
    // Cleared only once the window is actually gone, so a Rust `show` that
    // races this cannot reveal an emptied card.
    if (hidden && gen === generation) {
      resetUpload();
      record = null;
      hovering = false;
    }
  }

  function openInEditor(): void {
    const current = record;
    if (!current) return;
    // Fire and forget: whether the editor opens is not allowed to gate hiding
    // this window.
    void openEditor(current.id).catch(() => {});
    void dismiss();
  }

  function onKeydown(event: KeyboardEvent): void {
    // Only reachable when the window happens to have focus, which it never
    // takes on its own — the click paths above are the real dismissals.
    if (event.key !== 'Escape' || !record) return;
    event.preventDefault();
    void dismiss();
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if record}
  <div
    class="preview"
    role="group"
    aria-label="Capture preview"
    onpointerenter={hold}
    onpointerleave={release}
  >
    <button
      type="button"
      class="card"
      title="Open in the editor"
      aria-label="Open {record.name} in the editor"
      onclick={openInEditor}
    >
      <span class="shot">
        {#if thumbSrc && !thumbBroken}
          <img
            src={thumbSrc}
            alt=""
            draggable="false"
            onerror={() => {
              // A preview with nothing to preview has no reason to hold the
              // screen, and an unhidden one is the failure mode this whole
              // component is built around.
              thumbBroken = true;
              void dismiss();
            }}
          />
        {/if}
      </span>
      <span class="meta">
        <span class="name">{record.name}</span>
        <span class="dims">{record.width}&times;{record.height}</span>
      </span>
    </button>

    <!-- The upload affordance, where the user is already looking after a capture
         (M3 §6). It appears **only** when the configured destination is ready,
         and it uploads only when pressed — the preview never sends anything on
         its own, whatever the countdown does. Outside `.card` because that is a
         <button> and a button cannot contain one. -->
    {#if uploading}
      <div class="uprow">
        <div class="track" aria-hidden="true">
          <div
            class="ubar"
            class:indeterminate={percent === null}
            style={percent === null ? undefined : `width: ${percent}%`}
          ></div>
        </div>
        <span class="ustatus num" role="status">
          {percent === null ? 'Uploading…' : `${percent}%`}
        </span>
        <button type="button" class="ubtn" onclick={() => void abortUpload()}>Cancel</button>
      </div>
    {:else if uploadNote}
      <div class="uprow">
        <span class="ustatus" class:bad={uploadFailed} role="status" title={uploadNote}>
          {uploadNote}
        </span>
      </div>
      <!-- Not offered while auto-upload is on: Rust already sent this capture
           through `auto_upload_if_enabled`, and a button here would upload the
           same picture a second time. -->
    {:else if destReady && !record.url && settings.current?.autoUpload !== true}
      <div class="uprow">
        <button type="button" class="ubtn accent" onclick={() => void upload()}>
          <Icon name="upload" size={12} />
          Upload
        </button>
      </div>
    {:else if record.url}
      <div class="uprow">
        <span class="ustatus" title={record.url}>Uploaded</span>
      </div>
    {/if}

    <button type="button" class="close" aria-label="Dismiss preview" onclick={dismiss}>
      <Icon name="x" size={12} />
    </button>

    <!-- The countdown, made visible: a card that vanishes with no warning reads
         as a glitch, and the bar is also what shows that hovering paused it. -->
    <div class="progress" class:paused={hovering} aria-hidden="true">
      <div class="bar" style="transform: scaleX({fraction})"></div>
    </div>
  </div>
{/if}

<style>
  /* This window is a single edge-to-edge card, so its document must not wear
     the app shell's background. Gated on the `preview-window` class stamped in
     +page.svelte, because this component's CSS ships in the same bundle the
     main window loads and an ungated `html, body` rule would repaint the shell.

     Opaque, matching the overlay window, which is not built `transparent`
     either: were the preview window ever made transparent, this is the one
     declaration to change for genuinely rounded outer corners. */
  :global(html.preview-window),
  :global(html.preview-window body) {
    background: var(--surface);
  }

  .preview {
    position: fixed;
    inset: 0;
    display: flex;
    flex-direction: column;
    padding: 12px;
    overflow: hidden;
    color: var(--text);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow);
  }

  .card {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin: 0;
    padding: 0;
    font: inherit;
    text-align: left;
    color: inherit;
    background: none;
    border: 0;
    border-radius: var(--radius);
    cursor: pointer;
  }

  .card:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  /* The picture is the point: it takes every pixel the metadata line leaves. */
  .shot {
    flex: 1;
    min-height: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    overflow: hidden;
    background: var(--bg-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    transition: border-color 120ms ease;
  }

  .card:hover .shot {
    border-color: var(--border-strong);
  }

  /* `contain`, not `cover`: a preview that crops the capture is telling the
     user something false about what was taken. */
  img {
    display: block;
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
  }

  .meta {
    display: flex;
    align-items: baseline;
    gap: 8px;
    min-width: 0;
  }

  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 12px;
    color: var(--text);
  }

  .dims {
    flex: none;
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    color: var(--text-faint);
  }

  /* One 22px strip under the metadata line. The window is 300×200, so this is
     the whole budget: the picture gives up the height, nothing else moves. */
  .uprow {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: none;
    height: 22px;
    margin-top: 6px;
    min-width: 0;
  }

  .track {
    flex: 1;
    min-width: 0;
    height: 3px;
    overflow: hidden;
    border-radius: 999px;
    background: var(--border);
  }

  .ubar {
    height: 100%;
    width: 0;
    border-radius: 999px;
    background: var(--accent);
    transition: width 160ms linear;
  }

  /* No known total: a sliding sliver rather than a percentage nobody reported. */
  .ubar.indeterminate {
    width: 35%;
    animation: uslide 1.1s ease-in-out infinite;
  }

  @keyframes uslide {
    0% {
      transform: translateX(-100%);
    }
    100% {
      transform: translateX(290%);
    }
  }

  .ustatus {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 11px;
    color: var(--text-dim);
  }

  .ustatus.bad {
    color: var(--danger);
  }

  .ustatus.num {
    flex: none;
    font-variant-numeric: tabular-nums;
  }

  .ubtn {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex: none;
    height: 22px;
    padding: 0 8px;
    font: inherit;
    font-size: 11px;
    color: var(--text);
    background: var(--bg-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    cursor: pointer;
    transition:
      color 120ms ease,
      background-color 120ms ease,
      border-color 120ms ease;
  }

  .ubtn:hover {
    border-color: var(--border-strong);
  }

  .ubtn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  .ubtn.accent {
    color: var(--accent-text);
    background: var(--accent);
    border-color: var(--accent);
  }

  .ubtn.accent:hover {
    background: var(--accent-hover);
    border-color: var(--accent-hover);
  }

  .close {
    position: absolute;
    top: 6px;
    right: 6px;
    width: 20px;
    height: 20px;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    color: var(--text-dim);
    background: color-mix(in srgb, var(--bg-inset) 85%, transparent);
    border: 1px solid var(--border);
    border-radius: 999px;
    opacity: 0.6;
    cursor: pointer;
    transition:
      opacity 120ms ease,
      color 120ms ease,
      background-color 120ms ease;
  }

  .preview:hover .close {
    opacity: 1;
  }

  .close:hover {
    color: var(--text);
    background: var(--bg-inset);
  }

  .close:focus-visible {
    opacity: 1;
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  .progress {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    height: 2px;
    background: var(--border);
  }

  .bar {
    height: 100%;
    transform-origin: left center;
    background: var(--accent);
    transition: transform 80ms linear;
  }

  /* Held open by the pointer: the line stops and goes quiet, so the pause is
     visible rather than merely felt. */
  .progress.paused .bar {
    background: var(--text-faint);
  }

  @media (prefers-reduced-motion: reduce) {
    .bar,
    .ubar {
      transition: none;
    }

    .ubar.indeterminate {
      width: 100%;
      animation: none;
      opacity: 0.5;
    }
  }
</style>
