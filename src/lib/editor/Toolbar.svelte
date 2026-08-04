<script module lang="ts">
  import type { IconName } from "$lib/components/Icon.svelte";
  import { TOOLS as TOOL_DEFS } from "$lib/editor/tools";
  import type { ToolDef, ToolId } from "$lib/editor/tools";

  /** A tool row plus the one thing the engine has no opinion about: its glyph. */
  export type ToolSpec = ToolDef & { icon: IconName };

  /**
   * `crop` reuses the M1 region glyph — its two corner marks already *are* a
   * crop mark.
   */
  const ICONS: Record<ToolId, IconName> = {
    select: "cursor",
    arrow: "arrow",
    rect: "square",
    ellipse: "circle",
    line: "line",
    pen: "pen",
    text: "text",
    highlight: "highlighter",
    redact: "droplet",
    step: "step",
    crop: "crop"
  };

  /**
   * Toolbar order, labels and shortcuts all come from `tools.ts` — the single
   * table the contract defines (M2 §4.4). Only the icons are added here, so the
   * bar cannot drift from the tools the stage actually implements. Exported so
   * `EditorRoot` builds its key → tool map from these same rows.
   */
  export const TOOLS: readonly ToolSpec[] = TOOL_DEFS.map((def) => ({
    ...def,
    icon: ICONS[def.id]
  }));
</script>

<script lang="ts">
  import Icon from "$lib/components/Icon.svelte";
  import ColorPicker from "$lib/editor/ColorPicker.svelte";
  import { optionsForKind, toolById } from "$lib/editor/tools";
  import type { RedactMode, ToolOption, ToolOptions } from "$lib/editor/tools";
  import type { ShapeKind } from "$lib/editor/doc.svelte";

  interface Props {
    tool: ToolId;
    opts: ToolOptions;
    /** `kind` of the selected shape, or null. Drives the Select tool's options. */
    selectedKind: ShapeKind | null;
    canUndo: boolean;
    canRedo: boolean;
    hasCrop: boolean;
    onTool: (tool: ToolId) => void;
    onOptions: (patch: Partial<ToolOptions>) => void;
    /**
     * Brackets a continuous option gesture — a slider drag fires `oninput` on
     * every pixel, and with a shape selected each one is a document mutation.
     * `true` on press, `false` on release, so the whole sweep can fold into a
     * single undo entry (M2 §4.2).
     */
    onOptionsGesture?: (active: boolean) => void;
    onUndo: () => void;
    onRedo: () => void;
    onDelete: () => void;
    onResetCrop: () => void;
    /**
     * M5 §3. False when the capture has no file on disk — `ocr_capture` reads
     * the PNG back, exactly as the Lightbox and CaptureCard actions do, so with
     * `saveToDisk` off there is nothing to read.
     */
    canOcr: boolean;
    /**
     * True when the document has unsaved edits. OCR runs against the *file*, so
     * the button says so rather than quietly reading a stale image.
     */
    ocrStale?: boolean;
    onOcr: () => void;
  }

  let {
    tool,
    opts,
    selectedKind,
    canUndo,
    canRedo,
    hasCrop,
    onTool,
    onOptions,
    onOptionsGesture,
    onUndo,
    onRedo,
    onDelete,
    onResetCrop,
    canOcr,
    ocrStale = false,
    onOcr
  }: Props = $props();

  /**
   * Short enough for the tooltip, which is `white-space: nowrap` and centred on
   * its button — and this button sits in `.actions`, hard against the right end
   * of the bar, so a long one would hang off the window. The full sentence goes
   * in `title` instead, where the platform wraps and repositions it.
   */
  const ocrTip = $derived(!canOcr ? "Not on disk" : ocrStale ? "Saved file" : "Extract text");

  const ocrTitle = $derived(
    !canOcr
      ? "This capture was never written to disk, so there is no image to read text from"
      : ocrStale
        ? "Extract text — reads the saved file, so unsaved edits are not included"
        : "Read the text out of this capture"
  );

  // With Select active the options bar describes whatever is selected, so the
  // same colour/width controls edit an existing shape instead of a future one.
  const shown = $derived<readonly ToolOption[]>(
    tool === "select"
      ? selectedKind
        ? optionsForKind(selectedKind)
        : []
      : toolById(tool).options
  );

  const has = (option: ToolOption): boolean => shown.includes(option);
  const empty = $derived(shown.length === 0);

  /**
   * The three redaction modes and their keys (M5 §2). The keys are the ones
   * `tools.ts` maps — `X` selects Redact in erase mode now that the smart
   * eraser is not a tool of its own — and they are shown because a mode is the
   * only thing on this bar a shortcut lands on.
   */
  const REDACT_MODES: readonly { mode: RedactMode; label: string; key: string }[] = [
    { mode: "blur", label: "Blur", key: "B" },
    { mode: "pixelate", label: "Pixelate", key: "P" },
    { mode: "erase", label: "Smart eraser", key: "X" }
  ];

  /**
   * The amount means nothing in erase mode — that fill comes entirely from the
   * image — so the slider goes rather than sitting there dead (M5 §2). Its slot
   * keeps its width either way: a control that appears and disappears mid-bar
   * shoves everything beside it sideways under the pointer that just clicked.
   */
  const hasAmount = $derived(has("redactAmount") && opts.redactMode !== "erase");

  /**
   * Spread onto every slider. `pointerup` is the normal end; `pointercancel`
   * and `change` are the belt and braces — a gesture that opened and never
   * closed would silently fold every later edit into one undo entry.
   */
  const sliderGesture = {
    onpointerdown: () => onOptionsGesture?.(true),
    onpointerup: () => onOptionsGesture?.(false),
    onpointercancel: () => onOptionsGesture?.(false),
    onchange: () => onOptionsGesture?.(false)
  };
