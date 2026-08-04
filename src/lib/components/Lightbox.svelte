<script lang="ts">
  import { ask } from '@tauri-apps/plugin-dialog';
  import {
    copyCapture,
    openCapture,
    openEditor,
    revealCapture,
    versionedAssetUrl,
    type CaptureRecord
  } from '$lib/api';
  import { history } from '$lib/stores/history.svelte';
  import { toast } from '$lib/components/Toast.svelte';

  interface Props {
    records: CaptureRecord[];
    index: number;
    onclose: () => void;
    onnavigate: (index: number) => void;
  }

  let { records, index, onclose, onnavigate }: Props = $props();

  let scrimEl: HTMLDivElement | undefined = $state();
  let dialogEl: HTMLDivElement | undefined = $state();
  let imgBroken = $state(false);

  const record = $derived(records[index] ?? records[records.length - 1] ?? null);
  const hasFile = $derived(!!record && record.saved && record.path !== '');
  // Versioned: an edit saved over its original keeps the same path, and an
  // unchanged `src` is never re-fetched (see `versionedAssetUrl`).
  const fullSrc = $derived(record ? versionedAssetUrl(record.path, record.sizeBytes) : '');

  $effect(() => {
    if (records.length === 0) onclose();
  });

  // Reset the broken-image flag whenever we move to a different capture — or
  // when the one on screen is rewritten by the editor.
  $effect(() => {
    void fullSrc;
    imgBroken = false;
  });

  $effect(() => {
    const previous = document.activeElement as HTMLElement | null;
    dialogEl?.focus();
    return () => previous?.focus?.();
  });

  function formatBytes(n: number): string {
    if (n < 1024) return `${n} B`;
    const units = ['KB', 'MB', 'GB'];
    let value = n / 1024;
    let i = 0;
    while (value >= 1024 && i < units.length - 1) {
      value /= 1024;
      i++;
    }
    return `${value.toFixed(value < 10 ? 1 : 0)} ${units[i]}`;
  }

  function go(delta: number) {
    if (records.length < 2) return;
    const next = (index + delta + records.length) % records.length;
    onnavigate(next);
  }

  function focusable(): HTMLElement[] {
    if (!dialogEl) return [];
    const nodes = dialogEl.querySelectorAll<HTMLElement>(
      'button:not([disabled]), [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
    );
    return Array.from(nodes).filter((el) => el.offsetParent !== null);
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      event.preventDefault();
      onclose();
      return;
    }
    if (event.key === 'ArrowLeft') {
      event.preventDefault();
      go(-1);
      return;
    }
    if (event.key === 'ArrowRight') {
      event.preventDefault();
      go(1);
      return;
    }
    if (event.key !== 'Tab') return;

    // Focus trap: the dialog itself is tabindex="-1", so without this Tab would
    // walk out into the (inert but still focusable) app behind the scrim.
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

  function onScrimPointer(event: MouseEvent) {
    if (event.target === scrimEl) onclose();
  }

  async function doCopy() {
    if (!record) return;
    try {
      await copyCapture(record.id);
      toast.success('Copied to clipboard');
    } catch (err) {
      toast.error(String(err));
    }
  }

  /** The editor is its own window, so the preview steps aside for it. */
  async function doEdit() {
    if (!record) return;
    try {
      await openEditor(record.id);
      onclose();
    } catch (err) {
      toast.error(String(err));
    }
  }

  async function doOpen() {
    if (!record) return;
    try {
      await openCapture(record.id);
    } catch (err) {
      toast.error(String(err));
    }
  }

  async function doReveal() {
    if (!record) return;
    try {
      await revealCapture(record.id);
    } catch (err) {
      toast.error(String(err));
    }
  }

  async function doDelete() {
    if (!record) return;
    const target = record;
    const removeFile = hasFile;
    const message = removeFile
      ? `Delete "${target.name}"? The file will be removed from disk.`
      : `Remove "${target.name}" from history?`;
    const confirmed = await ask(message, { title: 'Delete capture', kind: 'warning' });
    if (!confirmed) return;
    // The store reports failures through `error` rather than rejecting.
    const ok = await history.remove(target.id, removeFile);
    if (!ok) {
      toast.error(history.error ?? 'Could not delete the capture');
      return;
    }
    toast.success(removeFile ? 'Capture deleted' : 'Removed from history');
    if (records.length === 0) onclose();
    else if (index > records.length - 1) onnavigate(records.length - 1);
  }
</script>

