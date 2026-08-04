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
  import OcrPanel from '$lib/components/OcrPanel.svelte';
  import { toast } from '$lib/components/Toast.svelte';

  interface Props {
    record: CaptureRecord;
    onopen: (record: CaptureRecord) => void;
    ondeleted?: (id: string) => void;
  }

  let { record, onopen, ondeleted }: Props = $props();

  let thumbBroken = $state(false);
  let ocrOpen = $state(false);

  const hasFile = $derived(record.saved && record.path !== '');
  // Versioned: an edit saved over its original keeps the same thumbnail path,
  // and an unchanged `src` is never re-fetched (see `versionedAssetUrl`).
  const thumbSrc = $derived(
    record.thumb ? versionedAssetUrl(record.thumb, record.sizeBytes) : ''
  );

  // A rewritten capture is a new picture in the same slot, so the broken-image
  // flag must not survive it — and neither may text extracted from the picture
  // it replaced.
  $effect(() => {
    void thumbSrc;
    thumbBroken = false;
    ocrOpen = false;
  });

  function relativeTime(ms: number): string {
    const diff = Date.now() - ms;
    if (diff < 45_000) return 'just now';
    const mins = Math.round(diff / 60_000);
    if (mins < 60) return `${mins}m ago`;
    const hours = Math.round(mins / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.round(hours / 24);
    if (days < 7) return `${days}d ago`;
    return new Date(ms).toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
  }

  function open() {
    onopen(record);
  }

  function onCardKey(event: KeyboardEvent) {
    // Enter/Space on an action button bubbles up here too — only act on the card itself.
    if (event.target !== event.currentTarget) return;
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      open();
    }
  }

  async function doCopy(event: MouseEvent) {
    event.stopPropagation();
    try {
      await copyCapture(record.id);
      toast.success('Copied to clipboard');
    } catch (err) {
      toast.error(String(err));
    }
  }

  async function doEdit(event: MouseEvent) {
    event.stopPropagation();
    try {
      await openEditor(record.id);
    } catch (err) {
      toast.error(String(err));
    }
  }

  async function doOpen(event: MouseEvent) {
    event.stopPropagation();
    try {
      await openCapture(record.id);
    } catch (err) {
      toast.error(String(err));
    }
  }

  async function doReveal(event: MouseEvent) {
    event.stopPropagation();
    try {
      await revealCapture(record.id);
    } catch (err) {
      toast.error(String(err));
    }
  }

  // No round trip of its own: the panel runs `ocr_capture` itself, so it can own
  // the working state instead of the card holding a spinner it cannot explain.
  function doExtractText(event: MouseEvent) {
    event.stopPropagation();
    ocrOpen = true;
  }

  async function doDelete(event: MouseEvent) {
    event.stopPropagation();
    const message = hasFile
      ? `Delete "${record.name}"? The file will be removed from disk.`
      : `Remove "${record.name}" from history?`;
    const confirmed = await ask(message, { title: 'Delete capture', kind: 'warning' });
    if (!confirmed) return;
    // The store reports failures through `error` rather than rejecting.
    const ok = await history.remove(record.id, hasFile);
    if (!ok) {
      toast.error(history.error ?? 'Could not delete the capture');
      return;
    }
    ondeleted?.(record.id);
    toast.success(hasFile ? 'Capture deleted' : 'Removed from history');
  }
</script>

<div
  class="card"
  role="button"
  tabindex="0"
  aria-label="Open {record.name}"
  onclick={open}
  onkeydown={onCardKey}
