<script lang="ts">
  import { onMount } from "svelte";
  import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
  import Icon, { type IconName } from "$lib/components/Icon.svelte";
  import { toast } from "$lib/components/Toast.svelte";
  import { settings } from "$lib/stores/settings.svelte";
  import { history } from "$lib/stores/history.svelte";
  import {
    cancelRecording,
    cancelScrollCapture,
    recordingStatus,
    captureActiveWindow,
    captureFullscreen,
    captureMonitor,
    captureWindow,
    errorMessage,
    FFMPEG_REMEDY_BROWSE,
    formatDuration,
    listMonitors,
    listWindows,
    onRecordCancelled,
    onRecordFinished,
    onRecordStatus,
    onScrollProgress,
    openSaveDir,
    RECORD_FORMAT_LABEL,
    RECORD_FORMATS,
    recordAvailability,
    recordFormatOf,
    startRecording,
    startRegionCapture,
    startScrollCapture,
    stopRecording,
    versionedAssetUrl,
    type CaptureRecord,
    type FfmpegAvailability,
    type MonitorInfo,
    type RecordFormat,
    type RecordOutcome,
    type RecordSource,
    type RecordSourceKind,
    type RecordStatus,
    type ScrollOutcome,
    type ScrollProgress,
    type View,
    type WindowInfo
  } from "$lib/api";

  let { onNavigate }: { onNavigate: (v: View) => void } = $props();

  let monitors = $state<MonitorInfo[]>([]);
  let windows = $state<WindowInfo[]>([]);
  let loadingWindows = $state(false);

  // Key of the capture currently in flight, e.g. "region" or "window:41230".
  // Captures are serialised because each one may hide and restore the main
  // window; two overlapping runs would fight over it.
  let pending = $state<string | null>(null);
  let busy = $derived(pending !== null);

  // Scrolling capture (M5 §4). It runs for tens of seconds, so it is the one
  // capture with live state of its own: the frame count Rust reports and a
  // cancel that has to reach Rust rather than just clearing a flag here.
  let scrollTarget = $state<number | null>(null);
  let scrolling = $state(false);
  let cancelling = $state(false);
  let progress = $state<ScrollProgress | null>(null);

  // Screen recording (M4). The live state is not local: Rust broadcasts
  // `record://status` to every window, so this card reflects a recording however
  // it was started — from here, from the tray, or from a hotkey — and its Stop
  // is the third of the three routes M4 §4 requires.
  let avail = $state<FfmpegAvailability | null>(null);
  let checkingFfmpeg = $state(false);
  let recordSource = $state<RecordSourceKind>("window");
  let recordFormat = $state<RecordFormat>("mp4");
  let recordWindow = $state<number | null>(null);
  let recordMonitor = $state<number | null>(null);
  let recordStatus = $state<RecordStatus | null>(null);
  let recordEnding = $state<"stop" | "cancel" | null>(null);
  let starting = $state(false);

  let hotkeys = $derived(settings.current?.hotkeys);
  let saveDir = $derived(settings.current?.saveDir ?? "");
  let recent = $derived(history.items.slice(0, 6));
  let primary = $derived(monitors.find((m) => m.isPrimary) ?? monitors[0]);
  let recording = $derived(recordStatus !== null && recordStatus.active);
  let stopRecordingKeys = $derived(shortcut(recordStatus?.stopHotkey));

  /** Whether the picker for the chosen source has something to point at. */
  let recordTargetReady = $derived(
    (recordSource === "window" && recordWindow !== null) ||
      (recordSource === "monitor" && recordMonitor !== null)
  );

  onMount(() => {
    void refreshWindows();
    void refreshMonitors();
    void refreshFfmpeg();
  });

  // Up for the life of the view, not for the life of a recording: a recording
  // begun by the stop hotkey's counterpart, the tray, or the HUD still has to
  // show up here, and a listener attached at Start would miss its own first
  // event.
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

    track(
      onRecordStatus((next) => {
        // A new recording clears an ending asked for on the last one.
        if (next.id !== recordStatus?.id) recordEnding = null;
        recordStatus = next.active ? next : null;
        if (!next.active) recordEnding = null;
      })
    );
    track(onRecordFinished(reportRecording));
    track(onRecordCancelled(reportRecording));

    // The window may have been opened part-way through a recording.
    void recordingStatusOnMount();

    return () => {
      cancelled = true;
      for (const unlisten of unlisteners) unlisten();
    };
  });

  // The picker opens on whatever Settings › Screen recording says rather than on
  // a hard-coded default that contradicts it. `adoptedFormat` is a plain `let`
  // and not `$state` on purpose: `settings.current` is replaced by every save, so
  // a bare effect would re-run on an unrelated setting change and undo the
  // choice the user just made in this picker.
  let adoptedFormat = "";
  $effect(() => {
    const stored = settings.current?.recordFormat;
    if (stored === undefined || stored === adoptedFormat) return;
    adoptedFormat = stored;
    recordFormat = recordFormatOf(stored);
  });

  // Subscribed for the life of the view, not just for the life of a run: the
  // listener has to be up before `start_scroll_capture` is invoked or the first
  // frames go unheard.
  $effect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    onScrollProgress((p) => (progress = p)).then((un) => {
      if (cancelled) un();
      else unlisten = un;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  });

  function label(err: unknown): string {
    return err instanceof Error ? err.message : String(err);
  }

  function describe(r: CaptureRecord): string {
    const dim = `${r.width}×${r.height}`;
    if (r.saved && r.copied) return `Saved ${r.name} · copied to clipboard`;
    if (r.saved) return `Saved ${r.name}`;
    if (r.copied) return `Copied ${dim} to clipboard`;
    return `Captured ${dim}`;
  }

  function shortcut(binding: string | undefined): string {
    if (!binding) return "";
    return binding
      .split("+")
      .map((part) => {
        const key = part.trim();
        if (key === "CmdOrCtrl" || key === "CommandOrControl") return "Ctrl";
        if (key === "PrintScreen") return "PrtScn";
        return key;
      })
      .join(" + ");
  }

  async function run(key: string, fn: () => Promise<CaptureRecord>): Promise<void> {
    if (pending) return;
    pending = key;
    try {
      toast.success(describe(await fn()));
    } catch (err) {
      toast.error(label(err));
    } finally {
      pending = null;
    }
  }

  async function region(): Promise<void> {
    if (pending) return;
    pending = "region";
    try {
      // Resolves once the overlay is up; the record arrives later via capture://new.
      await startRegionCapture();
    } catch (err) {
      toast.error(label(err));
    } finally {
      pending = null;
    }
  }

  async function refreshWindows(): Promise<void> {
    loadingWindows = true;
    try {
      windows = await listWindows();
      // A picked window that has since been closed would start a run against a
      // dead handle, so the picker re-homes onto the first live one. Never
      // mid-run: the list is only a display then, and the target is Rust's.
      if (!scrolling && !windows.some((w) => w.id === scrollTarget)) {
        scrollTarget = windows[0]?.id ?? null;
      }
      // Same re-homing for the recorder's picker, and never mid-recording: the
      // rect is fixed at start and Rust owns the target from then on (M4 §2).
      if (!recording && !windows.some((w) => w.id === recordWindow)) {
        recordWindow = windows[0]?.id ?? null;
      }
    } catch (err) {
      toast.error(label(err));
    } finally {
      loadingWindows = false;
    }
  }

  /**
   * Report what the run produced.
   *
   * `incomplete` decides the tone, not the reason code — Rust sets it whenever
   * the capture stopped short of the bottom *or* nothing scrolled at all, and a
   * truncated stitch looks exactly like a complete one. `message` is a finished
   * sentence that already names the frame count and the cause, so all this adds
   * is the part Rust has no reason to repeat: which file was kept.
   */
  function reportScroll(outcome: ScrollOutcome): void {
    if (outcome.incomplete) {
      toast.info(
        `${outcome.message} Kept ${outcome.record.name} — ${outcome.width}×${outcome.height}.`
      );
      return;
    }
    toast.success(`${outcome.message} ${describe(outcome.record)}.`);
  }

  async function startScroll(): Promise<void> {
    const target = scrollTarget;
    if (pending || target === null) return;
    pending = "scrolling";
    scrolling = true;
    cancelling = false;
    progress = null;
    try {
      reportScroll(await startScrollCapture(target));
    } catch (err) {
      toast.error(label(err));
    } finally {
      scrolling = false;
      cancelling = false;
      progress = null;
      pending = null;
    }
  }

  /**
   * Cancel has to reach Rust — clearing a flag here would leave the run driving
   * the wheel over someone else's window. Rust stops at its next poll, keeps
   * and finalizes what it already stitched, and resolves the original call, so
   * the minute is not thrown away.
   */
  async function cancelScroll(): Promise<void> {
    if (!scrolling || cancelling) return;
    cancelling = true;
    try {
      await cancelScrollCapture();
    } catch (err) {
      // The run is still going. Say so rather than flipping the button back to
      // a state that claims otherwise.
      cancelling = false;
      toast.error(errorMessage(err));
    }
  }

  // Nothing has been grabbed yet between the invoke and the first frame — the
  // target is still being raised to the foreground — so that gap is named
  // rather than shown as "Frame 0".
  const scrollStatus = $derived.by(() => {
    const p = progress;
    if (!p) return "Starting…";
    const of = p.maxFrames > 0 ? ` of ${p.maxFrames}` : "";
    const tall = p.height > 0 ? ` · ${p.height}px tall` : "";
    return `Frame ${p.frames}${of}${tall}`;
  });

  async function refreshMonitors(): Promise<void> {
    try {
      monitors = await listMonitors();
      if (!recording && !monitors.some((m) => m.id === recordMonitor)) {
        recordMonitor = (monitors.find((m) => m.isPrimary) ?? monitors[0])?.id ?? null;
      }
    } catch (err) {
      toast.error(label(err));
    }
  }

  /* ------------------------------------------------ screen recording (M4) */

  /** Adopt whatever is already recording when this view mounts. */
  async function recordingStatusOnMount(): Promise<void> {
    try {
      const current = await recordingStatus();
      if (current.active) recordStatus = current;
    } catch {
      // A view that cannot read the recorder still works for everything else.
    }
  }

  /**
   * Ask Rust where ffmpeg is (M4 §1). A *success* is cached for the session on
   * that side, keyed by the path setting, so this is cheap; a failure is not, so
   * calling again after the user runs the install command it suggested is all it
   * takes to notice.
   *
   * A failure to *ask* is not the same as ffmpeg being missing, so it leaves
   * `avail` null — the card then says it could not check rather than telling the
   * user to install something that may well already be there.
   */
  async function refreshFfmpeg(): Promise<void> {
    checkingFfmpeg = true;
    try {
      avail = await recordAvailability();
    } catch {
      avail = null;
    } finally {
      checkingFfmpeg = false;
    }
  }

  /** The remedies that are commands to run, as opposed to the Browse control. */
  const ffmpegCommands = $derived(
    (avail?.remedies ?? []).filter((r) => r.id !== FFMPEG_REMEDY_BROWSE && r.command !== "")
  );

  /**
   * Point at an ffmpeg binary directly (M4 §1). The last rung of the resolution
   * order, and the reason a machine with no package manager is not simply out of
   * luck — santi.sharex finds ffmpeg and never downloads one.
   */
  async function browseFfmpeg(): Promise<void> {
    try {
      const picked = await openFileDialog({
        multiple: false,
        directory: false,
        defaultPath: avail?.path || undefined,
        filters: [{ name: "ffmpeg", extensions: ["exe"] }]
      });
      if (typeof picked !== "string" || picked === "") return;
      const ok = await settings.update({ ffmpegPath: picked });
      if (!ok) {
        toast.error(settings.error ?? "Could not save the ffmpeg path");
        return;
      }
      // The stored path is the first step of the resolution order, so the cached
      // probe is stale the moment it changes.
      await refreshFfmpeg();
      if (avail?.available) toast.success(`Using ffmpeg at ${avail.path}`);
      else if (avail) toast.error(ffmpegProblem(avail));
    } catch (err) {
      toast.error(errorMessage(err));
    }
  }

  /** The structured "unavailable" state as one sentence, for a toast. */
  function ffmpegProblem(status: FfmpegAvailability): string {
    const what = status.missing.map((m) => m.label).join(", ");
    return what ? `Recording needs ${what}.` : "ffmpeg was not found.";
  }

  function recordSourceSpec(): RecordSource | null {
    if (recordSource === "window") {
      return recordWindow === null ? null : { type: "window", id: recordWindow };
    }
    if (recordSource === "monitor") {
      return recordMonitor === null ? null : { type: "monitor", id: recordMonitor };
    }
    // `region` carries an absolute rect, which only the region overlay can
    // produce — see the disabled option in the picker.
    return null;
  }

  /**
   * Say what a recording ended as.
   *
   * A cancel kept nothing, so it is reported as the plain fact it is rather than
   * as a saved file that does not exist. `truncated` is the one that must not be
   * swallowed: the record looks like any other, and only this says the clip may
   * be missing its last moments.
   */
  function reportRecording(outcome: RecordOutcome): void {
    if (outcome.cancelled || !outcome.record) {
      toast.info("Recording discarded — nothing was written.");
      return;
    }
    const length = formatDuration(outcome.durationMs);
    const lost =
      outcome.dropped > 0
        ? ` ${outcome.dropped} ${outcome.dropped === 1 ? "frame" : "frames"} dropped.`
        : "";
    const line = `Recorded ${length} — ${describe(outcome.record)}.${lost}`;
    if (outcome.truncated) toast.info(line);
    else toast.success(line);
  }

  /**
   * Start recording. Resolves as soon as frames are flowing; everything after
   * that arrives on `record://status` and, at the end, on `record://finished`
   * or `record://cancelled` — which is why the listeners above are up for the
   * life of the view rather than being attached here.
   */
  async function startRecord(): Promise<void> {
    const source = recordSourceSpec();
    if (starting || recording || !source || avail?.available !== true) return;
    starting = true;
    try {
      recordStatus = await startRecording({ source, format: recordFormat });
    } catch (err) {
      toast.error(label(err));
    } finally {
      starting = false;
    }
  }

  /**
   * Stop has to reach Rust: clearing a flag here would leave ffmpeg running and
   * the HUD on screen. This is the third of the three routes M4 §4 requires —
   * beside the HUD button and the global hotkey — and each has to work when the
   * other two do not.
   */
  async function stopRecord(): Promise<void> {
    if (!recording || recordEnding) return;
    recordEnding = "stop";
    try {
      await stopRecording();
    } catch (err) {
      // Rust refuses when nothing is recording, which is the ordinary race with
      // a recording that has just ended. Anything else means it is still going,
      // so the buttons go back rather than claiming otherwise.
      recordEnding = null;
      toast.error(errorMessage(err));
    }
  }

  async function cancelRecord(): Promise<void> {
    if (!recording || recordEnding) return;
    recordEnding = "cancel";
    try {
      await cancelRecording();
    } catch (err) {
      recordEnding = null;
      toast.error(errorMessage(err));
    }
  }

  async function revealSaveDir(): Promise<void> {
    try {
      await openSaveDir();
    } catch (err) {
      toast.error(label(err));
    }
  }

  const cards: { key: string; icon: IconName; title: string; desc: string }[] = [
    {
      key: "fullscreen",
      icon: "fullscreen",
      title: "Fullscreen",
      desc: "Every display, composited into one image."
    },
    {
      key: "active",
      icon: "window",
      title: "Active window",
      desc: "Grab whatever window has focus."
    },
    {
      key: "monitor",
      icon: "monitor",
      title: "Monitor",
      desc: "Just the primary display."
    }
  ];

  function runCard(key: string): void {
    if (key === "fullscreen") {
      void run(key, captureFullscreen);
    } else if (key === "active") {
      void run(key, captureActiveWindow);
    } else if (key === "monitor") {
      const target = primary;
      if (target) void run(key, () => captureMonitor(target.id));
    }
  }

  function cardChip(key: string): string {
    if (key === "fullscreen") return shortcut(hotkeys?.fullscreen);
    if (key === "active") return shortcut(hotkeys?.activeWindow);
    return "";
  }
