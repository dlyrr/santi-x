<script module lang="ts">
  /**
   * These eight values are the one documented exception to the
   * "no hard-coded colour" rule (ARCHITECTURE-M2 §5). They are annotation
   * *content* — they are burned into the exported PNG and must look identical
   * whichever theme the app is wearing — so they are literal hex, not tokens.
   * The surrounding chrome below reads tokens only.
   */
  export const PRESETS: readonly { hex: string; name: string }[] = [
    { hex: "#f2555a", name: "Red" },
    { hex: "#f97316", name: "Orange" },
    { hex: "#facc15", name: "Yellow" },
    { hex: "#22c55e", name: "Green" },
    { hex: "#3b82f6", name: "Blue" },
    { hex: "#a855f7", name: "Violet" },
    { hex: "#ffffff", name: "White" },
    { hex: "#141414", name: "Black" }
  ];

  /** `#ABC` and `#AABBCC` both reach us; compare on a single normal form. */
  function normalise(hex: string): string {
    const v = hex.trim().toLowerCase();
    const short = /^#([0-9a-f])([0-9a-f])([0-9a-f])$/.exec(v);
    if (short) return `#${short[1]}${short[1]}${short[2]}${short[2]}${short[3]}${short[3]}`;
    return /^#[0-9a-f]{6}$/.test(v) ? v : v;
  }
</script>

<script lang="ts">
  interface Props {
    value: string;
    onChange: (color: string) => void;
    /** Announced on the radiogroup; distinguishes it from any future picker. */
    label?: string;
  }

  let { value, onChange, label = "Annotation colour" }: Props = $props();

  let swatchEls: (HTMLButtonElement | undefined)[] = $state([]);

  const current = $derived(normalise(value));
  const activeIndex = $derived(PRESETS.findIndex((p) => p.hex === current));
  /** Custom colours have no swatch, so the roving tab stop parks on the first. */
  const tabIndex = $derived(activeIndex === -1 ? 0 : activeIndex);

  function pick(hex: string): void {
    if (normalise(hex) === current) return;
    onChange(hex);
  }

  /**
   * Roving-tabindex radiogroup: one tab stop for the whole strip, arrows move
   * *and* select, which is the expected behaviour for a radio group and lets a
   * keyboard user scrub the palette the way a pointer user hovers it.
   */
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
    // Stop here too: the editor's window-level handler reads bare arrow keys.
    event.stopPropagation();
    pick(PRESETS[next].hex);
    swatchEls[next]?.focus();
  }
</script>

<div class="picker">
  <div class="swatches" role="radiogroup" aria-label={label}>
    {#each PRESETS as preset, i (preset.hex)}
      <button
        type="button"
        role="radio"
        class="swatch"
        class:on={i === activeIndex}
        style="--swatch:{preset.hex}"
        aria-checked={i === activeIndex}
        aria-label={preset.name}
        tabindex={i === tabIndex ? 0 : -1}
        bind:this={swatchEls[i]}
        onclick={() => pick(preset.hex)}
        onkeydown={(e) => onSwatchKeydown(e, i)}
      ></button>
    {/each}
  </div>

  <span class="divider" aria-hidden="true"></span>

  <!-- Doubles as the current-colour readout: it always shows `value`, so a
       custom colour that matches no swatch is still visible at a glance. -->
  <label class="custom" class:on={activeIndex === -1}>
    <span class="sr">Custom colour</span>
    <input
      type="color"
      value={current}
      oninput={(e) => onChange(e.currentTarget.value)}
    />
  </label>
  <span class="hex tnum">{current.toUpperCase()}</span>
</div>

<style>
  .picker {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: none;
  }

  .swatches {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .swatch {
    width: 22px;
    height: 22px;
    flex: none;
    padding: 0;
    border: 1px solid var(--border-strong);
    border-radius: 6px;
    background: var(--swatch);
    cursor: default;
    transition:
      transform 120ms ease,
      box-shadow 120ms ease;
  }

  .swatch:hover {
    transform: scale(1.12);
  }

  /* Two rings: the inner one is the toolbar background punching a gap so the
     accent ring reads against dark and light swatches alike. */
  .swatch.on {
    box-shadow:
      0 0 0 2px var(--bg-raised),
      0 0 0 3.5px var(--accent);
  }

  .swatch:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 3px;
  }

  .divider {
    width: 1px;
    height: 18px;
    background: var(--border);
  }

  .custom {
    position: relative;
    display: block;
    width: 24px;
    height: 24px;
    flex: none;
    border-radius: 6px;
    overflow: hidden;
  }

  .custom.on {
    box-shadow:
      0 0 0 2px var(--bg-raised),
      0 0 0 3.5px var(--accent);
  }

  /* The native well carries a dashed hint so it is legible as "pick anything"
     even when its colour happens to equal one of the presets. */
  .custom input {
    width: 100%;
    height: 100%;
    padding: 0;
    border: 1px dashed var(--border-strong);
    border-radius: 6px;
    background: none;
    cursor: default;
  }

  .custom input::-webkit-color-swatch-wrapper {
    padding: 2px;
  }

  .custom input::-webkit-color-swatch {
    border: none;
    border-radius: 4px;
  }

  .custom input:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .hex {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-faint);
    letter-spacing: 0.02em;
    white-space: nowrap;
  }

  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
