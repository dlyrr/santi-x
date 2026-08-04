<script module lang="ts">
  import { fly } from "svelte/transition";
  import Icon, { type IconName } from "./Icon.svelte";

  export type ToastVariant = "info" | "success" | "error";

  type ToastItem = {
    id: number;
    message: string;
    variant: ToastVariant;
    timer: ReturnType<typeof setTimeout> | null;
  };

  const MAX_VISIBLE = 4;
  const TTL: Record<ToastVariant, number> = { info: 3000, success: 3000, error: 5000 };

  // Module-level rune state: the host is a singleton, so any view can call
  // `toast.error(...)` without threading a context or a store library through.
  let items = $state<ToastItem[]>([]);
  let seq = 0;

  export function dismissToast(id: number): void {
    const i = items.findIndex((t) => t.id === id);
    if (i === -1) return;
    const timer = items[i].timer;
    if (timer) clearTimeout(timer);
    items.splice(i, 1);
  }

  function push(message: string, variant: ToastVariant): number {
    const id = ++seq;
    const item: ToastItem = { id, message, variant, timer: null };
    items.push(item);
    while (items.length > MAX_VISIBLE) dismissToast(items[0].id);
    item.timer = setTimeout(() => dismissToast(id), TTL[variant]);
    return id;
  }

  export const toast = {
    info: (message: string) => push(message, "info"),
    success: (message: string) => push(message, "success"),
    error: (message: string) => push(message, "error"),
    dismiss: dismissToast,
    clear: () => {
      for (const t of items) if (t.timer) clearTimeout(t.timer);
      items = [];
    }
  };

  const GLYPH: Record<ToastVariant, IconName> = {
    info: "info",
    success: "check",
    error: "alert"
  };
</script>

<div class="host" role="status" aria-live="polite">
  {#each items as t (t.id)}
    <div class="toast {t.variant}" transition:fly={{ y: 8, duration: 140 }}>
      <span class="glyph"><Icon name={GLYPH[t.variant]} size={15} /></span>
      <span class="msg">{t.message}</span>
      <button class="close" aria-label="Dismiss" onclick={() => dismissToast(t.id)}>
        <Icon name="x" size={13} />
      </button>
    </div>
  {/each}
</div>

<style>
  .host {
    position: fixed;
    right: 16px;
    bottom: 16px;
    z-index: 200;
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 8px;
    pointer-events: none;
  }

  .toast {
    pointer-events: auto;
    display: flex;
    align-items: flex-start;
    gap: 10px;
    max-width: 360px;
    padding: 10px 10px 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    box-shadow: var(--shadow);
    font-size: 13px;
    line-height: 1.45;
    color: var(--text);
  }

  .glyph {
    display: flex;
    padding-top: 1px;
    color: var(--text-dim);
  }

  .toast.success .glyph {
    color: var(--success);
  }
  .toast.error .glyph {
    color: var(--danger);
  }
  .toast.error {
    border-color: color-mix(in srgb, var(--danger) 45%, var(--border));
  }

  .msg {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .close {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    margin: -1px -2px 0 0;
    padding: 0;
    border: 0;
    border-radius: 5px;
    background: none;
    color: var(--text-faint);
    cursor: pointer;
    transition:
      background 120ms ease,
      color 120ms ease;
  }

  .close:hover {
    background: var(--bg-inset);
    color: var(--text);
  }

  .close:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
</style>
