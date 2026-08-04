<script lang="ts">
  /**
   * The result of `ocr_capture` for one record (M5 §3).
   *
   * Three real states, and the third is the point: a recognition that finds
   * nothing succeeds, so "no text" arrives as an empty string rather than as an
   * error. Rendering that as a blank box would look like a bug in the app
   * instead of a fact about the picture, so it is named.
   *
   * The panel is a modal in its own right and is hosted inside elements that are
   * themselves clickable — a `CaptureCard` is a giant button, a `Lightbox` traps
   * Tab and reads arrow keys. It therefore stops its own pointer and key events
   * from bubbling rather than asking each host to remember to.
   */
  import Icon from '$lib/components/Icon.svelte';
  import { toast } from '$lib/components/Toast.svelte';
  import { errorMessage, ocrCapture, type CaptureRecord, type OcrResult } from '$lib/api';

  interface Props {
    record: CaptureRecord;
    onclose: () => void;
  }

  let { record, onclose }: Props = $props();

  type Status = 'working' | 'done' | 'failed';

  let status = $state<Status>('working');
  let result = $state<OcrResult | null>(null);
  let failure = $state('');
  let copiedAt = $state(0);
  let scrimEl: HTMLDivElement | undefined = $state();
  let dialogEl: HTMLDivElement | undefined = $state();

  // Monotonic. A retry — or a host that swaps the record under us — must not be
  // overtaken by the recognition it replaced.
  let run = 0;
  let copyTimer: ReturnType<typeof setTimeout> | null = null;

  const lines = $derived(result?.lines.filter((line) => line.text.trim() !== '') ?? []);
  /**
   * What Copy writes. `text` is the engine's own joining; the line fallback is
   * for an engine that filled `lines` and left `text` empty, which is a shape
   * difference rather than a "no text" result.
   */
  const body = $derived(
    result && result.text.trim() !== ''
      ? result.text
      : lines.map((line) => line.text).join('\n')
  );
  const found = $derived(status === 'done' && body.trim() !== '');
  const copied = $derived(copiedAt !== 0);

  $effect(() => {
    void recognise(record.id);
  });

  $effect(() => {
    const previous = document.activeElement as HTMLElement | null;
    dialogEl?.focus();
    return () => previous?.focus?.();
  });

  $effect(() => () => {
    if (copyTimer) clearTimeout(copyTimer);
  });

  async function recognise(id: string): Promise<void> {
    const token = ++run;
    status = 'working';
    result = null;
    failure = '';
    try {
      const next = await ocrCapture(id);
      if (token !== run) return;
      result = next;
      status = 'done';
    } catch (err) {
      if (token !== run) return;
      failure = errorMessage(err);
      status = 'failed';
    }
  }

  /**
   * `tauri-plugin-clipboard-manager` is a Rust dependency with no JS half
   * installed, and `copy_capture` writes images, so text goes through the
   * webview. Tauri serves the app from `tauri.localhost`, which WebView2 treats
   * as a trustworthy origin, so the async API is there; the `execCommand` path
   * is for the call being refused for want of document focus, which is the one
   * failure mode that actually shows up.
   */
  async function writeClipboardText(value: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(value);
      return;
    } catch {
      // Fall through to the synchronous path.
    }
    const area = document.createElement('textarea');
    area.value = value;
    area.setAttribute('readonly', '');
    area.style.position = 'fixed';
    area.style.top = '-1000px';
    area.style.opacity = '0';
    document.body.appendChild(area);
    area.select();
    const ok = document.execCommand('copy');
    area.remove();
    if (!ok) throw new Error('The webview refused to write to the clipboard.');
  }

  async function copyAll(): Promise<void> {
    if (!found) return;
    try {
      await writeClipboardText(body);
      if (copyTimer) clearTimeout(copyTimer);
      copiedAt = Date.now();
      copyTimer = setTimeout(() => (copiedAt = 0), 1600);
    } catch (err) {
      toast.error(errorMessage(err));
    }
  }

  async function copyLine(value: string): Promise<void> {
    try {
      await writeClipboardText(value);
      toast.success('Line copied');
    } catch (err) {
      toast.error(errorMessage(err));
    }
  }

  function focusable(): HTMLElement[] {
    if (!dialogEl) return [];
    const nodes = dialogEl.querySelectorAll<HTMLElement>(
      'button:not([disabled]), [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
    );
    return Array.from(nodes).filter((el) => el.offsetParent !== null);
  }

  function onKeydown(event: KeyboardEvent): void {
    // The hosts read Escape, Tab and the arrow keys for their own navigation.
    event.stopPropagation();
    if (event.key === 'Escape') {
      event.preventDefault();
      onclose();
      return;
    }
    if (event.key !== 'Tab') return;

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

  function onScrimClick(event: MouseEvent): void {
    event.stopPropagation();
    if (event.target === scrimEl) onclose();
  }

  /**
   * Re-parent the whole panel to `<body>`.
   *
   * Not a nicety: a `CaptureCard` lifts itself with `transform: translateY(-1px)`
   * on hover, and a transformed ancestor becomes the containing block for every
   * `position: fixed` descendant. Left inside the card, this scrim would snap
   * from the viewport down to the size of a thumbnail the moment the pointer
   * crossed it. Hosting it on `<body>` makes the panel independent of whatever
   * opened it.
   */
  function portal(node: HTMLElement) {
    document.body.appendChild(node);
    return {
      destroy() {
        node.remove();
      }
    };
  }
</script>

<div class="scrim" bind:this={scrimEl} use:portal role="presentation" onclick={onScrimClick}>
  <div
    class="dialog"
    bind:this={dialogEl}
    role="dialog"
    aria-modal="true"
    aria-label="Text extracted from {record.name}"
    tabindex="-1"
    onkeydown={onKeydown}
  >
    <header class="head">
      <span class="head-icon"><Icon name="scan" size={18} /></span>
      <span class="titles">
        <h2 class="title">Extracted text</h2>
        <p class="sub" title={record.name}>{record.name}</p>
      </span>
      {#if status === 'done' && found}
        <span class="chip tnum">{lines.length} {lines.length === 1 ? 'line' : 'lines'}</span>
        {#if result?.language}
          <span class="chip" title="The OCR language pack that read this">{result.language}</span>
        {/if}
      {/if}
      <button type="button" class="btn btn-icon" aria-label="Close" onclick={onclose}>
        <Icon name="x" size={16} />
      </button>
    </header>

    <div class="body">
      {#if status === 'working'}
        <div class="state" aria-live="polite">
          <span class="sweep" aria-hidden="true"></span>
          <p class="state-title">Reading the text…</p>
          <p class="state-text">
            Windows is recognising this capture offline. Nothing leaves the machine.
          </p>
        </div>
      {:else if status === 'failed'}
        <div class="state" aria-live="polite">
          <span class="state-icon bad"><Icon name="alert" size={22} /></span>
          <p class="state-title">Could not read this capture</p>
          <p class="state-text">{failure}</p>
          <button type="button" class="btn" onclick={() => recognise(record.id)}>
            <Icon name="refresh" size={15} />
            Try again
          </button>
        </div>
      {:else if !found}
        <div class="state" aria-live="polite">
          <span class="state-icon"><Icon name="scan" size={22} /></span>
          <p class="state-title">No text found</p>
          <p class="state-text">
            The recognition ran and came back empty — there is nothing in this picture the engine
            reads as text. Very small, very low-contrast or heavily stylised lettering is the usual
            reason.
          </p>
          <button type="button" class="btn" onclick={() => recognise(record.id)}>
            <Icon name="refresh" size={15} />
            Run it again
          </button>
        </div>
      {:else}
        <div class="text scroll selectable">{body}</div>

        <div class="lines-head">
          <span class="section-title">Lines</span>
          <span class="hint">Reading order, as the engine segmented the page.</span>
        </div>
        <ul class="lines scroll">
          {#each lines as line, i (i)}
            <li class="line">
              <span class="line-no tnum" aria-hidden="true">{i + 1}</span>
              <span class="line-text selectable">{line.text}</span>
              <button
                type="button"
                class="line-copy"
                aria-label="Copy line {i + 1}"
                title="Copy this line"
                onclick={() => copyLine(line.text)}
              >
                <Icon name="copy" size={14} />
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    </div>

    <footer class="foot">
      <span class="foot-note">
        {#if status === 'done' && found}
          Recognised offline by Windows.
        {/if}
      </span>
      <div class="tools">
        <button type="button" class="btn btn-primary" disabled={!found} onclick={copyAll}>
          <Icon name={copied ? 'check' : 'copy'} size={15} />
          {copied ? 'Copied' : 'Copy text'}
        </button>
        <button type="button" class="btn" onclick={onclose}>Close</button>
      </div>
    </footer>
  </div>
</div>

<style>
  /* Above the lightbox (z 60): this panel is opened from inside it. */
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 80;
    display: grid;
    place-items: center;
    padding: 32px;
    background: color-mix(in srgb, var(--bg) 84%, transparent);
    backdrop-filter: var(--blur);
  }

  .dialog {
    display: flex;
    flex-direction: column;
    width: min(680px, 100%);
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
    align-items: center;
    gap: 10px;
    padding: 14px 14px 14px 16px;
    border-bottom: 1px solid var(--border);
  }

  .head-icon {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 32px;
    height: 32px;
    flex: none;
    border-radius: var(--radius);
    background: var(--bg-inset);
    color: var(--text-dim);
  }

  .titles {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    flex: 1;
  }

  .title {
    margin: 0;
    font-family: var(--font-display);
    font-size: 15px;
    font-weight: 600;
    color: var(--text);
  }

  .sub {
    margin: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 12px;
    color: var(--text-faint);
  }

  .body {
    display: flex;
    flex-direction: column;
    gap: 12px;
    min-height: 0;
    padding: 16px;
    background: var(--bg-inset);
  }

  /* Working, failed and empty all land here, so they are one shape: the panel
     never changes size just because the answer was "nothing". */
  .state {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    padding: 44px 24px;
    text-align: center;
  }

  .state-icon {
    display: flex;
    color: var(--text-faint);
  }

  .state-icon.bad {
    color: var(--danger);
  }

  .state-title {
    margin: 0;
    font-family: var(--font-display);
    font-size: 14px;
    font-weight: 600;
    color: var(--text);
  }

  .state-text {
    margin: 0 0 4px;
    max-width: 46ch;
    font-size: 12px;
    line-height: 1.6;
    color: var(--text-dim);
    text-wrap: pretty;
  }

  /* The only motion while the command is out. Reduced motion stops it and the
     sentence beneath carries the state on its own. */
  .sweep {
    position: relative;
    overflow: hidden;
    width: 120px;
    height: 2px;
    margin-bottom: 12px;
    border-radius: 999px;
    background: var(--border);
  }

  .sweep::after {
    content: '';
    position: absolute;
    inset: 0;
    border-radius: inherit;
    background: var(--accent);
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

  .text {
    max-height: 34vh;
    padding: 12px 14px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1.65;
    color: var(--text);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .lines-head {
    display: flex;
    align-items: baseline;
    gap: 8px;
    flex-wrap: wrap;
  }

  /* `.section-title` carries an 8px bottom margin globally; this row spaces
     itself. */
  .lines-head :global(.section-title) {
    margin-bottom: 0;
  }

  .hint {
    font-size: 12px;
    color: var(--text-faint);
  }

  .lines {
    display: flex;
    flex-direction: column;
    max-height: 26vh;
    margin: 0;
    padding: 4px;
    list-style: none;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
  }

  .line {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 6px 6px 6px 8px;
    border-radius: 6px;
  }

  .line + .line {
    box-shadow: inset 0 1px 0 var(--border);
  }

  .line:hover {
    background: var(--bg-inset);
  }

  .line-no {
    flex: none;
    min-width: 20px;
    padding-top: 1px;
    text-align: right;
    font-size: 11px;
    color: var(--text-faint);
  }

  .line-text {
    flex: 1;
    min-width: 0;
    font-size: 13px;
    line-height: 1.5;
    color: var(--text);
    overflow-wrap: anywhere;
  }

  .line-copy {
    display: flex;
    align-items: center;
    justify-content: center;
    flex: none;
    width: 24px;
    height: 24px;
    border-radius: 6px;
    border: 1px solid transparent;
    color: var(--text-faint);
    cursor: pointer;
    opacity: 0;
    transition:
      opacity 120ms ease,
      color 120ms ease,
      border-color 120ms ease;
  }

  .line:hover .line-copy,
  .line-copy:focus-visible {
    opacity: 1;
  }

  .line-copy:hover {
    color: var(--text);
    border-color: var(--border);
  }

  .foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 12px 14px;
    border-top: 1px solid var(--border);
  }

  .foot-note {
    min-width: 0;
    font-size: 12px;
    color: var(--text-faint);
  }

  .tools {
    display: flex;
    gap: 8px;
    flex: none;
  }
</style>
