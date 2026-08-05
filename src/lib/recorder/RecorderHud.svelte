<script lang="ts">
  /**
   * The recording HUD (M4 §4): a small always-on-top card that says what is
   * being recorded, how long it has been going, and how to stop it.
   *
   * The window it lives in is borderless, `skip_taskbar` and **never takes
   * focus**, which is what turns two pieces of polish into rules:
   *
   * 1. *Stop must be reachable.* The button here is one of three routes, beside
   *    the global stop hotkey and the tray item, and each has to work when the
   *    other two do not. So the hotkey is printed on the card — the window
   *    cannot be alt-tabbed to and the button is not always where the pointer
   *    is — and it says so plainly when nothing claimed the combination.
   * 2. *The card must never be stranded.* An unfocusable always-on-top window
   *    with no titlebar cannot be closed by the user. Rust owns hiding it and
   *    hides it on every ending — stop, cancel, failure, exit. This side adds
   *    the one case Rust cannot see: the window is visible and nothing is
   *    recording, which ends in the window hiding itself. That check is armed
   *    per *recording*, not per mount: this window is built once and reused, so
   *    a one-shot timer would guard the first recording of the session and
   *    nothing after it.
   *
   * Everything it renders comes from one event, `record://status`, plus a
   * `recordingStatus()` read on mount for the first recording of a session,
   * which happens while this webview is still being built.
   */
  import {
    cancelRecording,
    errorMessage,
    formatDuration,
    hideRecorderWindow,
    isRecordFormat,
    onRecordStatus,
    RECORD_FORMAT_LABEL,
    recordingStatus,
    stopRecording,
    type RecordStatus
  } from '$lib/api';
  import { settings } from '$lib/stores/settings.svelte';

  /**
   * How often the clock is redrawn. Rust's `record://status` arrives about four
   * times a second and is the authority; this only interpolates between events
   * so the seconds never appear to stall.
   */
  const TICK_MS = 200;

  /**
   * How long a visible HUD may sit with nothing recording before it hides
   * itself. Generous, because Rust shows the window a beat before the first
   * status reaches this webview and cutting that short would take the HUD off
   * screen at the exact moment the recording begins.
   */
  const STRANDED_TIMEOUT_MS = 2500;

  let status = $state<RecordStatus | null>(null);

  /** When the last status landed, so the clock can run on between them. */
  let statusAt = $state(0);
  let now = $state(0);

  /** Which ending this card asked for. Both buttons go dead for either. */
  let ending = $state<'stop' | 'cancel' | null>(null);

  /** A stop or cancel that was refused. The recording is still running. */
  let problem = $state('');

  const live = $derived(status !== null && status.active);
  const elapsed = $derived(
    status === null ? 0 : status.elapsedMs + (statusAt === 0 ? 0 : Math.max(0, now - statusAt))
  );

  const format = $derived(
    status && isRecordFormat(status.format)
      ? RECORD_FORMAT_LABEL[status.format]
      : (status?.format ?? '')
  );

  const source = $derived(
    status ? status.source.charAt(0).toUpperCase() + status.source.slice(1) : ''
  );

  /**
   * Past the point where stopping is still a choice: the frames are in and
   * ffmpeg is finishing the file, which for a two-pass GIF is a real wait. The
   * buttons come off rather than sitting there disabled, because there is
   * nothing left to press them for.
   */
  const closing = $derived(
    status !== null && (status.phase === 'finishing' || status.phase === 'encoding')
  );

  const phaseLabel = $derived(
    status?.phase === 'encoding'
      ? 'Encoding the file…'
      : status?.phase === 'finishing'
        ? 'Finishing the recording…'
        : ''
  );

  /**
   * The stop hotkey as keys rather than as an accelerator. Empty when nothing
   * claimed the combination, which the markup turns into a sentence rather than
   * an empty chip — a HUD advertising a shortcut that does nothing would be
   * worse than one admitting the button is the only way.
   */
  const stopKeys = $derived(
    (status?.stopHotkey ?? '')
      .split('+')
      .map((part) => {
        const key = part.trim();
        if (key === 'CmdOrCtrl' || key === 'CommandOrControl') return 'Ctrl';
        if (key === 'Super' || key === 'Meta') return 'Win';
        if (key === 'PrintScreen') return 'PrtScn';
        return key;
      })
      .filter((key) => key !== '')
      .join(' + ')
  );

  $effect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    // Subscribed before the catch-up read below, so a status that lands while
    // that round trip is in flight is heard rather than raced past.
    void onRecordStatus(adopt)
      .then((un) => {
        if (cancelled) un();
        else unlisten = un;
      })
      .catch(() => {});

    // Rust emits the opening status once, immediately after it shows the
    // window, and on the first recording of a session this webview is still
    // loading when that goes out. Reading it back is the documented other half
    // of that contract — without it the first HUD of a session is blank.
    void recordingStatus()
      .then((current) => {
        if (!cancelled && !status) adopt(current);
      })
      .catch(() => {});

    return () => {
      cancelled = true;
      unlisten?.();
    };
  });

  /**
   * The stranded check, re-armed for **every** recording rather than once.
   *
   * This window is built once and reused forever, so this component mounts once
   * and never again: a single timer started at mount would guard the first
   * recording of the session and nothing after it. The second recording is
   * exactly as capable of leaving an unfocusable always-on-top card on screen as
   * the first, and by then there would be no timer left to notice.
   *
   * Armed whenever nothing is recording — which includes the moments before the
   * first status arrives, hence the generous timeout and the re-read inside
   * `hideIfStranded` — and torn down the instant one is.
   */
  $effect(() => {
    if (live) return;
    const stranded = setTimeout(() => void hideIfStranded(), STRANDED_TIMEOUT_MS);
    return () => clearTimeout(stranded);
  });

  // The clock only has to move while there is a clock. Left running it would
  // re-render a hidden window five times a second for the life of the app.
  $effect(() => {
    if (!live) return;
    const ticker = setInterval(() => (now = Date.now()), TICK_MS);
    return () => clearInterval(ticker);
  });

  // This window outlives any single recording, so the pre-paint cache in
  // localStorage would keep it wearing whatever theme was current when it was
  // built. The store stamps the live theme and follows `settings://changed`.
  $effect(() => {
    void settings.load();
  });

  function adopt(next: RecordStatus): void {
    // A different recording is a fresh card: an ending asked for on the last one
    // is not pending on this one.
    if (next.id !== status?.id) {
      ending = null;
      problem = '';
    }
    status = next.active ? next : null;
    statusAt = next.active ? Date.now() : 0;
    now = Date.now();
    if (!next.active) ending = null;
  }

  /**
   * The one state Rust cannot see: this window is on screen and nothing is
   * recording. Re-read rather than trusting local state — a status that has not
   * arrived yet is not the same as no recording, and hiding the HUD out from
   * under a live one is the failure this whole component is built around. A
   * failed read leaves the card up, which is the safe direction.
   */
  async function hideIfStranded(): Promise<void> {
    if (live) return;
    try {
      const current = await recordingStatus();
      if (current.active) {
        adopt(current);
        return;
      }
      await hideRecorderWindow();
    } catch {
      // Better a stale card than one hidden over a running recording.
    }
  }

  /**
   * Finish and keep. The window stays up until Rust hides it — encoding a
   * two-pass GIF takes a moment, and a card that vanished before the file
   * existed would look like the recording had been thrown away.
   */
  async function stop(): Promise<void> {
    if (ending) return;
    ending = 'stop';
    problem = '';
    try {
      await stopRecording();
    } catch (err) {
      // Rust refuses when nothing is recording, which is the ordinary race with
      // a recording that has just ended — and it will hide this window itself
      // in that case. Anything else means the recording is still going, so the
      // buttons go back rather than leaving a dead card over a live capture.
      ending = null;
      problem = errorMessage(err);
    }
  }

  async function discard(): Promise<void> {
    if (ending) return;
    ending = 'cancel';
    problem = '';
    try {
      await cancelRecording();
    } catch (err) {
      ending = null;
      problem = errorMessage(err);
    }
  }