</script>

<header class="head">
  <div class="head-title">
    <h1>Capture</h1>
    {#if saveDir}<span class="head-sub" title={saveDir}>{saveDir}</span>{/if}
  </div>
  <div class="head-actions">
    <button type="button" class="btn" onclick={revealSaveDir}>
      <Icon name="folder" size={16} />
      Save folder
    </button>
  </div>
</header>

<div class="body">
  <section class="grid" aria-label="Capture actions">
    <button
      type="button"
      class="shot hero"
      class:working={pending === "region"}
      disabled={busy}
      onclick={region}
    >
      <span class="shot-icon"><Icon name="region" size={26} /></span>
      <span class="shot-text">
        <span class="shot-title">Region</span>
        <span class="shot-desc">Freeze the screen and drag out any rectangle.</span>
      </span>
      {#if shortcut(hotkeys?.region)}
        <kbd class="kbd">{shortcut(hotkeys?.region)}</kbd>
      {/if}
      <span class="progress" aria-hidden="true"></span>
    </button>

    {#each cards as card (card.key)}
      <button
        type="button"
        class="shot"
        class:working={pending === card.key}
        disabled={busy || (card.key === "monitor" && !primary)}
        onclick={() => runCard(card.key)}
      >
        <span class="shot-icon"><Icon name={card.icon} size={20} /></span>
        <span class="shot-text">
          <span class="shot-title">{card.title}</span>
          <span class="shot-desc">{card.desc}</span>
        </span>
        {#if cardChip(card.key)}
          <kbd class="kbd">{cardChip(card.key)}</kbd>
        {:else if card.key === "monitor" && primary}
          <span class="chip num">{primary?.width} &times; {primary?.height}</span>
        {/if}
        <span class="progress" aria-hidden="true"></span>
      </button>
    {/each}

    <!-- Not a button: it holds a picker, and mid-run it holds the Cancel. A run
         is tens of seconds long, so the frame count and the cancel are the
         card, not decoration on it. -->
    <div class="scroller" class:working={scrolling}>
      <span class="shot-icon"><Icon name="scroll" size={20} /></span>
      <span class="shot-text">
        <span class="shot-title">Scrolling capture</span>
        <span class="shot-desc">
          {#if scrolling}
            Leave the mouse alone. The run drives the real pointer, so moving it ends the capture —
            including reaching for Cancel, which is why Cancel is reachable with Tab and Enter.
            Either way the frames stitched so far are kept.
          {:else}
            Scroll a window a step at a time and stitch the frames into one tall image. It takes
            over the mouse pointer while it runs. Good on ordinary scrolling content; poor on
            virtualised lists, parallax, and headers that repeat in every frame.
          {/if}
        </span>
      </span>

      <div class="scroll-controls">
        {#if scrolling}
          <span class="scroll-status num" role="status" aria-live="polite">{scrollStatus}</span>
          <button
            type="button"
            class="btn"
            disabled={cancelling}
            title="Stop here and keep what has been stitched"
            onclick={cancelScroll}
          >
            {cancelling ? "Stopping…" : "Cancel"}
          </button>
        {:else}
          <select
            class="select"
            aria-label="Window to scroll and capture"
            disabled={busy || windows.length === 0}
            bind:value={scrollTarget}
          >
            {#if windows.length === 0}
              <option value={null}>No capturable windows</option>
            {:else}
              {#each windows as w (w.id)}
                <option value={w.id}>{w.title || "Untitled window"} — {w.appName}</option>
              {/each}
            {/if}
          </select>
          <button
            type="button"
            class="btn"
            disabled={busy || scrollTarget === null}
            onclick={startScroll}
          >
            Start
          </button>
        {/if}
      </div>

      <span class="progress" aria-hidden="true"></span>
    </div>

    <!-- Screen recorder (M4). Not a button for the same reason the scrolling
         card is not: it holds pickers, and mid-run it holds the Stop. -->
    <div class="recorder" class:working={recording}>
      <span class="shot-icon"><Icon name="video" size={20} /></span>
      <span class="shot-text">
        <span class="shot-title">Screen recorder</span>
        <span class="shot-desc">
          {#if recording}
            Recording. Stop from the HUD in the corner of the screen, from the tray, or{#if stopRecordingKeys}
              with <kbd class="kbd">{stopRecordingKeys}</kbd>{:else}
              from here — no stop hotkey is bound{/if}. Cancel throws the file away.
          {:else if avail?.available}
            Record a window or a display to MP4 or GIF. The rect is fixed when recording starts —
            a window that moves or resizes keeps the rect it had.
          {:else}
            Record a window or a display to MP4 or GIF. It needs ffmpeg, which santi.sharex finds
            on this machine rather than downloading.
          {/if}
        </span>
      </span>

      {#if recording}
        <div class="rec-controls">
          <span class="scroll-status num" role="status" aria-live="polite">
            {recordEnding === "cancel"
              ? "Discarding…"
              : recordEnding === "stop"
                ? "Finishing…"
                : `Recording ${formatDuration(recordStatus?.elapsedMs ?? 0)}`}
          </span>
          <button
            type="button"
            class="btn"
            disabled={recordEnding !== null}
            title="Finish the recording and keep the file"
            onclick={stopRecord}
          >
            Stop
          </button>
          <button
            type="button"
            class="btn"
            disabled={recordEnding !== null}
            title="Throw the recording away"
            onclick={cancelRecord}
          >
            Cancel
          </button>
        </div>
      {:else if avail?.available}
        <div class="rec-controls">
          <select
            class="select"
            aria-label="What to record"
            disabled={busy || starting}
            bind:value={recordSource}
          >
            <!-- Disabled rather than absent, and with the reason on it: a
                 recorded region needs an absolute rect, and the only thing that
                 can draw one is the region overlay's record mode, which is not
                 wired up. An option that opened the overlay and then did nothing
                 would be worse than one that says so. -->
            <option value="region" disabled>
              Region — needs the overlay's record mode
            </option>
            <option value="window">Window</option>
            <option value="monitor">Display</option>
          </select>

          <!-- The same pickers the rest of this screen already uses, rather than
               a second list that could disagree with them. -->
          {#if recordSource === "monitor"}
            <select
              class="select"
              aria-label="Display to record"
              disabled={busy || starting || monitors.length === 0}
              bind:value={recordMonitor}
            >
              {#if monitors.length === 0}
                <option value={null}>No displays reported</option>
              {:else}
                {#each monitors as m (m.id)}
                  <option value={m.id}>{m.name} — {m.width} × {m.height}</option>
                {/each}
              {/if}
            </select>
          {:else}
            <select
              class="select"
              aria-label="Window to record"
              disabled={busy || starting || windows.length === 0}
              bind:value={recordWindow}
            >
              {#if windows.length === 0}
                <option value={null}>No capturable windows</option>
              {:else}
                {#each windows as w (w.id)}
                  <option value={w.id}>{w.title || "Untitled window"} — {w.appName}</option>
                {/each}
              {/if}
            </select>
          {/if}

          <select
            class="select narrow"
            aria-label="Recording format"
            disabled={busy || starting}
            bind:value={recordFormat}
          >
            {#each RECORD_FORMATS as f (f)}
              <option value={f}>{RECORD_FORMAT_LABEL[f]}</option>
            {/each}
          </select>

          <button
            type="button"
            class="btn"
            disabled={busy || starting || !recordTargetReady}
            onclick={startRecord}
          >
            {starting ? "Starting…" : "Start"}
          </button>
        </div>
      {:else}
        <!-- The structured reason, not a generic error (M4 §1): what is missing,
             the exact commands that fix it, and a way to point at a binary.
             Never an offer to download one. -->
        <div class="rec-missing">
          {#if checkingFfmpeg && !avail}
            <p class="rec-problem">Looking for ffmpeg…</p>
          {:else if !avail}
            <p class="rec-problem">Could not check for ffmpeg.</p>
            <p class="rec-note">
              Nothing has been ruled out — the check itself failed, not ffmpeg. Try again.
            </p>
          {:else}
            <p class="rec-problem">
              Recording needs
              {#each avail.missing as miss, i (miss.id)}
                {#if i > 0}{i === avail.missing.length - 1 ? " and " : ", "}{/if}{miss.label}
              {:else}
                ffmpeg
              {/each}.
            </p>
            {#if ffmpegCommands.length > 0}
              <p class="rec-note">Any one of these installs it:</p>
              <p class="rec-cmds">
                {#each ffmpegCommands as remedy (remedy.id)}
                  <code title={remedy.label}>{remedy.command}</code>
                {/each}
              </p>
            {/if}
            <p class="rec-note">
              Already have one somewhere else? Point at the binary and santi.sharex will use it —
              it never downloads one.
            </p>
            {#if avail.searched.length > 0}
              <details class="rec-searched">
                <summary>Where santi.sharex looked</summary>
                <ul>
                  {#each avail.searched as attempt, i (i)}
                    <li>
                      <span class="mono">{attempt.path || attempt.source}</span>
                      <span class="rec-outcome">
                        {attempt.outcome === "notFound"
                          ? "not there"
                          : (attempt.detail ?? attempt.outcome)}
                      </span>
                    </li>
                  {/each}
                </ul>
              </details>
            {/if}
          {/if}
          <div class="rec-actions">
            <button type="button" class="btn" onclick={browseFfmpeg}>Browse…</button>
            <button
              type="button"
              class="btn ghost"
              disabled={checkingFfmpeg}
              onclick={() => refreshFfmpeg()}
            >
              <Icon name="refresh" size={16} />
              {checkingFfmpeg ? "Checking…" : "Check again"}
            </button>
          </div>
        </div>
      {/if}

      <span class="progress" aria-hidden="true"></span>
    </div>
  </section>

  <section class="block">
    <div class="block-head">
      <h2>Capture a specific window</h2>
      <button type="button" class="btn ghost" onclick={refreshWindows} disabled={loadingWindows}>
        <Icon name="refresh" size={16} />
        Refresh
      </button>
    </div>

    <div class="panel list scroll">
      {#if windows.length === 0}
        <p class="empty">
          {loadingWindows ? "Looking for open windows…" : "No capturable windows found."}
        </p>
      {:else}
        {#each windows as w (w.id)}
          <div class="item">
            <span class="item-icon"><Icon name="window" size={16} /></span>
            <span class="item-meta">
              <span class="item-title">{w.title || "Untitled window"}</span>
              <span class="item-sub">
                {w.appName}<span class="dot">&middot;</span><span class="num"
                  >{w.width} &times; {w.height}</span
                >
              </span>
            </span>
            <button
              type="button"
              class="btn"
              disabled={busy}
              onclick={() => run(`window:${w.id}`, () => captureWindow(w.id))}
            >
              {pending === `window:${w.id}` ? "Capturing…" : "Capture"}
            </button>
          </div>
        {/each}
      {/if}
    </div>
  </section>

  <section class="block">
    <div class="block-head">
      <h2>Displays</h2>
      <button type="button" class="btn ghost" onclick={refreshMonitors}>
        <Icon name="refresh" size={16} />
        Refresh
      </button>
    </div>

    {#if monitors.length === 0}
      <p class="panel empty">No displays reported.</p>
    {:else}
      <div class="monitors">
        {#each monitors as m (m.id)}
          <div class="mon card">
            <div class="mon-head">
              <Icon name="monitor" size={16} />
              <span class="mon-name">{m.name}</span>
              {#if m.isPrimary}<span class="chip">Primary</span>{/if}
            </div>
            <div class="mon-res num">{m.width} &times; {m.height}</div>
            <button
              type="button"
              class="btn"
              disabled={busy}
              onclick={() => run(`monitor:${m.id}`, () => captureMonitor(m.id))}
            >
              {pending === `monitor:${m.id}` ? "Capturing…" : "Capture"}
            </button>
          </div>
        {/each}
      </div>
    {/if}
  </section>

  <section class="block">
    <div class="block-head">
      <h2>Recent</h2>
      <button type="button" class="btn ghost" onclick={() => onNavigate("history")}>
        View all
        <Icon name="chevron-right" size={16} />
      </button>
    </div>

    {#if recent.length === 0}
      <p class="panel empty">Captures you take will show up here.</p>
    {:else}
      <div class="recent">
        {#each recent as r (r.id)}
          <button
            type="button"
            class="thumb"
            title={r.name}
            onclick={() => onNavigate("history")}
          >
            <img
              src={versionedAssetUrl(r.thumb, r.sizeBytes)}
              alt={r.name}
              draggable="false"
            />
            <span class="thumb-meta num">{r.width} &times; {r.height}</span>
          </button>
        {/each}
      </div>
    {/if}
  </section>
</div>

<style>
  .num {
    font-variant-numeric: tabular-nums;
  }

  /* Sticky header spans the full pane; the 32px inset lives on the header and
     the body separately so content scrolls under the border, not under a gap. */
  .head {
    position: sticky;
    top: 0;
    z-index: 10;
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    padding: 24px 32px 16px;
    border-bottom: 1px solid var(--border);
    background: var(--bg);
  }

  .head-title {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }

  h1 {
    margin: 0;
    font-family: var(--font-display);
    font-size: 20px;
    font-weight: 600;
    letter-spacing: -0.01em;
    color: var(--text);
  }

  .head-sub {
    max-width: 46ch;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 12px;
    color: var(--text-faint);
  }

  .head-actions {
    display: flex;
    gap: 8px;
  }

  .body {
    display: flex;
    flex-direction: column;
    gap: 32px;
    padding: 24px 32px 40px;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 12px;
  }

  /* One tile box, two kinds of tile: `.shot` is a button, `.scroller` and
     `.recorder` are panels with controls inside them. They share the box and
     nothing else — the hover, press and focus affordances below stay on `.shot`,
     because clicking either panel itself does nothing. */
  .shot,
  .scroller,
  .recorder {
    position: relative;
    overflow: hidden;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 10px;
    padding: 16px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-raised);
    font: inherit;
    color: var(--text);
    text-align: left;
    transition:
      background 140ms ease,
      border-color 140ms ease,
      transform 140ms ease;
  }

  .shot {
    cursor: pointer;
  }

  .shot:hover:not(:disabled) {
    background: var(--surface);
    border-color: var(--border-strong);
  }

  .shot:active:not(:disabled) {
    transform: translateY(1px);
  }

  .shot:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .shot:disabled {
    cursor: default;
    opacity: 0.55;
  }

  .shot.working,
  .scroller.working {
    opacity: 1;
    border-color: color-mix(in srgb, var(--accent) 55%, var(--border));
  }

  /* A live recording is the loudest state on this screen, and the only one that
     is still true after the user has looked away, so it is marked in the danger
     colour rather than in the accent every other in-flight job uses. */
  .recorder.working {
    opacity: 1;
    border-color: color-mix(in srgb, var(--danger) 55%, var(--border));
  }

  .recorder.working .progress {
    background: var(--danger);
  }

  .scroller,
  .recorder {
    grid-column: 1 / -1;
    flex-direction: row;
    align-items: center;
    gap: 16px;
  }

  /* The ffmpeg-missing block is a paragraph, not a control strip, so it wraps
     onto its own line under the icon and the description. */
  .recorder {
    flex-wrap: wrap;
  }

  .scroller .shot-text,
  .recorder .shot-text {
    flex: 1;
  }

  .scroll-controls,
  .rec-controls {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: none;
    min-width: 0;
  }

  /* Window titles are long and arbitrary; the tile must not widen for them. */
  .scroll-controls .select,
  .rec-controls .select {
    max-width: 260px;
  }

  .rec-controls .select.narrow {
    max-width: 96px;
  }

  .scroll-status {
    font-size: 13px;
    color: var(--text-dim);
    white-space: nowrap;
  }

  /* Full width so it starts under the description rather than squeezing it. */
  .rec-missing {
    flex: 1 0 100%;
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 12px 14px;
    border: 1px solid color-mix(in srgb, var(--danger) 35%, transparent);
    border-radius: var(--radius);
    background: color-mix(in srgb, var(--danger) 7%, var(--bg-inset));
  }

  .rec-problem {
    margin: 0;
    font-size: 13px;
    color: var(--text);
  }

  .rec-note {
    margin: 0;
    max-width: 68ch;
    font-size: 12px;
    line-height: 1.5;
    color: var(--text-dim);
  }

  .rec-cmds {
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .rec-cmds code {
    padding: 2px 8px;
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--text);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 5px;
    user-select: all;
  }

  .rec-actions {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 4px;
  }

  /* Folded away by default: it is evidence for the one user whose ffmpeg lives
     somewhere unusual, not part of the message everyone else has to read. */
  .rec-searched summary {
    font-size: 12px;
    color: var(--text-dim);
    cursor: pointer;
  }

  .rec-searched ul {
    margin: 6px 0 0;
    padding-left: 18px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 12px;
    color: var(--text-faint);
  }

  .rec-searched .mono {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-dim);
    word-break: break-all;
  }

  .rec-outcome {
    padding-left: 6px;
  }

  /* Inside a sentence rather than as a binding on a tile, so it is quieter than
     the `.kbd` the capture cards wear. */
  .shot-desc .kbd {
    margin: 0 2px;
    padding: 0 5px;
    font-size: 11px;
    line-height: 16px;
  }

  .hero {
    grid-column: 1 / -1;
    flex-direction: row;
    align-items: center;
    gap: 16px;
    padding: 20px;
    border-color: color-mix(in srgb, var(--accent) 32%, var(--border));
    background: linear-gradient(
      to right,
      color-mix(in srgb, var(--accent) 10%, var(--bg-raised)),
      var(--bg-raised) 70%
    );
  }

  .hero:hover:not(:disabled) {
    border-color: color-mix(in srgb, var(--accent) 55%, var(--border));
    background: linear-gradient(
      to right,
      color-mix(in srgb, var(--accent) 16%, var(--bg-raised)),
      var(--bg-raised) 70%
    );
  }

  .shot-icon {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 34px;
    height: 34px;
    border-radius: var(--radius);
    background: var(--bg-inset);
    color: var(--text-dim);
  }

  .hero .shot-icon {
    width: 48px;
    height: 48px;
    border-radius: 12px;
    background: var(--accent);
    color: var(--accent-text);
  }

  .shot-text {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }

  .hero .shot-text {
    flex: 1;
  }

  .shot-title {
    font-size: 13px;
    font-weight: 600;
  }

  .hero .shot-title {
    font-family: var(--font-display);
    font-size: 17px;
  }

  .shot-desc {
    font-size: 12px;
    line-height: 1.45;
    color: var(--text-dim);
  }

  .shot .kbd,
  .shot .chip {
    margin-top: auto;
  }

  .hero .kbd {
    margin-top: 0;
  }

  .progress {
    position: absolute;
    left: 0;
    bottom: 0;
    height: 2px;
    width: 100%;
    transform: scaleX(0);
    transform-origin: left;
    background: var(--accent);
  }

  .shot.working .progress,
  .scroller.working .progress,
  .recorder.working .progress {
    animation: sweep 900ms ease-in-out infinite;
  }

  @keyframes sweep {
    0% {
      transform: translateX(-100%) scaleX(0.4);
    }
    100% {
      transform: translateX(100%) scaleX(0.4);
    }
  }

  .block {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .block-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }

  h2 {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
    color: var(--text);
  }

  .list {
    max-height: 268px;
    overflow-y: auto;
    padding: 4px;
  }

  .item {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 46px;
    padding: 0 8px;
    border-radius: var(--radius);
    transition: background 120ms ease;
  }

  .item + .item {
    box-shadow: inset 0 1px 0 var(--border);
  }

  .item:hover {
    background: var(--bg-inset);
  }

  .item-icon {
    display: flex;
    color: var(--text-faint);
  }

  .item-meta {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
    flex: 1;
  }

  .item-title {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 13px;
    color: var(--text);
  }

  .item-sub {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 12px;
    color: var(--text-faint);
  }

  .dot {
    padding: 0 5px;
  }

  .monitors {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: 10px;
  }

  .mon {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 8px;
    padding: 14px;
  }

  .mon-head {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    color: var(--text-dim);
  }

  .mon-name {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 13px;
    font-weight: 500;
    color: var(--text);
  }

  .mon-res {
    font-size: 12px;
    color: var(--text-faint);
  }

  .recent {
    display: grid;
    grid-template-columns: repeat(6, minmax(0, 1fr));
    gap: 10px;
  }

  .thumb {
    position: relative;
    overflow: hidden;
    display: block;
    aspect-ratio: 16 / 10;
    padding: 0;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-inset);
    cursor: pointer;
    transition:
      border-color 140ms ease,
      transform 140ms ease;
  }

  .thumb:hover {
    border-color: var(--border-strong);
    transform: translateY(-1px);
  }

  .thumb:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }

  .thumb-meta {
    position: absolute;
    left: 6px;
    bottom: 6px;
    padding: 2px 6px;
    border-radius: 5px;
    background: color-mix(in srgb, var(--bg) 78%, transparent);
    font-size: 11px;
    color: var(--text-dim);
  }

  .btn {
    display: inline-flex;
    align-items: center;
    gap: 7px;
  }

  .btn.ghost {
    border-color: transparent;
    background: none;
    color: var(--text-dim);
  }

  .btn.ghost:hover:not(:disabled) {
    background: var(--bg-inset);
    color: var(--text);
  }

  @media (max-width: 1000px) {
    .recent {
      grid-template-columns: repeat(3, minmax(0, 1fr));
    }
  }
</style>