</script>

<header class="bar">
  <div class="tools" role="toolbar" aria-label="Annotation tools" aria-orientation="horizontal">
    {#each TOOLS as spec (spec.id)}
      <div class="slot">
        <button
          type="button"
          class="tool"
          class:on={tool === spec.id}
          aria-pressed={tool === spec.id}
          aria-label="{spec.label} ({spec.key.toUpperCase()})"
          onclick={() => onTool(spec.id)}
        >
          <Icon name={spec.icon} size={16} />
        </button>
        <span class="tip" aria-hidden="true">
          {spec.label}
          <kbd class="tip-key">{spec.key.toUpperCase()}</kbd>
        </span>
      </div>
    {/each}
  </div>

  <span class="rule" aria-hidden="true"></span>

  <!-- Fixed height, never `display: none` on the whole region: swapping tools
       must not make the toolbar jump. The hint fills the reserved space when a
       tool has no options at all. -->
  <div class="options" aria-label="Tool options">
    {#if has("color")}
      <ColorPicker value={opts.color} onChange={(color) => onOptions({ color })} />
    {/if}

    {#if has("stroke")}
      <label class="ctl">
        <span class="ctl-label">Width</span>
        <input
          class="slider"
          type="range"
          min="1"
          max="24"
          step="1"
          value={opts.stroke}
          aria-label="Stroke width"
          {...sliderGesture}
          oninput={(e) => onOptions({ stroke: e.currentTarget.valueAsNumber })}
        />
        <output class="ctl-value tnum">{opts.stroke}</output>
      </label>
    {/if}

    {#if has("fill")}
      <button
        type="button"
        class="pill"
        class:on={opts.fill}
        aria-pressed={opts.fill}
        onclick={() => onOptions({ fill: !opts.fill })}
      >
        Fill
      </button>
    {/if}

    {#if has("textSize")}
      <label class="ctl">
        <span class="ctl-label">Size</span>
        <input
          class="slider"
          type="range"
          min="10"
          max="120"
          step="2"
          value={opts.textSize}
          aria-label="Text size"
          {...sliderGesture}
          oninput={(e) => onOptions({ textSize: e.currentTarget.valueAsNumber })}
        />
        <output class="ctl-value tnum">{opts.textSize}</output>
      </label>
    {/if}

    {#if has("redactMode")}
      <div class="seg" role="group" aria-label="Redaction mode">
        {#each REDACT_MODES as m (m.mode)}
          <button
            type="button"
            class="seg-btn"
            class:on={opts.redactMode === m.mode}
            aria-pressed={opts.redactMode === m.mode}
            aria-label="{m.label} ({m.key})"
            onclick={() => onOptions({ redactMode: m.mode })}
          >
            {m.label}
          </button>
        {/each}
      </div>
    {/if}

    {#if has("redactAmount")}
      <!-- Fixed-width slot: see `hasAmount`. The slider leaves in erase mode,
           the width does not. -->
      <div class="amount">
        {#if hasAmount}
          <label class="ctl">
            <span class="ctl-label">
              {opts.redactMode === "blur" ? "Radius" : "Blocks"}
            </span>
            <input
              class="slider"
              type="range"
              min="2"
              max="40"
              step="1"
              value={opts.redactAmount}
              aria-label="Redaction amount"
              {...sliderGesture}
              oninput={(e) => onOptions({ redactAmount: e.currentTarget.valueAsNumber })}
            />
            <output class="ctl-value tnum">{opts.redactAmount}</output>
          </label>
        {:else}
          <span class="hint">Fills from nearby pixels</span>
        {/if}
      </div>
    {/if}

    {#if tool === "crop"}
      <span class="hint">
        Drag a region &middot; <kbd class="kbd">Enter</kbd> applies &middot;
        <kbd class="kbd">Esc</kbd> cancels
      </span>
      {#if hasCrop}
        <button type="button" class="pill" onclick={onResetCrop}>Reset crop</button>
      {/if}
    {:else if empty}
      <span class="hint">
        {tool === "select"
          ? "Click a shape to move, resize or restyle it."
          : "This tool has no options."}
      </span>
    {/if}
  </div>

  <span class="rule" aria-hidden="true"></span>

  <div class="actions">
    <!-- M5 §3. Sits with the actions, not the tools: it reads the capture
         rather than changing it, so it never becomes the active tool. -->
    <div class="slot">
      <button
        type="button"
        class="tool"
        aria-label="Extract text"
        title={ocrTitle}
        disabled={!canOcr}
        onclick={onOcr}
      >
        <Icon name="scan" size={16} />
      </button>
      <span class="tip" aria-hidden="true">{ocrTip}</span>
    </div>
    <span class="rule" aria-hidden="true"></span>
    <div class="slot">
      <button
        type="button"
        class="tool"
        aria-label="Undo (Ctrl+Z)"
        disabled={!canUndo}
        onclick={onUndo}
      >
        <Icon name="undo" size={16} />
      </button>
      <span class="tip" aria-hidden="true">Undo <kbd class="tip-key">Ctrl Z</kbd></span>
    </div>
    <div class="slot">
      <button
        type="button"
        class="tool"
        aria-label="Redo (Ctrl+Shift+Z)"
        disabled={!canRedo}
        onclick={onRedo}
      >
        <Icon name="redo" size={16} />
      </button>
      <span class="tip" aria-hidden="true">Redo <kbd class="tip-key">Ctrl &#8679; Z</kbd></span>
    </div>
    <div class="slot">
      <button
        type="button"
        class="tool danger"
        aria-label="Delete selected shape (Delete)"
        disabled={selectedKind === null}
        onclick={onDelete}
      >
        <Icon name="trash" size={16} />
      </button>
      <span class="tip" aria-hidden="true">Delete <kbd class="tip-key">Del</kbd></span>
    </div>
  </div>
</header>

<style>
  .bar {
    display: flex;
    align-items: center;
    gap: 12px;
    height: 52px;
    flex: none;
    padding: 0 12px;
    background: var(--bg-raised);
    border-bottom: 1px solid var(--border);
    /* Tooltips hang below the bar; a stacking context keeps them over the
       stage without each one needing its own z-index. */
    position: relative;
    z-index: 2;
  }

  .tools,
  .actions {
    display: flex;
    align-items: center;
    gap: 2px;
    flex: none;
  }

  .rule {
    width: 1px;
    height: 24px;
    flex: none;
    background: var(--border);
  }

  /* The bar's own gap is 12px, so the outer rules sit in air. Inside `.actions`
     the gap is 2px, which would leave the Extract-text divider touching the
     buttons either side of it. */
  .actions .rule {
    margin: 0 5px;
  }

  .slot {
    position: relative;
    display: flex;
  }

  .tool {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 32px;
    height: 32px;
    flex: none;
    border: 1px solid transparent;
    border-radius: var(--radius);
    background: transparent;
    color: var(--text-dim);
    cursor: default;
    transition:
      background-color 120ms ease,
      color 120ms ease,
      border-color 120ms ease;
  }

  .tool:hover:not(:disabled) {
    background: color-mix(in srgb, var(--text) 8%, transparent);
    color: var(--text);
  }

  .tool:active:not(:disabled) {
    background: color-mix(in srgb, var(--text) 13%, transparent);
  }

  .tool.on {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    border-color: color-mix(in srgb, var(--accent) 45%, transparent);
    color: var(--accent);
  }

  .tool.danger:hover:not(:disabled) {
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 14%, transparent);
  }

  .tool:disabled {
    color: var(--text-faint);
    opacity: 0.55;
    pointer-events: none;
  }

  .tool:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .tip {
    position: absolute;
    top: calc(100% + 10px);
    left: 50%;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 5px 8px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface);
    box-shadow: var(--shadow);
    color: var(--text);
    font-size: 12px;
    line-height: 1;
    white-space: nowrap;
    opacity: 0;
    transform: translate(-50%, -4px);
    pointer-events: none;
    transition:
      opacity 110ms ease,
      transform 110ms ease;
  }

  /* Delayed on hover so sweeping across the strip does not strobe, immediate
     on keyboard focus where the user is already waiting for the label. */
  .slot:hover .tip {
    opacity: 1;
    transform: translate(-50%, 0);
    transition-delay: 380ms;
  }

  .tool:focus-visible + .tip {
    opacity: 1;
    transform: translate(-50%, 0);
    transition-delay: 0ms;
  }

  .tip-key {
    padding: 2px 5px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-inset);
    color: var(--text-dim);
    font-family: var(--font-mono);
    font-size: 10px;
    line-height: 1;
  }

  .options {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 14px;
    flex: 1;
    min-width: 0;
    /* Reserved height: the bar must not resize as the tool changes. */
    height: 34px;
    overflow-x: auto;
    overflow-y: hidden;
    /* An overflowing options strip must not clip the swatch selection ring. */
    padding: 0 4px;
    scrollbar-width: none;
  }

  .options::-webkit-scrollbar {
    height: 0;
  }

  .ctl {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: none;
  }

  /* Wide enough for the label + slider + readout it holds in blur and pixelate
     mode, and it keeps that width in erase mode where there is no amount to
     set. Sized so the substitute hint fits inside it rather than widening it. */
  .amount {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 176px;
    flex: none;
  }

  .ctl-label {
    font-size: 12px;
    color: var(--text-dim);
    white-space: nowrap;
  }

  .ctl-value {
    min-width: 22px;
    font-size: 12px;
    color: var(--text);
    text-align: right;
  }

  .slider {
    -webkit-appearance: none;
    appearance: none;
    width: 96px;
    height: 18px;
    flex: none;
    background: transparent;
    cursor: default;
  }

  .slider::-webkit-slider-runnable-track {
    height: 4px;
    border-radius: 999px;
    background: var(--bg-inset);
    border: 1px solid var(--border);
  }

  .slider::-webkit-slider-thumb {
    -webkit-appearance: none;
    appearance: none;
    width: 12px;
    height: 12px;
    margin-top: -5px;
    border-radius: 50%;
    border: 2px solid var(--bg-raised);
    background: var(--accent);
    box-shadow: 0 0 0 1px var(--border-strong);
  }

  .slider:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
    border-radius: var(--radius);
  }

  .pill {
    display: inline-flex;
    align-items: center;
    height: 26px;
    flex: none;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-inset);
    color: var(--text-dim);
    font-size: 12px;
    font-weight: 500;
    cursor: default;
    transition:
      background-color 120ms ease,
      color 120ms ease,
      border-color 120ms ease;
  }

  .pill:hover {
    color: var(--text);
    border-color: var(--border-strong);
  }

  .pill.on {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    border-color: color-mix(in srgb, var(--accent) 45%, transparent);
    color: var(--accent);
  }

  .pill:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .seg {
    display: flex;
    flex: none;
    padding: 2px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-inset);
  }

  .seg-btn {
    height: 22px;
    padding: 0 10px;
    border: none;
    border-radius: 5px;
    background: transparent;
    color: var(--text-dim);
    font-size: 12px;
    font-weight: 500;
    cursor: default;
    transition:
      background-color 120ms ease,
      color 120ms ease;
  }

  .seg-btn:hover {
    color: var(--text);
  }

  .seg-btn.on {
    background: var(--bg-raised);
    color: var(--text);
    box-shadow: 0 1px 2px color-mix(in srgb, var(--text) 12%, transparent);
  }

  .seg-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -1px;
  }

  .hint {
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: 12px;
    color: var(--text-faint);
    white-space: nowrap;
  }

  .hint .kbd {
    height: 18px;
    min-width: 18px;
  }
</style>