</script>

{#if status}
  <div class="hud" role="group" aria-label="Screen recording in progress">
    <div class="top">
      <span class="live">
        <span class="dot" class:paused={closing} aria-hidden="true"></span>
        <span class="clock num">{formatDuration(elapsed)}</span>
      </span>
      <span class="meta">
        <span class="line">
          {#if source}{source}<span class="sep" aria-hidden="true">&middot;</span>{/if}
          <span class="num">{status.width}&times;{status.height}</span>
          <span class="sep" aria-hidden="true">&middot;</span>
          <span class="num">{status.fps} fps</span>
          {#if format}<span class="sep" aria-hidden="true">&middot;</span>{format}{/if}
        </span>
        <!-- Dropped frames are expected, not a fault (M4 §3) — silence about
             them is. Shown only once there are any: a permanent "0 dropped" is a
             warning that never fires. -->
        {#if problem}
          <span class="line bad">{problem}</span>
        {:else if status.dropped > 0}
          <span class="line bad">
            <span class="num">{status.dropped}</span>
            {status.dropped === 1 ? 'frame' : 'frames'} dropped
          </span>
        {:else if status.hardware}
          <span class="line faint">GPU encode</span>
        {/if}
      </span>
    </div>

    <div class="controls">
      {#if closing}
        <span class="phase" role="status">{phaseLabel}</span>
      {:else}
        <button
          type="button"
          class="btn accent"
          disabled={ending !== null}
          title="Finish the recording and keep the file"
          onclick={() => void stop()}
        >
          {ending === 'stop' ? 'Stopping…' : 'Stop'}
        </button>
        <button
          type="button"
          class="btn"
          disabled={ending !== null}
          title="Throw the recording away"
          onclick={() => void discard()}
        >
          {ending === 'cancel' ? 'Discarding…' : 'Cancel'}
        </button>
      {/if}

      <!-- The window cannot take focus, so the button is not always reachable.
           The hotkey is the route that always is, which is why it is printed
           here rather than left to the settings screen. -->
      {#if stopKeys}
        <kbd class="kbd" title="Stops the recording from anywhere, even while this card cannot be clicked">
          {stopKeys}
        </kbd>
      {:else}
        <span class="hint">No stop hotkey — use the tray</span>
      {/if}
    </div>
  </div>
{/if}

<style>
  /* This window is a single edge-to-edge card, so its document must not wear the
     app shell's background. Gated on the `recorder-window` class stamped in
     +page.svelte, because this component's CSS ships in the same bundle the main
     window loads and an ungated `html, body` rule would repaint the shell. Same
     arrangement, and same opacity reasoning, as the capture preview. */
  :global(html.recorder-window),
  :global(html.recorder-window body) {
    background: var(--surface);
  }

  /* 340x96 logical, and the layout has to hold at exactly that: two rows, the
     second pinned to the bottom. Nothing here may grow a third. */
  .hud {
    position: fixed;
    inset: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px 12px;
    overflow: hidden;
    color: var(--text);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow);
  }

  .top {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 10px;
    min-width: 0;
  }

  .live {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    flex: none;
  }

  /* The one thing that has to read at a glance from across the desk. */
  .dot {
    width: 8px;
    height: 8px;
    flex: none;
    border-radius: 50%;
    background: var(--danger);
    animation: pulse 1.6s ease-in-out infinite;
  }

  /* Nothing is being captured any more — the file is being written. The mark
     stops rather than continuing to claim the screen is being recorded. */
  .dot.paused {
    background: var(--text-faint);
    animation: none;
  }

  @keyframes pulse {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.35;
    }
  }

  .clock {
    font-family: var(--font-display);
    font-size: 19px;
    font-weight: 600;
    line-height: 1.05;
    letter-spacing: -0.01em;
  }

  .meta {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 1px;
    min-width: 0;
    font-size: 11px;
    line-height: 1.4;
    color: var(--text-dim);
  }

  .line {
    max-width: 100%;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .line.bad {
    color: var(--danger);
  }

  .line.faint {
    color: var(--text-faint);
  }

  .sep {
    padding: 0 4px;
    opacity: 0.6;
  }

  .num {
    font-variant-numeric: tabular-nums;
  }

  .controls {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: auto;
    min-width: 0;
  }

  .btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: none;
    height: 26px;
    padding: 0 12px;
    font: inherit;
    font-size: 12px;
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

  .btn:hover:not(:disabled) {
    border-color: var(--border-strong);
  }

  .btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  .btn:disabled {
    opacity: 0.55;
    cursor: default;
  }

  .btn.accent {
    color: var(--accent-text);
    background: var(--accent);
    border-color: var(--accent);
  }

  .btn.accent:hover:not(:disabled) {
    background: var(--accent-hover);
    border-color: var(--accent-hover);
  }

  .phase {
    font-size: 12px;
    white-space: nowrap;
    color: var(--text-dim);
  }

  .hint {
    margin-left: auto;
    font-size: 11px;
    white-space: nowrap;
    color: var(--text-faint);
  }

  .kbd {
    margin-left: auto;
    flex: none;
    padding: 0 6px;
    font-family: var(--font);
    font-size: 11px;
    line-height: 18px;
    white-space: nowrap;
    color: var(--text);
    background: var(--bg-inset);
    border: 1px solid var(--border);
    border-bottom-width: 2px;
    border-radius: 5px;
    cursor: default;
  }

  @media (prefers-reduced-motion: reduce) {
    .dot {
      animation: none;
    }
  }
</style>
