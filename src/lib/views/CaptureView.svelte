<script lang="ts">
  import { onMount } from "svelte";
  import Icon, { type IconName } from "$lib/components/Icon.svelte";
  import { toast } from "$lib/components/Toast.svelte";
  import { settings } from "$lib/stores/settings.svelte";
  import { history } from "$lib/stores/history.svelte";
  import {
    cancelScrollCapture,
    captureActiveWindow,
    captureFullscreen,
    captureMonitor,
    captureWindow,
    errorMessage,
    listMonitors,
    listWindows,
    onScrollProgress,
    openSaveDir,
    startRegionCapture,
    startScrollCapture,
    versionedAssetUrl,
    type CaptureRecord,
    type MonitorInfo,
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

  let hotkeys = $derived(settings.current?.hotkeys);
  let saveDir = $derived(settings.current?.saveDir ?? "");
  let recent = $derived(history.items.slice(0, 6));
  let primary = $derived(monitors.find((m) => m.isPrimary) ?? monitors[0]);

  onMount(() => {
    void refreshWindows();
    void refreshMonitors();
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
    } catch (err) {
      toast.error(label(err));
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

  /* One tile box, two kinds of tile: `.shot` is a button, `.scroller` is a
     panel with controls inside it. They share the box and nothing else — the
     hover, press and focus affordances below stay on `.shot`, because clicking
     the scrolling card itself does nothing. */
  .shot,
  .scroller {
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

  .scroller {
    grid-column: 1 / -1;
    flex-direction: row;
    align-items: center;
    gap: 16px;
  }

  .scroller .shot-text {
    flex: 1;
  }

  .scroll-controls {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: none;
    min-width: 0;
  }

  /* Window titles are long and arbitrary; the tile must not widen for them. */
  .scroll-controls .select {
    max-width: 260px;
  }

  .scroll-status {
    font-size: 13px;
    color: var(--text-dim);
    white-space: nowrap;
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
  .scroller.working .progress {
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