{#if record}
  <div
    class="scrim"
    bind:this={scrimEl}
    role="presentation"
    onclick={onScrimPointer}
  >
    <div
      class="dialog"
      bind:this={dialogEl}
      role="dialog"
      aria-modal="true"
      aria-label={record.name}
      tabindex="-1"
      onkeydown={onKeydown}
    >
      <header class="head">
        <div class="titles">
          <h2 class="name" title={record.name}>{record.name}</h2>
          <p class="meta">
            <span class="num">{record.width}&times;{record.height}</span>
            <span class="sep" aria-hidden="true">&middot;</span>
            <span class="kind">{record.kind}</span>
            <span class="sep" aria-hidden="true">&middot;</span>
            <span class="num">{formatBytes(record.sizeBytes)}</span>
            <span class="sep" aria-hidden="true">&middot;</span>
            <span class="num">{new Date(record.createdAt).toLocaleString()}</span>
          </p>
        </div>
        <button type="button" class="btn btn-icon" aria-label="Close preview" onclick={onclose}>
          <svg
            viewBox="0 0 16 16"
            width="16"
            height="16"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            aria-hidden="true"
          >
            <path d="m4 4 8 8M12 4l-8 8" />
          </svg>
        </button>
      </header>

      <div class="stage">
        {#if records.length > 1}
          <button type="button" class="nav prev" aria-label="Previous capture" onclick={() => go(-1)}>
            <svg
              viewBox="0 0 16 16"
              width="18"
              height="18"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <path d="M10 3 5 8l5 5" />
            </svg>
          </button>
        {/if}

        {#if !hasFile}
          <div class="explain">
            <svg
              viewBox="0 0 24 24"
              width="26"
              height="26"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <rect x="3" y="4.5" width="18" height="15" rx="2.5" />
              <path d="M3 3l18 18" />
            </svg>
            <h3>Nothing on disk to preview</h3>
            <p>
              This capture was taken with <strong>Save to disk</strong> turned off, so only the
              thumbnail and its details were kept. Turn the setting on in Settings &rsaquo; Capture
              to keep future files.
            </p>
          </div>
        {:else if imgBroken}
          <div class="explain">
            <h3>File is missing</h3>
            <p class="path">{record.path}</p>
            <p>The image was moved or deleted outside santi.sharex.</p>
          </div>
        {:else}
          <img
            class="full"
            src={fullSrc}
            alt={record.name}
            onerror={() => (imgBroken = true)}
          />
        {/if}

        {#if records.length > 1}
          <button type="button" class="nav next" aria-label="Next capture" onclick={() => go(1)}>
            <svg
              viewBox="0 0 16 16"
              width="18"
              height="18"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <path d="m6 3 5 5-5 5" />
            </svg>
          </button>
        {/if}
      </div>

      <footer class="foot">
        <span class="counter num">{index + 1} of {records.length}</span>
        <div class="tools">
          <button type="button" class="btn" disabled={!hasFile} onclick={doEdit}>Edit</button>
          <button type="button" class="btn" disabled={!hasFile} onclick={doCopy}>Copy</button>
          <button type="button" class="btn" disabled={!hasFile} onclick={doOpen}>Open</button>
          <button type="button" class="btn" disabled={!hasFile} onclick={doReveal}>
            Show in folder
          </button>
          <button type="button" class="btn btn-danger" onclick={doDelete}>Delete</button>
        </div>
      </footer>
    </div>
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 60;
    display: grid;
    place-items: center;
    padding: 32px;
    background: color-mix(in srgb, var(--bg) 84%, transparent);
    backdrop-filter: var(--blur);
  }

  .dialog {
    display: flex;
    flex-direction: column;
    width: min(1080px, 100%);
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
    align-items: flex-start;
    gap: 16px;
    padding: 16px 16px 12px 20px;
    border-bottom: 1px solid var(--border);
  }

  .titles {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .name {
    margin: 0;
    font-family: var(--font-display);
    font-size: 15px;
    font-weight: 600;
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .meta {
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    color: var(--text-faint);
  }

  .kind {
    text-transform: capitalize;
  }

  .sep {
    opacity: 0.6;
  }

  .num {
    font-variant-numeric: tabular-nums;
  }

  .stage {
    position: relative;
    flex: 1;
    min-height: 0;
    display: grid;
    place-items: center;
    padding: 20px;
    background: var(--bg-inset);
  }

  .full {
    max-width: 100%;
    max-height: min(64vh, 720px);
    object-fit: contain;
    border-radius: var(--radius);
    background: var(--bg-inset);
  }

  .explain {
    max-width: 380px;
    padding: 40px 8px;
    text-align: center;
    color: var(--text-dim);
  }

  .explain h3 {
    margin: 12px 0 6px;
    font-family: var(--font-display);
    font-size: 15px;
    font-weight: 600;
    color: var(--text);
  }

  .explain p {
    margin: 0;
    font-size: 12px;
    line-height: 1.6;
  }

  .explain .path {
    margin-bottom: 8px;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-faint);
    word-break: break-all;
  }

  .nav {
    position: absolute;
    top: 50%;
    transform: translateY(-50%);
    width: 34px;
    height: 34px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: 1px solid var(--border);
    border-radius: 50%;
    background: color-mix(in srgb, var(--bg-raised) 90%, transparent);
    color: var(--text-dim);
    cursor: pointer;
    transition:
      color 140ms ease,
      border-color 140ms ease,
      background-color 140ms ease;
  }

  .nav:hover {
    color: var(--text);
    border-color: var(--border-strong);
    background: var(--bg-raised);
  }

  .nav:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .prev {
    left: 12px;
  }

  .next {
    right: 12px;
  }

  .foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 12px 16px;
    border-top: 1px solid var(--border);
  }

  .counter {
    font-size: 12px;
    color: var(--text-faint);
  }

  .tools {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }

  .btn {
    height: 34px;
    padding: 0 12px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    font: inherit;
    font-size: 13px;
    color: var(--text);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    cursor: pointer;
    transition:
      color 140ms ease,
      background-color 140ms ease,
      border-color 140ms ease;
  }

  .btn:hover:not(:disabled) {
    border-color: var(--border-strong);
    background: var(--bg-inset);
  }

  .btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .btn:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }

  .btn-icon {
    width: 34px;
    padding: 0;
    flex: none;
    color: var(--text-dim);
  }

  .btn-danger:hover:not(:disabled) {
    color: var(--danger);
    border-color: var(--danger);
  }
</style>
