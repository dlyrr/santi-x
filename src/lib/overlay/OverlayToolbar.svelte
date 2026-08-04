<script module lang="ts">
  import { TOOLS as EDITOR_TOOLS } from "$lib/editor/Toolbar.svelte";
  import type { ToolSpec } from "$lib/editor/Toolbar.svelte";
  import type { ToolId } from "$lib/editor/tools";

  const BY_ID = new Map<ToolId, ToolSpec>(EDITOR_TOOLS.map((spec) => [spec.id, spec]));
  /** Every id below is a `ToolId`, and every `ToolId` is in the M2 table. */
  const specOf = (id: ToolId): ToolSpec => BY_ID.get(id) as ToolSpec;

  /**
   * The overlay's Region tool — M2.7 §3, and the head of the bar.
   *
   * It is the engine's `select` id wearing a different hat. In the editor
   * `select` is the neutral tool; in the overlay the neutral tool is the one
   * that drags out the region, which is also the tool the overlay opens in and
   * the one Escape returns to. Reusing the id keeps `tool` a plain `ToolId`
   * everywhere — the shared options table, `toolById()` and the key map all
   * keep working — and it inherits `V`, which is the key M2.7 gives Region.
   *
   * Only the presentation is the overlay's own: M1's region glyph, a crosshair,
   * and the label the user is actually looking for.
   */
  export const REGION_TOOL: ToolId = "select";

  export const REGION_SPEC: ToolSpec = {
    ...specOf(REGION_TOOL),
    label: "Region",
    cursor: "crosshair",
    icon: "region"
  };

  /**
   * The bar's layout — M2.6 §3, re-headed by M2.7 §3: Region, then shapes,
   * drawing, text/step, redaction, then colour + stroke, Undo/Redo, and the two
   * actions. Ids only: the label, shortcut, cursor and icon still come from the
   * M2 table, so the bar can never offer a tool the shared engine does not
   * implement.
   *
   * `crop` is absent because in the overlay the region *is* the crop and there
   * is nothing to re-crop afterwards (M2.5 §3). Region is not in this table
   * because it is not one of the drawing tools: it is the mode the rest of them
   * are switched away from, so it gets its own labelled button ahead of them.
   */
  const GROUPS: readonly { label: string; tools: readonly ToolId[] }[] = [
    { label: "Shapes", tools: ["rect", "ellipse", "line", "arrow"] },
    { label: "Drawing", tools: ["pen", "highlight"] },
    { label: "Text", tools: ["text", "step"] },
    { label: "Redaction", tools: ["redact"] }
  ];

  const TOOL_GROUPS: readonly { label: string; tools: readonly ToolSpec[] }[] = GROUPS.map((g) => ({
    label: g.label,
    tools: g.tools.map(specOf)
  }));

  /**
   * Every tool the bar shows, flattened, in bar order — Region included, since
   * its key must map like any other. Exported so `RegionOverlay` builds its
   * key → tool map from these same rows rather than a second list that could
   * drift out of step with the buttons.
   */
  export const OVERLAY_TOOLS: readonly ToolSpec[] = [
    REGION_SPEC,
    ...TOOL_GROUPS.flatMap((g) => g.tools)
  ];
</script>

