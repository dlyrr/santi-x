<script lang="ts">
  import CaptureCard from '$lib/components/CaptureCard.svelte';
  import Lightbox from '$lib/components/Lightbox.svelte';
  import { history } from '$lib/stores/history.svelte';
  import { toast } from '$lib/components/Toast.svelte';
  import type { CaptureKind, CaptureRecord } from '$lib/api';

  /** Mirrors `CaptureKind`; `edit` is what "Save as new" writes (M2 §3). */
  type KindFilter = 'all' | CaptureKind;

  const KINDS: { value: KindFilter; label: string }[] = [
    { value: 'all', label: 'All kinds' },
    { value: 'region', label: 'Region' },
    { value: 'fullscreen', label: 'Fullscreen' },
    { value: 'window', label: 'Window' },
    { value: 'monitor', label: 'Monitor' },
    { value: 'edit', label: 'Edited' }
  ];

  let query = $state('');
  let kind = $state<KindFilter>('all');
  let lightboxIndex = $state(-1);
  let confirmingClear = $state(false);
  let clearFiles = $state(true);
  let clearing = $state(false);
  let confirmEl: HTMLDivElement | undefined = $state();

  const records = $derived(history.items);

  const filtered = $derived.by(() => {
    const q = query.trim().toLowerCase();
    return records.filter((r) => {
      if (kind !== 'all' && r.kind !== kind) return false;
      if (q === '') return true;
      return r.name.toLowerCase().includes(q) || r.kind.toLowerCase().includes(q);
    });
  });

  const totalBytes = $derived(filtered.reduce((sum, r) => sum + r.sizeBytes, 0));
  const isFiltering = $derived(query.trim() !== '' || kind !== 'all');

  $effect(() => {
    if (confirmingClear) confirmEl?.focus();
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

  function openLightbox(record: CaptureRecord) {
    lightboxIndex = filtered.findIndex((r) => r.id === record.id);
  }

  function resetFilters() {
    query = '';
    kind = 'all';
  }

  function onWindowKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape' && confirmingClear) {
      confirmingClear = false;
    }
  }

  async function doClear() {
    clearing = true;
    try {
      // The store swallows the rejection and reports through `error`, so the
      // return value — not a catch — is what tells us the write landed.
      const ok = await history.clear(clearFiles);
      if (!ok) {
        toast.error(history.error ?? 'Could not clear the history');
        return;
      }
      confirmingClear = false;
      lightboxIndex = -1;
      toast.success(clearFiles ? 'History and files deleted' : 'History cleared');
    } finally {
      clearing = false;
    }
  }
</script>

<svelte:window onkeydown={onWindowKeydown} />

