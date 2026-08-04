<script lang="ts">
  import "../app.css";
  import { onMount } from "svelte";
  import { onCaptureError } from "$lib/api";
  import Sidebar from "$lib/components/Sidebar.svelte";
  import Toast, { toast } from "$lib/components/Toast.svelte";
  import CaptureView from "$lib/views/CaptureView.svelte";
  import HistoryView from "$lib/views/HistoryView.svelte";
  import SettingsView from "$lib/views/SettingsView.svelte";
  import RegionOverlay from "$lib/overlay/RegionOverlay.svelte";
  import EditorRoot from "$lib/editor/EditorRoot.svelte";
  import CapturePreview from "$lib/preview/CapturePreview.svelte";
  import ShareXShell from "$lib/shell/ShareXShell.svelte";
  import { settings } from "$lib/stores/settings.svelte";
  import { history } from "$lib/stores/history.svelte";
  import {
    LEGACY_THEME_STORAGE_KEY,
    THEME_STORAGE_KEY,
    isTheme,
    type Theme,
    type View
  } from "$lib/types";

  // Resolved during init, never in onMount: the overlay, editor and preview
  // windows must not paint a frame of the app shell before switching.
  // adapter-static gives all four windows the same document, so ?w= is the only
  // discriminator.
  const which = new URLSearchParams(window.location.search).get("w");
  const isOverlay = which === "overlay";
  const isEditor = which === "editor";
  const isPreview = which === "preview";

  // The preview window is one edge-to-edge card, so its document must not wear
  // the app shell's background; CapturePreview's own CSS is gated on this class.
  // app.html stamps the overlay's equivalent pre-paint because the overlay is
  // shown over the live desktop within the same frame it arms. This window is
  // shown only after Rust has something to put in it, so init is early enough.
  if (isPreview) document.documentElement.classList.add("preview-window");

  let view = $state<View>("capture");

  /**
   * `sharex` is the one theme that changes the shell's *layout*, not just its
   * palette (M2.7 §4), so the choice has to be right on the first frame. The
   * settings store is empty until its round trip lands and reports `dark` until
   * then, which would paint the default shell and then swap it. The pre-paint
   * cache in localStorage is the same value `app.html` used to pick the palette,
   * so reading it here keeps shell and palette agreeing from the first paint.
   *
   * The legacy key is read for the same reason `app.html` reads it: on the first
   * launch after the rename the new key may hold nothing yet. Read-only — the
   * inline script owns migrating the value forward.
   */
  function cachedTheme(): Theme | null {
    try {
      const stored =
        localStorage.getItem(THEME_STORAGE_KEY) ?? localStorage.getItem(LEGACY_THEME_STORAGE_KEY);
      return isTheme(stored) ? stored : null;
    } catch {
      return null;
    }
  }

  const bootTheme = isOverlay || isEditor || isPreview ? null : cachedTheme();
  const isShareX = $derived((settings.ready ? settings.theme : bootTheme) === "sharex");

  onMount(() => {
    if (isOverlay || isEditor || isPreview) return;

    void settings.load();
    void history.load();

    // Hotkey-triggered captures have no UI to report into, so the shell owns
    // the error channel.
    const stop = onCaptureError((message) => toast.error(message));
    return () => {
      void stop.then((unlisten) => unlisten());
    };
  });

  function navigate(next: View): void {
    view = next;
  }
</script>

{#if isOverlay}
  <RegionOverlay />
{:else if isEditor}
  <EditorRoot />
{:else if isPreview}
  <CapturePreview />
{:else if isShareX}
  <!-- The `sharex` theme swaps the whole main-window shell, ShareX's own
       arrangement rather than this one wearing ShareX's colours (M2.7 §4). -->
  <ShareXShell />
  <Toast />
{:else}
  <div class="shell">
    <Sidebar {view} onNavigate={navigate} />
    <main class="pane scroll">
      {#if view === "capture"}
        <CaptureView onNavigate={navigate} />
      {:else if view === "history"}
        <HistoryView />
      {:else}
        <SettingsView />
      {/if}
    </main>
  </div>
  <Toast />
{/if}

<style>
  .shell {
    display: flex;
    height: 100vh;
    overflow: hidden;
    background: var(--bg);
    color: var(--text);
  }

  /* The pane is the scroll container itself so each view can pin its own
     sticky page header to the top edge. Views own their padding. */
  .pane {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
    overflow-x: hidden;
  }
</style>