<script lang="ts">
  import Icon from "$lib/components/Icon.svelte";
  import { PRESETS } from "$lib/editor/ColorPicker.svelte";
  import { toolById } from "$lib/editor/tools";
  import type { ToolOption, ToolOptions } from "$lib/editor/tools";

  interface Props {
    /** Active tool; `REGION_TOOL` is the default and means "drag out the region". */
    tool: ToolId;
    /** Live value of every option control (colour, stroke, fill, sizes…). */
    opts: ToolOptions;
    canUndo: boolean;
    canRedo: boolean;
    /**
     * False until there is something to capture — a region, or shapes drawn on
     * the frozen desktop (M2.7 §3). The bar is up before either exists.
     */
    canCommit: boolean;
    onTool: (tool: ToolId) => void;
    onOptions: (patch: Partial<ToolOptions>) => void;
    /**
     * Brackets a continuous option gesture, exactly as the editor's toolbar does
     * (M2 §4.2): a slider drag fires `oninput` on every pixel, and with a shape
     * selected each one is a document mutation. `true` on press, `false` on
     * release, so the whole sweep folds into a single undo entry.
     */
    onOptionsGesture?: (active: boolean) => void;
    onUndo: () => void;
    onRedo: () => void;
    /** Enter: take the shot. */
    onCommit: () => void;
    /** Esc: abandon the capture. */
    onCancel: () => void;
    /**
     * The pointer entered (`true`) or left (`false`) the bar. Since M2.6 §3 the
     * bar is up from the moment the overlay arms, so it covers desktop the user
     * may want to select: the overlay stands its crosshair, loupe and window
     * highlight down while this is true.
     */
    onHover?: (over: boolean) => void;
  }

  let {
    tool,
    opts,
    canUndo,
    canRedo,
    canCommit,
    onTool,
    onOptions,
    onOptionsGesture,
    onUndo,
    onRedo,
    onCommit,
    onCancel,
    onHover
  }: Props = $props();

  /**
   * Exactly the active tool's options (M2 §4.4), so a control the renderer
   * ignores can never appear — `redact` has no colour, `highlight` no stroke.
   * The neutral state keeps colour + stroke on the bar so they can be set
   * before a tool is picked.
   */
  const shown = $derived<readonly ToolOption[]>(
    tool === REGION_TOOL ? ["color", "stroke"] : toolById(tool).options
  );

  const has = (option: ToolOption): boolean => shown.includes(option);

  /**
   * Spread onto every slider, same as the editor's toolbar. `pointerup` is the
   * normal end; `pointercancel` and `change` are the belt and braces — a gesture
   * that opened and never closed would silently fold every later edit into one
   * undo entry.
   */
  const sliderGesture = {
    onpointerdown: () => onOptionsGesture?.(true),
    onpointerup: () => onOptionsGesture?.(false),
    onpointercancel: () => onOptionsGesture?.(false),
    onchange: () => onOptionsGesture?.(false)
  };

  let swatchEls: (HTMLButtonElement | undefined)[] = $state([]);

  // Both sides are `#rrggbb` in practice — the presets are literals and the
  // native well always yields the long form — so case is the only normalising
  // this needs.
  const colour = $derived(opts.color.trim().toLowerCase());
  const activeSwatch = $derived(PRESETS.findIndex((p) => p.hex.toLowerCase() === colour));
  /** Roving tab stop: a custom colour has no swatch, so it parks on the first. */
  const swatchTabIndex = $derived(activeSwatch === -1 ? 0 : activeSwatch);

  /**
   * The overlay reads keys off `window` and the pointer off its own root, so a
   * click on the bar must neither move focus nor look like a drag on the frozen
   * desktop underneath. Buttons cancel `pointerdown` (which suppresses the
   * compatibility mousedown, and with it the focus change, while leaving the
   * click intact); the bar itself stops every pointer event from bubbling, and
   * cancels `pointerdown` for the same reason — since M2.6 the bar is up during
   * `armed`, where a press that reached the overlay would open a selection.
   */
  function keepFocus(event: PointerEvent): void {
    event.preventDefault();
  }

  function swallow(event: Event): void {
    event.stopPropagation();
  }

  /**
   * The bar's own `pointerdown` guard.
   *
   * `stopPropagation` always: since M2.6 the bar is up during `armed`, where a
   * press that reached the overlay would open a selection under it.
   *
   * `preventDefault` only for what is not a native form control. Cancelling
   * `pointerdown` suppresses the compatibility `mousedown`, and `mousedown` is
   * exactly what Chromium's `<input type="range">` drags its thumb on — so a
   * blanket preventDefault here left the stroke, text-size and redaction
   * sliders looking live and doing nothing. The buttons keep theirs (via
   * `keepFocus`), because for them the suppressed default is only the focus
   * change and `click` still fires either way.
   *
   * The cost is that a slider now takes focus, which is precisely the state the
   * overlay's `ownsEscape()` was narrowed for: a range or colour control never
   * answers its own Escape, so the way out of the overlay survives.
   */
  function swallowDown(event: PointerEvent): void {
    if (!(event.target instanceof HTMLInputElement)) event.preventDefault();
    event.stopPropagation();
  }

  function onContextMenu(event: MouseEvent): void {
    // Right-click anywhere else cancels the capture; over the bar it does nothing.
    event.preventDefault();
    event.stopPropagation();
  }

  /**
   * Activation and navigation keys belong to whichever control has focus: let
   * Enter reach the parent and a focused Undo button would both undo *and*
   * commit the capture. Everything else — Escape, the tool shortcuts, Ctrl+Z —
   * passes through, because those must work with focus anywhere on the bar.
   */
  const WIDGET_KEYS = new Set([
    "Enter",
    " ",
    "Spacebar",
    "ArrowLeft",
    "ArrowRight",
    "ArrowUp",
    "ArrowDown",
    "Home",
    "End"
  ]);

  function onBarKeydown(event: KeyboardEvent): void {
    if (WIDGET_KEYS.has(event.key)) event.stopPropagation();
  }

  function pickColour(hex: string): void {
    if (hex.toLowerCase() !== colour) onOptions({ color: hex });
  }

  /** Arrows move *and* select, as a radiogroup should. */
  function onSwatchKeydown(event: KeyboardEvent, index: number): void {
    let next = index;
    switch (event.key) {
      case "ArrowRight":
      case "ArrowDown":
        next = (index + 1) % PRESETS.length;
        break;
      case "ArrowLeft":
      case "ArrowUp":
        next = (index - 1 + PRESETS.length) % PRESETS.length;
        break;
      case "Home":
        next = 0;
        break;
      case "End":
        next = PRESETS.length - 1;
        break;
      default:
        return;
    }
    event.preventDefault();
    pickColour(PRESETS[next].hex);
    swatchEls[next]?.focus();
  }
