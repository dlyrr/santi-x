<script lang="ts">
  import { onMount } from "svelte";
  import { getVersion } from "@tauri-apps/api/app";
  import Icon, { type IconName } from "./Icon.svelte";
  import type { View } from "$lib/types";

  let { view, onNavigate }: { view: View; onNavigate: (v: View) => void } = $props();

  const items: { id: View; label: string; icon: IconName }[] = [
    { id: "capture", label: "Capture", icon: "camera" },
    { id: "history", label: "History", icon: "clock" },
    { id: "workflows", label: "Workflows", icon: "workflow" },
    { id: "settings", label: "Settings", icon: "settings" }
  ];

  let version = $state("");

  onMount(() => {
    getVersion()
      .then((v) => (version = v))
      .catch(() => (version = ""));
  });
</script>

<aside class="sidebar">
  <div class="brand">
    <span class="mark"><Icon name="region" size={16} /></span>
    <span class="wordmark">santi.sharex</span>
  </div>

  <nav aria-label="Sections">
    {#each items as item (item.id)}
      <button
        type="button"
        class="nav"
        class:active={view === item.id}
        aria-current={view === item.id ? "page" : undefined}
        onclick={() => onNavigate(item.id)}
      >
        <Icon name={item.icon} size={16} />
        <span>{item.label}</span>
      </button>
    {/each}
  </nav>

  <div class="foot">
    <span class="ver">{version ? `v${version}` : ""}</span>
  </div>
</aside>

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    flex: none;
    width: 224px;
    height: 100%;
    padding: 16px 12px 12px;
    border-right: 1px solid var(--border);
    background: var(--bg-raised);
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 0 6px 18px;
  }

  .mark {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 26px;
    height: 26px;
    border-radius: 7px;
    color: var(--accent-text);
    background: var(--accent);
  }

  .wordmark {
    font-family: var(--font-display);
    font-size: 16px;
    font-weight: 600;
    letter-spacing: -0.01em;
    color: var(--text);
  }

  nav {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .nav {
    position: relative;
    display: flex;
    align-items: center;
    gap: 10px;
    height: 36px;
    padding: 0 10px;
    border: 0;
    border-radius: var(--radius);
    background: none;
    font: inherit;
    font-size: 13px;
    color: var(--text-dim);
    text-align: left;
    cursor: pointer;
    transition:
      background 120ms ease,
      color 120ms ease;
  }

  .nav:hover {
    background: var(--bg-inset);
    color: var(--text);
  }

  .nav.active {
    background: var(--surface);
    color: var(--text);
    font-weight: 500;
  }

  .nav.active::before {
    content: "";
    position: absolute;
    left: 0;
    top: 9px;
    bottom: 9px;
    width: 2px;
    border-radius: 2px;
    background: var(--accent);
  }

  .nav:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  .foot {
    margin-top: auto;
    padding: 10px 10px 2px;
    border-top: 1px solid var(--border);
  }

  .ver {
    font-size: 12px;
    font-variant-numeric: tabular-nums;
    color: var(--text-faint);
  }
</style>