<div class="page">
  <header class="page-header">
    <div class="heading">
      <h1 class="page-title">History</h1>
      <p class="stats">
        <span class="num">{filtered.length}</span>
        {filtered.length === 1 ? 'capture' : 'captures'}
        <span class="sep" aria-hidden="true">&middot;</span>
        <span class="num">{formatBytes(totalBytes)}</span>
      </p>
    </div>

    <div class="tools">
      <div class="search">
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
          <circle cx="7.25" cy="7.25" r="4.25" />
          <path d="m10.5 10.5 3 3" />
        </svg>
        <input
          class="search-input"
          type="search"
          placeholder="Search captures"
          aria-label="Search captures by name or kind"
          bind:value={query}
        />
      </div>

      <select class="kind-select" aria-label="Filter by capture kind" bind:value={kind}>
        {#each KINDS as option (option.value)}
          <option value={option.value}>{option.label}</option>
        {/each}
      </select>

      <button
        type="button"
        class="btn btn-danger"
        disabled={records.length === 0}
        aria-expanded={confirmingClear}
        onclick={() => (confirmingClear = !confirmingClear)}
      >
        Clear history
      </button>
    </div>
  </header>

  {#if confirmingClear}
    <div
      class="confirm"
      bind:this={confirmEl}
      role="group"
      aria-label="Confirm clearing history"
      tabindex="-1"
    >
      <p class="confirm-text">
        Remove all <span class="num">{records.length}</span>
        {records.length === 1 ? 'capture' : 'captures'} from history?
      </p>
      <label class="check">
        <input type="checkbox" bind:checked={clearFiles} />
        <span>Also delete the image files from disk</span>
      </label>
      <div class="confirm-actions">
        <button type="button" class="btn" onclick={() => (confirmingClear = false)}>Cancel</button>
        <button type="button" class="btn btn-danger" disabled={clearing} onclick={doClear}>
          {clearFiles ? 'Delete everything' : 'Clear history'}
        </button>
      </div>
    </div>
  {/if}

  {#if records.length === 0}
    <div class="empty">
      <svg
        viewBox="0 0 24 24"
        width="28"
        height="28"
        fill="none"
        stroke="currentColor"
        stroke-width="1.5"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <path d="M3 8.5A2.5 2.5 0 0 1 5.5 6h1.7l1.1-1.8A1.5 1.5 0 0 1 9.6 3.5h4.8a1.5 1.5 0 0 1 1.3.7L16.8 6h1.7A2.5 2.5 0 0 1 21 8.5v8A2.5 2.5 0 0 1 18.5 19h-13A2.5 2.5 0 0 1 3 16.5z" />
        <circle cx="12" cy="12" r="3.5" />
      </svg>
      <h2>No captures yet</h2>
      <p>
        Take a shot from the Capture tab or press your region hotkey. Everything you grab lands
        here, newest first.
      </p>
    </div>
  {:else if filtered.length === 0}
    <div class="empty">
      <h2>No matches</h2>
      <p>Nothing in your history matches the current search or kind filter.</p>
      <button type="button" class="btn" onclick={resetFilters}>Reset filters</button>
    </div>
  {:else}
    <div class="grid">
      {#each filtered as record (record.id)}
        <CaptureCard
          {record}
          onopen={openLightbox}
          ondeleted={() => (lightboxIndex = -1)}
        />
      {/each}
    </div>
  {/if}
</div>

{#if lightboxIndex >= 0 && filtered.length > 0}
  <Lightbox
    records={filtered}
    index={Math.min(lightboxIndex, filtered.length - 1)}
    onclose={() => (lightboxIndex = -1)}
    onnavigate={(i) => (lightboxIndex = i)}
  />
{/if}

<style>
  /* The shell pane has no padding of its own — every view owns its inset. */
  .page {
    display: flex;
    flex-direction: column;
    gap: 16px;
    padding: 0 32px 40px;
  }

  /* Bled back out to the pane edges so the pinned header and its hairline span
     the full width, the way the capture view's header does. */
  .page-header {
    position: sticky;
    top: 0;
    z-index: 4;
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: 24px;
    flex-wrap: wrap;
    margin: 0 -32px 8px;
    padding: 24px 32px 16px;
    background: var(--bg);
    border-bottom: 1px solid var(--border);
  }

  .heading {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .page-title {
    margin: 0;
    font-family: var(--font-display);
    font-size: 20px;
    font-weight: 600;
    line-height: 1.2;
    color: var(--text);
  }

  .stats {
    margin: 0;
    font-size: 12px;
    color: var(--text-faint);
  }

  .num {
    font-variant-numeric: tabular-nums;
  }

  .sep {
    opacity: 0.6;
    margin: 0 2px;
  }

  .tools {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }

  .search {
    position: relative;
    display: flex;
    align-items: center;
  }

  .search svg {
    position: absolute;
    left: 10px;
    color: var(--text-faint);
    pointer-events: none;
  }

  .search-input {
    height: 34px;
    width: 220px;
    padding: 0 10px 0 32px;
    font: inherit;
    font-size: 13px;
    color: var(--text);
    background: var(--bg-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    transition:
      border-color 140ms ease,
      background-color 140ms ease;
  }

  .search-input::placeholder {
    color: var(--text-faint);
  }

  .search-input:hover {
    border-color: var(--border-strong);
  }

  .search-input:focus-visible,
  .kind-select:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
    border-color: var(--accent);
  }

  .kind-select {
    height: 34px;
    padding: 0 8px;
    font: inherit;
    font-size: 13px;
    color: var(--text);
    background: var(--bg-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    cursor: pointer;
    transition: border-color 140ms ease;
  }

  .kind-select:hover {
    border-color: var(--border-strong);
  }

  .btn {
    height: 34px;
    padding: 0 12px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
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

  .btn-danger:hover:not(:disabled) {
    color: var(--danger);
    border-color: var(--danger);
  }

  .confirm {
    display: flex;
    align-items: center;
    gap: 16px;
    flex-wrap: wrap;
    padding: 12px 16px;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-lg);
  }

  .confirm:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .confirm-text {
    margin: 0;
    font-size: 13px;
    color: var(--text);
  }

  .check {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    color: var(--text-dim);
    cursor: pointer;
  }

  .check input {
    width: 15px;
    height: 15px;
    accent-color: var(--accent);
    cursor: pointer;
  }

  .check input:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .confirm-actions {
    display: flex;
    gap: 8px;
    margin-left: auto;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: 16px;
  }

  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    text-align: center;
    padding: 72px 24px;
    color: var(--text-faint);
    border: 1px dashed var(--border);
    border-radius: var(--radius-lg);
  }

  .empty h2 {
    margin: 4px 0 0;
    font-family: var(--font-display);
    font-size: 15px;
    font-weight: 600;
    color: var(--text);
  }

  .empty p {
    margin: 0;
    max-width: 380px;
    font-size: 12px;
    line-height: 1.6;
    color: var(--text-dim);
  }

  .empty .btn {
    margin-top: 8px;
  }
</style>
