<script lang="ts">
  /**
   * The `editor` window shell (ARCHITECTURE-M2 §1, §4). It owns everything the
   * canvas is not: loading the capture, tool + option state, the keyboard map,
   * the save/copy round trips and the close prompt.
   *
   * What it expects from the modules it sits on top of:
   *   `$lib/editor/tools`      ToolId, ToolOptions, RedactMode
   *   `$lib/editor/doc.svelte` the `doc` singleton — a rune store whose public
   *                            fields (`src`, `width`, `height`, `crop`,
   *                            `shapes`, `selected`) are exactly `EditorDoc`,
   *                            so it can be handed straight to `renderToPng`
   *   `$lib/editor/export`     renderToPng(doc) -> base64 PNG, no data: prefix
   *   `stage/Stage.svelte`     reads `doc` itself; takes the active tool and
   *                            options, and reports display scale + drag state
   *                            back through bindable props
   */
  import { onMount } from "svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { ask } from "@tauri-apps/plugin-dialog";
  import {
    closeEditor,
    copyEdit,
    errorMessage,
    getCapture,
    getCaptureImage,
    onEditorLoad,
    saveEdit,
    versionedAssetUrl,
    type CaptureRecord,
    type UnlistenFn
  } from "$lib/api";
  import { settings } from "$lib/stores/settings.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import Toast, { toast } from "$lib/components/Toast.svelte";
  import Toolbar, { TOOLS } from "$lib/editor/Toolbar.svelte";
  import Stage from "$lib/editor/stage/Stage.svelte";
  import { doc } from "$lib/editor/doc.svelte";
  import type { ShapeKind, ShapePatch } from "$lib/editor/doc.svelte";
  import { renderToPng } from "$lib/editor/export";
  import { toolHitForKey } from "$lib/editor/tools";
  import type { ToolId, ToolOptions } from "$lib/editor/tools";

  type Phase = "loading" | "ready" | "error";
  type Fault = { title: string; text: string; path?: string };
  type Job = "copy" | "save" | "saveAs";

  /** Fallbacks for the very first paint, before settings have landed. */
  const DEFAULT_OPTIONS: ToolOptions = {
    color: "#f2555a",
    stroke: 4,
    fill: false,
    textSize: 28,
    redactMode: "blur",
    redactAmount: 12
  };

  const ZOOM_STEPS = [0.1, 0.15, 0.25, 0.33, 0.5, 0.67, 0.75, 1, 1.5, 2, 3, 4, 6, 8];


  let phase = $state<Phase>("loading");
  let fault = $state<Fault | null>(null);
  let record = $state<CaptureRecord | null>(null);

  let tool = $state<ToolId>("select");
  let opts = $state<ToolOptions>({ ...DEFAULT_OPTIONS });

  let zoom = $state(1);
  let fit = $state(true);
  /** True while the stage owns the interaction: a live drag or an open caret. */
  let stageBusy = $state(false);

  let job = $state<Job | null>(null);
  // Not $state: it gates the close handler rather than the UI, and flipping it
  // must not schedule a re-render in the middle of a teardown.
  let closing = false;

  const selectedKind = $derived(
    doc.shapes.find((s) => s.id === doc.selected)?.kind ?? null
  );

  const outSize = $derived(
    doc.crop
      ? { width: doc.crop.width, height: doc.crop.height }
      : { width: doc.width, height: doc.height }
  );

  const canAct = $derived(phase === "ready" && record !== null);

  /* --------------------------------------------------------------- loading */

  /**
   * Decoded through fetch + `createImageBitmap`, NOT `new Image()`.
   *
   * The asset protocol serves from a foreign origin (`http://asset.localhost`),
   * so an `<img>` pointed at it taints every canvas it is drawn into and the
   * export dies with "Tainted canvases may not be exported" — at save time,
   * long after the damage was done. A bitmap decoded from a Blob carries no
   * origin, so the export canvas stays clean.
   */
  async function loadImage(url: string): Promise<ImageBitmap> {
    const res = await fetch(url);
    if (!res.ok) throw new Error(`The image could not be read (${res.status}).`);
    return createImageBitmap(await res.blob());
  }

  async function loadCapture(id: string): Promise<void> {
    phase = "loading";
    fault = null;
    try {
      const next = await getCapture(id);
      record = next;

      // A record with an empty path was taken with "Save to disk" off: there is
      // no file to edit, and rendering an empty canvas would be a lie.
      if (!next.path) {
        fault = {
          title: "Nothing on disk to edit",
          text:
            "This capture was taken with Save to disk turned off, so only its thumbnail was kept. Turn the setting on in Settings › Capture to keep future files."
        };
        phase = "error";
        return;
      }

      const path = await getCaptureImage(next.id);
      // Versioned: re-editing a capture that was already saved over must not
      // pick the pre-edit bytes out of the webview cache.
      const image = await loadImage(versionedAssetUrl(path, next.sizeBytes));
      doc.load(image, image.width, image.height);
      // A fresh document always opens fitted, whatever the previous one used.
      fit = true;
      tool = "select";
      phase = "ready";
    } catch (e) {
      fault = {
        title: "Could not open this capture",
        text: errorMessage(e),
        path: record?.path || undefined
      };
      phase = "error";
    }
  }

  /* ------------------------------------------------------------- lifecycle */

  function confirmDiscard(): Promise<boolean> {
    return ask("Discard the unsaved changes to this capture?", {
      title: "Unsaved changes",
      kind: "warning",
      okLabel: "Discard",
      cancelLabel: "Keep editing"
    });
  }

  async function shutWindow(): Promise<void> {
    try {
      await closeEditor();
    } catch {
      // The command is the polite path; if it never lands, close the window
      // ourselves rather than stranding the user in an editor they cannot quit.
      try {
        await getCurrentWindow().close();
      } catch {
        /* nothing left to try */
      }
    }
  }

  async function requestClose(): Promise<void> {
    if (closing) return;
    if (doc.dirty && !(await confirmDiscard())) return;
    closing = true;
    await shutWindow();
  }

  onMount(() => {
    void settings.load();

    const id = new URLSearchParams(window.location.search).get("id");
    if (!id) {
      fault = {
        title: "No capture to edit",
        text: "The editor window was opened without a capture id."
      };
      phase = "error";
    } else {
      void loadCapture(id);
    }

    const stops: UnlistenFn[] = [];

    // A second `open_editor` focuses this window instead of spawning another,
    // so the swap has to happen in here — and must not silently bin edits.
    void onEditorLoad((nextId) => {
      void (async () => {
        if (nextId === record?.id && phase === "ready") return;
        if (doc.dirty && !(await confirmDiscard())) return;
        await loadCapture(nextId);
      })();
    }).then((stop) => stops.push(stop));

    // preventDefault has to run synchronously, before the prompt is awaited,
    // or Tauri has already committed to closing by the time we answer.
    void getCurrentWindow()
      .onCloseRequested((event) => {
        if (closing || !doc.dirty) return;
        event.preventDefault();
        void (async () => {
          if (!(await confirmDiscard())) return;
          closing = true;
          await shutWindow();
        })();
      })
      .then((stop) => stops.push(stop));

    return () => {
      for (const stop of stops) stop();
    };
  });

  // Seed the tool defaults from settings once. Later `settings://changed`
  // events must not stomp on a colour the user has since picked in the toolbar.
  let seeded = false;
  $effect(() => {
    const current = settings.current;
    if (!current || seeded) return;
    seeded = true;
    opts.color = current.editorDefaultColor;
    opts.stroke = current.editorDefaultStroke;
  });

  $effect(() => {
    const name = record?.name ?? "capture";
    const title = `${doc.dirty ? "• " : ""}Edit — ${name}`;
    void getCurrentWindow().setTitle(title);
  });

  /* ------------------------------------------------------------- mutations */

  /** Drops undefined keys so a store patch never writes a field back as blank. */
  function prune<T extends object>(patch: T): Partial<T> {
    return Object.fromEntries(
      Object.entries(patch).filter(([, v]) => v !== undefined)
    ) as Partial<T>;
  }

  /**
   * True between a slider's pointerdown and its release. The gesture itself is
   * not opened on the document yet — only the first restyle it actually
   * produces opens one, so pressing a slider without moving it neither dirties
   * the document nor leaves an empty undo entry behind.
   */
  let optionDrag = false;

  /**
   * A slider fires `oninput` on every pixel of a drag, and with a shape
   * selected every one of those is a document mutation — one sweep of the width
   * control would push ~20 undo entries and, over a session, flush the 100-entry
   * stack. Fold the whole sweep into one entry, exactly as the stage folds a
   * pointer drag (M2 §4.2).
   */
  function optionGesture(active: boolean): void {
    if (active) {
      optionDrag = true;
      return;
    }
    optionDrag = false;
    doc.endDrag();
  }

  /**
   * An option change sets the default for the next shape *and* restyles the
   * selected one — but only with the fields that shape actually owns, so a
   * `fill` toggle never lands a dead property on an arrow.
   */
  /**
   * The fields of `patch` the selected shape actually owns. An erase owns none
   * of them — its fill comes from the image (M2.11 §2) — and every other kind
   * drops whatever the renderer would ignore.
   */
  function shapeFields(kind: ShapeKind, patch: Partial<ToolOptions>): ShapePatch {
    switch (kind) {
      case "rect":
      case "ellipse":
        return prune({ color: patch.color, stroke: patch.stroke, fill: patch.fill });
      case "text":
        return prune({ color: patch.color, size: patch.textSize });
      case "redact":
        return prune({ mode: patch.redactMode, amount: patch.redactAmount });
      case "erase":
        return {};
      default:
        return prune({ color: patch.color, stroke: patch.stroke });
    }
  }

  function applyOptions(patch: Partial<ToolOptions>): void {
    opts = { ...opts, ...patch };

    const id = doc.selected;
    if (id === null) return;
    const shape = doc.shapes.find((s) => s.id === id);
    if (!shape) return;

    const next = shapeFields(shape.kind, patch);
    /*
     * Nothing survived the filter, so this control does not describe the
     * selected shape. The options bar itself already hides such a control, but
     * a tool shortcut carries a preset with it — pressing `B` with an erase
     * selected lands here with a `redactMode` the erase has no use for.
     *
     * Bailing BEFORE `beginDrag` and `updateShape` is the point: both push an
     * undo entry, and an entry that restores identical pixels is worse than no
     * entry — Ctrl+Z would appear to do nothing, and the document would be
     * marked dirty enough to prompt on close.
     */
    if (Object.keys(next).length === 0) return;

    if (optionDrag && !doc.dragging) doc.beginDrag("restyle");
    doc.updateShape(id, next);
  }

  function deleteSelected(): void {
    if (doc.selected === null) return;
    doc.removeShape(doc.selected);
  }

  /* ------------------------------------------------------------------ zoom */

  function zoomBy(direction: 1 | -1): void {
    fit = false;
    if (direction > 0) {
      zoom = ZOOM_STEPS.find((z) => z > zoom + 1e-4) ?? ZOOM_STEPS[ZOOM_STEPS.length - 1];
    } else {
      const below = ZOOM_STEPS.filter((z) => z < zoom - 1e-4);
      zoom = below.length > 0 ? below[below.length - 1] : ZOOM_STEPS[0];
    }
  }

  /* --------------------------------------------------------------- actions */

  async function doCopy(): Promise<void> {
    if (!canAct || job !== null) return;
    job = "copy";
    try {
      await copyEdit(await renderToPng(doc));
      toast.success("Copied to clipboard");
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      job = null;
    }
  }

  async function doSave(replace: boolean): Promise<void> {
    const target = record;
    if (!canAct || job !== null || !target) return;
    job = replace ? "save" : "saveAs";
    try {
      const png = await renderToPng(doc);
      // Adopt whatever Rust hands back: after "Save as new" the editor is now
      // editing the new record, so a following Ctrl+S replaces *that* file.
      record = await saveEdit(target.id, png, replace);
      doc.markClean();
      toast.success(replace ? "Saved" : "Saved as a new capture");
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      job = null;
    }
  }

  /* -------------------------------------------------------------- keyboard */

  /** Text entry swallows every shortcut — including the canvas text caret. */
  function isTyping(target: EventTarget | null): boolean {
    const el = target as HTMLElement | null;
    if (!el) return false;
    if (el.isContentEditable) return true;
    const tag = el.tagName;
    return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
  }

  function onEscape(): void {
    // ARCHITECTURE-M2 §4.4: drag (owned by the stage, handled above) -> tool ->
    // selection -> close.
    if (tool !== "select") {
      tool = "select";
      return;
    }
    if (doc.selected !== null) {
      doc.selected = null;
      return;
    }
    void requestClose();
  }

  function onKeydown(event: KeyboardEvent): void {
    // A live drag or an open text caret owns the keyboard; the stage answers
    // Escape and typing itself, and a bare `A` there must stay an `A`.
    if (isTyping(event.target) || stageBusy) return;

    // Escape stays live in every phase — it is the only way out of the error
    // state without reaching for the mouse.
    if (event.key === "Escape") {
      event.preventDefault();
      onEscape();
      return;
    }

    if (event.altKey || phase !== "ready") return;

    const mod = event.ctrlKey || event.metaKey;
    const key = event.key.toLowerCase();

    if (mod) {
      switch (key) {
        case "z":
          event.preventDefault();
          if (event.shiftKey) doc.redo();
          else doc.undo();
          return;
        case "y":
          event.preventDefault();
          doc.redo();
          return;
        case "s":
          if (!canAct) return;
          event.preventDefault();
          void doSave(!event.shiftKey);
          return;
        case "c":
          // Ctrl+C means "copy the image" only when nothing is selected;
          // with a selection it belongs to the stage.
          if (doc.selected !== null || !canAct) return;
          event.preventDefault();
          void doCopy();
          return;
        case "0":
          event.preventDefault();
          fit = true;
          return;
        case "=":
        case "+":
          event.preventDefault();
          zoomBy(1);
          return;
        case "-":
          event.preventDefault();
          zoomBy(-1);
          return;
        default:
          return;
      }
    }

    if (event.key === "Delete" || event.key === "Backspace") {
      if (doc.selected === null) return;
      event.preventDefault();
      deleteSelected();
      return;
    }

    // Single-key tool switch. Checked last so Ctrl+S never lands on Save-as-new.
    // `B`/`P` both land on Redact and bring the mode with them, matching
    // ShareX's separate Blur and Pixelate tools.
    const hit = toolHitForKey(key);
    if (hit) {
      event.preventDefault();
      tool = hit.tool.id;
      if (hit.preset) applyOptions(hit.preset);
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<div class="editor">
  <Toolbar
    {tool}
    {opts}
    {selectedKind}
    canUndo={doc.canUndo}
    canRedo={doc.canRedo}
    hasCrop={doc.crop !== null}
    onTool={(next) => (tool = next)}
    onOptions={applyOptions}
    onOptionsGesture={optionGesture}
    onUndo={() => doc.undo()}
    onRedo={() => doc.redo()}
    onDelete={deleteSelected}
    onResetCrop={() => doc.setCrop(null)}
  />

  <main class="stage-area">
    {#if phase === "loading"}
      <div class="state">
        <span class="spinner big" aria-hidden="true"></span>
        <p class="state-text">Loading capture&hellip;</p>
      </div>
    {:else if phase === "error"}
      <div class="state">
        <div class="empty">
          <span class="empty-icon"><Icon name="alert" size={26} /></span>
          <h2 class="empty-title">{fault?.title ?? "Something went wrong"}</h2>
          {#if fault?.path}
            <p class="fault-path selectable">{fault.path}</p>
          {/if}
          <p class="empty-text">{fault?.text ?? ""}</p>
          <button type="button" class="btn" onclick={() => void requestClose()}>
            Close editor
          </button>
        </div>
      </div>
    {:else}
      <Stage
        {tool}
        {opts}
        bind:zoom
        bind:fit
        bind:busy={stageBusy}
        onToolDone={() => (tool = "select")}
      />
    {/if}
  </main>

  <footer class="foot">
    <div class="foot-left">
      <div class="zoom" role="group" aria-label="Zoom">
        <button
          type="button"
          class="zoom-btn"
          aria-label="Zoom out"
          disabled={phase !== "ready"}
          onclick={() => zoomBy(-1)}
        >
          <Icon name="zoom-out" size={15} />
        </button>
        <button
          type="button"
          class="zoom-level tnum"
          aria-label="Fit to window"
          disabled={phase !== "ready"}
          onclick={() => (fit = true)}
        >
          {Math.round(zoom * 100)}%
        </button>
        <button
          type="button"
          class="zoom-btn"
          aria-label="Zoom in"
          disabled={phase !== "ready"}
          onclick={() => zoomBy(1)}
        >
          <Icon name="zoom-in" size={15} />
        </button>
      </div>

      {#if phase === "ready"}
        <span class="dims tnum">{outSize.width} &times; {outSize.height}</span>
        {#if doc.crop}
          <span class="chip">Cropped</span>
        {/if}
        {#if doc.shapes.length > 0}
          <span class="meta tnum">
            {doc.shapes.length}
            {doc.shapes.length === 1 ? "annotation" : "annotations"}
          </span>
        {/if}
        {#if doc.dirty}
          <span class="meta">Unsaved changes</span>
        {/if}
      {/if}
    </div>

    <div class="foot-right">
      <button type="button" class="btn btn-ghost" onclick={() => void requestClose()}>
        Cancel
      </button>
      <button
        type="button"
        class="btn"
        disabled={!canAct || job !== null}
        aria-busy={job === "copy"}
        onclick={() => void doCopy()}
      >
        {#if job === "copy"}<span class="spinner" aria-hidden="true"></span>{/if}
        Copy
      </button>
      <button
        type="button"
        class="btn"
        disabled={!canAct || job !== null}
        aria-busy={job === "saveAs"}
        onclick={() => void doSave(false)}
      >
        {#if job === "saveAs"}<span class="spinner" aria-hidden="true"></span>{/if}
        Save as new
      </button>
      <button
        type="button"
        class="btn btn-primary"
        disabled={!canAct || job !== null}
        aria-busy={job === "save"}
        onclick={() => void doSave(true)}
      >
        {#if job === "save"}<span class="spinner" aria-hidden="true"></span>{/if}
        Save
      </button>
    </div>
  </footer>
</div>

<Toast />

<style>
  .editor {
    display: flex;
    flex-direction: column;
    height: 100vh;
    overflow: hidden;
    background: var(--bg);
    color: var(--text);
  }

  /* The stage sits on the inset backdrop so the image itself reads as the
     lit object in the window, the way a native editor canvas does. */
  .stage-area {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
    background: var(--bg-inset);
    box-shadow: inset 0 1px 0 color-mix(in srgb, var(--text) 6%, transparent);
  }

  .stage-area > :global(*) {
    flex: 1;
    min-width: 0;
    min-height: 0;
  }

  .state {
    flex: 1;
    display: grid;
    place-items: center;
    gap: 12px;
    padding: 32px;
    align-content: center;
  }

  .state-text {
    font-size: 13px;
    color: var(--text-dim);
  }

  .state .empty {
    max-width: 460px;
    gap: 6px;
    background: var(--bg-raised);
    border-style: solid;
  }

  .state .empty .btn {
    margin-top: 12px;
  }

  .fault-path {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-faint);
    word-break: break-all;
  }

  .foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    height: 48px;
    flex: none;
    padding: 0 12px;
    background: var(--bg-raised);
    border-top: 1px solid var(--border);
  }

  .foot-left,
  .foot-right {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }

  .foot-left {
    gap: 12px;
  }

  .zoom {
    display: flex;
    align-items: center;
    flex: none;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-inset);
    overflow: hidden;
  }

  .zoom-btn,
  .zoom-level {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    height: 26px;
    background: transparent;
    color: var(--text-dim);
    cursor: default;
    transition:
      background-color 120ms ease,
      color 120ms ease;
  }

  .zoom-btn {
    width: 28px;
  }

  .zoom-level {
    min-width: 54px;
    padding: 0 6px;
    border-left: 1px solid var(--border);
    border-right: 1px solid var(--border);
    font-size: 12px;
    color: var(--text);
  }

  .zoom-btn:hover:not(:disabled),
  .zoom-level:hover:not(:disabled) {
    background: color-mix(in srgb, var(--text) 8%, transparent);
    color: var(--text);
  }

  .zoom-btn:disabled,
  .zoom-level:disabled {
    color: var(--text-faint);
    pointer-events: none;
  }

  .zoom-btn:focus-visible,
  .zoom-level:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }

  .dims {
    font-size: 12px;
    color: var(--text);
    white-space: nowrap;
  }

  .meta {
    font-size: 12px;
    color: var(--text-faint);
    white-space: nowrap;
  }

  /* Reserves its own width so a button does not resize the row when it starts
     working — a 4K encode keeps the spinner up long enough to notice. */
  .spinner {
    width: 13px;
    height: 13px;
    flex: none;
    border: 2px solid color-mix(in srgb, currentColor 28%, transparent);
    border-top-color: currentColor;
    border-radius: 50%;
    animation: spin 620ms linear infinite;
  }

  .spinner.big {
    width: 22px;
    height: 22px;
    color: var(--text-faint);
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .spinner {
      animation-duration: 1600ms;
    }
  }
</style>