</script>

<!-- The listeners on the bar itself are guards, not interactions: they stop
     pointer and widget-key events reaching the overlay underneath. Every real
     control inside is a button or an input. -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
  class="bar"
  role="group"
  aria-label="Capture toolbar"
  onpointerdown={swallowDown}
  onpointermove={swallow}
  onpointerup={swallow}
  onpointercancel={swallow}
  onpointerenter={() => onHover?.(true)}
  onpointerleave={() => onHover?.(false)}
  onclick={swallow}
  ondblclick={swallow}
  oncontextmenu={onContextMenu}
  onkeydown={onBarKeydown}
>
  <!-- Region leads the bar and carries its label, because since M2.7 §3 it is
       the mode the overlay opens in and the one the drawing tools are switched
       away from: an icon indistinguishable from the rest of the strip would
       leave "am I about to select or about to draw?" unanswered. -->
  <div class="group" role="group" aria-label="Capture region">
    <div class="slot">
      <button
        type="button"
        class="mode-btn"
        class:on={tool === REGION_TOOL}
        aria-pressed={tool === REGION_TOOL}
        aria-label="{REGION_SPEC.label} ({REGION_SPEC.key.toUpperCase()})"
        onpointerdown={keepFocus}
        onclick={() => onTool(REGION_TOOL)}
      >
        <Icon name={REGION_SPEC.icon} size={15} />
        {REGION_SPEC.label}
      </button>
      <span class="tip" aria-hidden="true">
        Drag to capture
        <kbd class="tip-key">{REGION_SPEC.key.toUpperCase()}</kbd>
      </span>
    </div>
  </div>

  <span class="rule" aria-hidden="true"></span>

  <div class="tools" role="toolbar" aria-label="Annotation tools" aria-orientation="horizontal">
    {#each TOOL_GROUPS as group, gi (group.label)}
      {#if gi > 0}
        <span class="rule" aria-hidden="true"></span>
      {/if}
      <div class="group" role="group" aria-label={group.label}>
        {#each group.tools as spec (spec.id)}
          <div class="slot">
            <button
              type="button"
              class="icon-btn"
              class:on={tool === spec.id}
              aria-pressed={tool === spec.id}
              aria-label="{spec.label} ({spec.key.toUpperCase()})"
              onpointerdown={keepFocus}
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
    {/each}
  </div>

  <span class="rule" aria-hidden="true"></span>

  <div class="group opts" role="group" aria-label="Style">
    {#if has("color")}
      <div class="swatches" role="radiogroup" aria-label="Annotation colour">
        {#each PRESETS as preset, i (preset.hex)}
          <button
            type="button"
            role="radio"
            class="swatch"
            class:on={i === activeSwatch}
            style="--swatch:{preset.hex}"
            aria-checked={i === activeSwatch}
            aria-label={preset.name}
            tabindex={i === swatchTabIndex ? 0 : -1}
            bind:this={swatchEls[i]}
            onpointerdown={keepFocus}
            onclick={() => pickColour(preset.hex)}
            onkeydown={(e) => onSwatchKeydown(e, i)}
          ></button>
        {/each}
      </div>

      <!-- Doubles as the current-colour readout, so a custom colour that matches
           no swatch is still visible at a glance. -->
      <label class="custom" class:on={activeSwatch === -1}>
        <span class="sr">Custom colour</span>
        <input
          type="color"
          value={colour}
          oninput={(e) => onOptions({ color: e.currentTarget.value })}
        />
      </label>
    {/if}

    {#if has("stroke")}
      <label class="ctl">
        <span class="sr">Stroke width</span>
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
        onpointerdown={keepFocus}
        onclick={() => onOptions({ fill: !opts.fill })}
      >
        Fill
      </button>
    {/if}

    {#if has("textSize")}
      <label class="ctl">
        <span class="sr">Text size</span>
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
        <button
          type="button"
          class="seg-btn"
          class:on={opts.redactMode === "blur"}
          aria-pressed={opts.redactMode === "blur"}
          onpointerdown={keepFocus}
          onclick={() => onOptions({ redactMode: "blur" })}
        >
          Blur
        </button>
        <button
          type="button"
          class="seg-btn"
          class:on={opts.redactMode === "pixelate"}
          aria-pressed={opts.redactMode === "pixelate"}
          onpointerdown={keepFocus}
          onclick={() => onOptions({ redactMode: "pixelate" })}
        >
          Pixelate
        </button>
      </div>
    {/if}

    {#if has("redactAmount")}
      <label class="ctl">
        <span class="sr">{opts.redactMode === "blur" ? "Blur radius" : "Block size"}</span>
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
    {/if}
  </div>

  <span class="rule" aria-hidden="true"></span>

  <div class="group" role="group" aria-label="History">
    <div class="slot">
      <button
        type="button"
        class="icon-btn"
        aria-label="Undo (Ctrl+Z)"
        disabled={!canUndo}
        onpointerdown={keepFocus}
        onclick={onUndo}
      >
        <Icon name="undo" size={16} />
      </button>
      <span class="tip" aria-hidden="true">Undo <kbd class="tip-key">Ctrl Z</kbd></span>
    </div>
    <div class="slot">
      <button
        type="button"
        class="icon-btn"
        aria-label="Redo (Ctrl+Shift+Z)"
        disabled={!canRedo}
        onpointerdown={keepFocus}
        onclick={onRedo}
      >
        <Icon name="redo" size={16} />
      </button>
      <span class="tip" aria-hidden="true">Redo <kbd class="tip-key">Ctrl &#8679; Z</kbd></span>
    </div>
  </div>

  <span class="rule" aria-hidden="true"></span>

  <!-- The two actions carry their shortcut inline rather than in a tooltip:
       they sit at the end of the bar, where a centred tip would hang off the
       edge of the desktop. -->
  <div class="group" role="group" aria-label="Capture">
    <button type="button" class="act" onpointerdown={keepFocus} onclick={onCancel}>
      <Icon name="x" size={13} />
      Cancel
      <kbd class="act-key">Esc</kbd>
    </button>
    <button
      type="button"
      class="act primary"
      disabled={!canCommit}
      onpointerdown={keepFocus}
      onclick={onCommit}
    >
      <Icon name="camera" size={13} />
      Capture
      <kbd class="act-key">&#9166;</kbd>
    </button>
  </div>
</div>

<style>
  /* Anchoring is the parent's; this only ever describes the bar itself. It
     floats over arbitrary desktop pixels, so it carries a solid --surface fill
     and stacks the theme shadow with the overlay's own panel shadow — the
     `claude` --shadow alone is a hairline lift, invisible against a busy
     wallpaper.

     Dense and icon-first (M2.6 §3): 28px squares in a tight row with dividers
     between the groups, so the whole tool set reads as one strip the way
     ShareX's does. */
  .bar {
    display: flex;
    align-items: center;
    gap: 5px;
    padding: 0 6px;
    height: 38px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--surface);
    box-shadow:
      var(--shadow),
      var(--ov-panel-shadow);
    color: var(--text);
    font-family: var(--font);
    user-select: none;
    -webkit-user-select: none;
    /* Tooltips hang below the bar and must clear the scrim and marquee. */
    position: relative;
    cursor: default;
  }

  /* One row for the whole tool set, dividers included: the groups are an aria
     and visual grouping, not a layout of their own. */
  .tools {
    display: flex;
    align-items: center;
    gap: 5px;
    flex: none;
  }

  .group {
    display: flex;
    align-items: center;
    gap: 1px;
    flex: none;
  }

  .opts {
    gap: 8px;
  }

  .rule {
    width: 1px;
    height: 20px;
    flex: none;
    background: var(--border);
  }

  .slot {
    position: relative;
    display: flex;
  }

  .icon-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
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

  .icon-btn:hover:not(:disabled) {
    background: color-mix(in srgb, var(--text) 8%, transparent);
    color: var(--text);
  }

  .icon-btn:active:not(:disabled) {
    background: color-mix(in srgb, var(--text) 13%, transparent);
  }

  .icon-btn.on {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    border-color: color-mix(in srgb, var(--accent) 45%, transparent);
    color: var(--accent);
  }

  .icon-btn:disabled {
    color: var(--text-faint);
    opacity: 0.55;
    pointer-events: none;
  }

  /* The one labelled tool on the bar. Sized to the icon row so it does not
     break the strip's 28px rhythm, and its active state is louder than a tool
     button's — this is the mode readout, not just a pressed toggle. */
  .mode-btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 28px;
    flex: none;
    padding: 0 9px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-inset);
    color: var(--text-dim);
    font-size: 12px;
    font-weight: 500;
    white-space: nowrap;
    cursor: default;
    transition:
      background-color 120ms ease,
      color 120ms ease,
      border-color 120ms ease;
  }

  .mode-btn:hover:not(.on) {
    color: var(--text);
    border-color: var(--border-strong);
  }

  .mode-btn.on {
    background: color-mix(in srgb, var(--accent) 22%, transparent);
    border-color: color-mix(in srgb, var(--accent) 60%, transparent);
    color: var(--accent);
    font-weight: 600;
  }

  .icon-btn:focus-visible,
  .mode-btn:focus-visible,
  .pill:focus-visible,
  .act:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .tip {
    position: absolute;
    top: calc(100% + 8px);
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

  /* Delayed on hover so sweeping the strip does not strobe, immediate on
     keyboard focus where the user is already waiting for the label. */
  .slot:hover .tip {
    opacity: 1;
    transform: translate(-50%, 0);
    transition-delay: 380ms;
  }

  .icon-btn:focus-visible + .tip,
  .mode-btn:focus-visible + .tip {
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

  /* Palette: the eight literal hex values are annotation *content* and come
     from ColorPicker's PRESETS — the single list Settings and the editor share
     (ARCHITECTURE-M2 §5). Compact swatches rather than the editor's picker:
     this bar is read at a glance mid-task, not browsed. */
  .swatches {
    display: flex;
    align-items: center;
    gap: 4px;
    flex: none;
  }

  .swatch {
    width: 16px;
    height: 16px;
    flex: none;
    padding: 0;
    border: 1px solid var(--border-strong);
    border-radius: 4px;
    background: var(--swatch);
    cursor: default;
    transition:
      transform 120ms ease,
      box-shadow 120ms ease;
  }

  .swatch:hover {
    transform: scale(1.14);
  }

  /* Two rings: the inner one is the bar's own surface punching a gap, so the
     accent ring reads against white and near-black swatches alike. */
  .swatch.on,
  .custom.on {
    box-shadow:
      0 0 0 2px var(--surface),
      0 0 0 3.5px var(--accent);
  }

  .swatch:focus-visible,
  .custom input:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 3px;
  }

  .custom {
    position: relative;
    display: block;
    width: 18px;
    height: 18px;
    flex: none;
    border-radius: 4px;
  }

  /* Dashed so it reads as "pick anything" even when its colour happens to
     equal one of the presets. */
  .custom input {
    width: 100%;
    height: 100%;
    padding: 0;
    border: 1px dashed var(--border-strong);
    border-radius: 4px;
    background: none;
    cursor: default;
  }

  .custom input::-webkit-color-swatch-wrapper {
    padding: 2px;
  }

  .custom input::-webkit-color-swatch {
    border: none;
    border-radius: 2px;
  }

  /* Icon-only everywhere else, so the numeric controls drop their text label
     too and keep only the live value — the slider's own aria-label carries the
     meaning for assistive tech. */
  .ctl {
    display: flex;
    align-items: center;
    gap: 5px;
    flex: none;
  }

  .ctl-value {
    min-width: 17px;
    font-size: 11px;
    color: var(--text-dim);
    text-align: right;
  }

  .slider {
    -webkit-appearance: none;
    appearance: none;
    width: 66px;
    height: 16px;
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
    border: 2px solid var(--surface);
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
    height: 22px;
    flex: none;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-inset);
    color: var(--text-dim);
    font-size: 11px;
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

  .seg {
    display: flex;
    flex: none;
    padding: 2px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-inset);
  }

  .seg-btn {
    height: 18px;
    padding: 0 7px;
    border: none;
    border-radius: 4px;
    background: transparent;
    color: var(--text-dim);
    font-size: 11px;
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
    background: var(--surface);
    color: var(--text);
    box-shadow: 0 1px 2px color-mix(in srgb, var(--text) 12%, transparent);
  }

  .seg-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -1px;
  }

  .act {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 28px;
    flex: none;
    padding: 0 9px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    color: var(--text);
    font-size: 12px;
    font-weight: 500;
    white-space: nowrap;
    cursor: default;
    /* One timing for every button on the bar (M2.9 §2). Hover and active only —
       nothing here animates on entrance. */
    transition:
      background-color 120ms ease,
      border-color 120ms ease,
      color 120ms ease;
  }

  .act:hover:not(:disabled) {
    background: color-mix(in srgb, var(--text) 7%, var(--bg-raised));
    border-color: var(--border-strong);
  }

  .act:active:not(:disabled) {
    background: color-mix(in srgb, var(--text) 12%, var(--bg-raised));
  }

  .act.primary {
    margin-left: 2px;
    padding: 0 11px;
    background: var(--accent);
    border-color: transparent;
    color: var(--accent-text);
    font-weight: 600;
  }

  .act.primary:hover:not(:disabled) {
    background: var(--accent-hover);
    border-color: transparent;
  }

  .act.primary:active:not(:disabled) {
    background: color-mix(in srgb, var(--text) 10%, var(--accent-hover));
  }

  /* Capture is up before a region exists, so it spends the `armed` phase
     disabled rather than firing a commit that would only cancel. */
  .act:disabled {
    opacity: 0.45;
    pointer-events: none;
  }

  .act-key {
    padding: 2px 5px;
    border-radius: 4px;
    background: color-mix(in srgb, var(--text) 9%, transparent);
    color: var(--text-dim);
    font-family: var(--font-mono);
    font-size: 10px;
    line-height: 1;
  }

  .act.primary .act-key {
    background: color-mix(in srgb, var(--accent-text) 22%, transparent);
    color: var(--accent-text);
  }

  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }

  @media (prefers-reduced-motion: reduce) {
    .tip,
    .swatch,
    .icon-btn,
    .mode-btn,
    .pill,
    .seg-btn,
    .act {
      transition: none;
    }

    /* The only transform on the bar, and the one thing here that actually moves. */
    .swatch:hover {
      transform: none;
    }
  }
</style>