>
  <div class="thumb">
    {#if thumbSrc && !thumbBroken}
      <img src={thumbSrc} alt="" loading="lazy" onerror={() => (thumbBroken = true)} />
    {:else}
      <div class="fallback" aria-hidden="true">
        <svg
          viewBox="0 0 24 24"
          width="22"
          height="22"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
        >
          <rect x="3" y="4.5" width="18" height="15" rx="2.5" />
          <path d="m4 17 5-5 4 4 2.5-2.5L20 17" />
          <path d="M3 3l18 18" />
        </svg>
        <span>No preview</span>
      </div>
    {/if}

    <div class="actions">
      <button
        type="button"
        class="act"
        title="Edit"
        aria-label="Edit {record.name}"
        disabled={!hasFile}
        onclick={doEdit}
      >
        <svg
          viewBox="0 0 16 16"
          width="16"
          height="16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M11.4 2.6a1.25 1.25 0 0 1 1.77 0l.23.23a1.25 1.25 0 0 1 0 1.77L6.1 12l-2.85.75L4 9.9z" />
          <path d="m10.5 3.5 2 2" />
        </svg>
      </button>

      <button
        type="button"
        class="act"
        title="Copy to clipboard"
        aria-label="Copy {record.name} to clipboard"
        disabled={!hasFile}
        onclick={doCopy}
      >
        <svg
          viewBox="0 0 16 16"
          width="16"
          height="16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <rect x="5.75" y="5.75" width="8.5" height="8.5" rx="2" />
          <path d="M10.25 3.75v-.5a1.5 1.5 0 0 0-1.5-1.5h-5a1.5 1.5 0 0 0-1.5 1.5v5a1.5 1.5 0 0 0 1.5 1.5h.5" />
        </svg>
      </button>

      <button
        type="button"
        class="act"
        title="Open in default viewer"
        aria-label="Open {record.name}"
        disabled={!hasFile}
        onclick={doOpen}
      >
        <svg
          viewBox="0 0 16 16"
          width="16"
          height="16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M9.75 2.25h4v4" />
          <path d="M13.75 2.25 7.5 8.5" />
          <path d="M12.75 9.5v2.75a1.5 1.5 0 0 1-1.5 1.5h-7.5a1.5 1.5 0 0 1-1.5-1.5v-7.5a1.5 1.5 0 0 1 1.5-1.5H6.5" />
        </svg>
      </button>

      <button
        type="button"
        class="act"
        title="Show in folder"
        aria-label="Show {record.name} in folder"
        disabled={!hasFile}
        onclick={doReveal}
      >
        <svg
          viewBox="0 0 16 16"
          width="16"
          height="16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M2 5.25A1.75 1.75 0 0 1 3.75 3.5h2.4c.47 0 .92.19 1.24.53l.86.89h4A1.75 1.75 0 0 1 14 6.67v5.58A1.75 1.75 0 0 1 12.25 14h-8.5A1.75 1.75 0 0 1 2 12.25z" />
        </svg>
      </button>

      <!-- OCR reads the PNG off disk, so this is dead without one, exactly like
           the four actions above it. -->
      <button
        type="button"
        class="act"
        title="Extract text"
        aria-label="Extract text from {record.name}"
        disabled={!hasFile}
        onclick={doExtractText}
      >
        <svg
          viewBox="0 0 16 16"
          width="16"
          height="16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M2.5 5.75v-2.25a1 1 0 0 1 1-1h2.25" />
          <path d="M10.25 2.5h2.25a1 1 0 0 1 1 1v2.25" />
          <path d="M13.5 10.25v2.25a1 1 0 0 1-1 1h-2.25" />
          <path d="M5.75 13.5H3.5a1 1 0 0 1-1-1v-2.25" />
          <path d="M5.25 6.5h5.5" />
          <path d="M5.25 9.5h3.5" />
        </svg>
      </button>

      <button
        type="button"
        class="act danger"
        title="Delete"
        aria-label="Delete {record.name}"
        onclick={doDelete}
      >
        <svg
          viewBox="0 0 16 16"
          width="16"
          height="16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M2.75 4.5h10.5" />
          <path d="M6.25 4.5V3.4A1.4 1.4 0 0 1 7.65 2h.7a1.4 1.4 0 0 1 1.4 1.4v1.1" />
          <path d="m4.4 4.5.58 8.1A1.5 1.5 0 0 0 6.48 14h3.04a1.5 1.5 0 0 0 1.5-1.4l.58-8.1" />
        </svg>
      </button>
    </div>

    {#if !record.saved}
      <span class="badge" title="This capture was not written to disk">Not saved</span>
    {/if}
  </div>

  <div class="foot">
    <span class="name" title={record.name}>{record.name}</span>
    <span class="meta">
      <span class="num">{record.width}&times;{record.height}</span>
      <span class="dot" aria-hidden="true">&middot;</span>
      <span>{relativeTime(record.createdAt)}</span>
    </span>
  </div>
</div>

<!-- Portals itself out to <body>, so the card's hover lift cannot become the
     containing block for its fixed scrim. -->
{#if ocrOpen && hasFile}
  <OcrPanel {record} onclose={() => (ocrOpen = false)} />
{/if}

<style>
  .card {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--surface);
    overflow: hidden;
    cursor: pointer;
    text-align: left;
    transition:
      border-color 140ms ease,
      background-color 140ms ease,
      transform 140ms ease;
  }

  .card:hover {
    border-color: var(--border-strong);
    transform: translateY(-1px);
  }

  .card:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .thumb {
    position: relative;
    aspect-ratio: 16 / 10;
    background: var(--bg-inset);
    border-bottom: 1px solid var(--border);
    overflow: hidden;
  }

  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }

  .fallback {
    width: 100%;
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 6px;
    color: var(--text-faint);
    font-size: 12px;
  }

  .badge {
    position: absolute;
    left: 8px;
    top: 8px;
    padding: 2px 7px;
    border-radius: 999px;
    font-size: 11px;
    line-height: 16px;
    color: var(--text-dim);
    background: color-mix(in srgb, var(--bg) 80%, transparent);
    border: 1px solid var(--border);
    backdrop-filter: var(--blur);
  }

  .actions {
    position: absolute;
    inset: auto 0 0 0;
    display: flex;
    gap: 4px;
    padding: 8px;
    justify-content: flex-end;
    background: linear-gradient(
      to top,
      color-mix(in srgb, var(--bg) 88%, transparent),
      transparent
    );
    opacity: 0;
    transition: opacity 140ms ease;
  }

  .card:hover .actions,
  .card:focus-within .actions {
    opacity: 1;
  }

  .act {
    width: 28px;
    height: 28px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: color-mix(in srgb, var(--bg-raised) 92%, transparent);
    color: var(--text-dim);
    cursor: pointer;
    transition:
      color 140ms ease,
      background-color 140ms ease,
      border-color 140ms ease;
  }

  .act:hover:not(:disabled) {
    color: var(--text);
    border-color: var(--border-strong);
    background: var(--bg-raised);
  }

  .act.danger:hover:not(:disabled) {
    color: var(--danger);
    border-color: var(--danger);
  }

  .act:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
    opacity: 1;
  }

  .act:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .foot {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 10px 12px 12px;
    min-width: 0;
  }

  .name {
    font-size: 13px;
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .meta {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    color: var(--text-faint);
  }

  .num {
    font-variant-numeric: tabular-nums;
  }

  .dot {
    opacity: 0.6;
  }
</style>
