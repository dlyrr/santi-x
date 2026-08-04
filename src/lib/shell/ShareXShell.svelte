<script lang="ts">
  /**
   * The `sharex` theme's main-window shell (M2.7 §4).
   *
   * M2.6 gave the theme ShareX's palette; this is the rest of it — ShareX 21.0's
   * actual arrangement. A narrow menu column of dense icon+label rows on
   * `--bg-raised`, one hairline, and content sitting straight on `--bg` with no
   * card, no rounded panel and no shadow. That flatness is most of the look, so
   * nothing in here may grow a container.
   *
   * Scope: the `main` window only. The overlay, the editor and the other three
   * themes keep their own layout — `+page.svelte` picks between the shells.
   *
   * Two rules this file exists to keep:
   *   1. Every colour is a token. The theme does the colouring, not this file.
   *   2. Nothing is faked. Rows santi.sharex cannot honour yet are visibly disabled
   *      and name the milestone they land in; everything else routes into the
   *      same view components the default shell uses.
   */
  import { onMount } from "svelte";
  import { getVersion } from "@tauri-apps/api/app";
  import Icon, { type IconName } from "$lib/components/Icon.svelte";
  import Toggle from "$lib/components/Toggle.svelte";
  import { toast } from "$lib/components/Toast.svelte";
  import CaptureView from "$lib/views/CaptureView.svelte";
  import HistoryView from "$lib/views/HistoryView.svelte";
  import SettingsView from "$lib/views/SettingsView.svelte";
  import { settings } from "$lib/stores/settings.svelte";
  import { history } from "$lib/stores/history.svelte";
  import {
    errorMessage,
    getHotkeyStatus,
    onHotkeyStatus,
    openEditor,
    openSaveDir,
    type HotkeyMechanism,
    type HotkeyStatus,
    type Hotkeys,
    type Settings,
    type View
  } from "$lib/api";

  /**
   * The `View` members route into the shared views; the other three are pages
   * that exist only in this shell, because only ShareX's menu asks for them.
   */
  type Pane = View | "hotkeys" | "tools" | "tasks";

  /** Icon tint. Tokens only — ShareX's menu is coloured, but not by literals. */
  type Tone = "accent" | "success" | "dim";

  type MenuRow =
    | { kind: "pane"; label: string; icon: IconName; tone: Tone; pane: Pane }
    | { kind: "action"; label: string; icon: IconName; tone: Tone; run: () => void }
    | { kind: "soon"; label: string; icon: IconName; milestone: string; note: string };

  const HOTKEY_ROWS: { action: keyof Hotkeys; description: string }[] = [
    { action: "region", description: "Capture region" },
    { action: "fullscreen", description: "Capture entire screen" },
    { action: "activeWindow", description: "Capture active window" }
  ];

  const MECHANISM_TITLE: Record<HotkeyMechanism, string> = {
    plugin: "Bound. Registered with Windows the ordinary way (RegisterHotKey).",
    hook: "Bound. Another app owns this combination, so the keyboard hook claims it.",
    none: "Not bound — nothing happens when you press it."
  };

  /** The `Settings` booleans that describe what happens after a capture. */
  type TaskKey =
    | "saveToDisk"
    | "copyToClipboard"
    | "openEditorAfter"
    | "openFolderAfter"
    | "hideWindowOnCapture";

  /**
   * The after-capture switches: ShareX 21.0's checklist over this app's real
   * settings.
   * These are the same `Settings` fields the settings view writes, through the
   * same store — one source of truth, shown the way ShareX shows it.
   */
  const TASK_ROWS: { key: TaskKey; label: string; help: string }[] = [
    {
      key: "saveToDisk",
      label: "Save image to file",
      help: "Write a PNG into the screenshots folder."
    },
    {
      key: "copyToClipboard",
      label: "Copy image to clipboard",
      help: "Put the image on the clipboard as soon as it is taken."
    },
    {
      key: "openEditorAfter",
      label: "Open in image editor",
      help: "Jump straight into annotating."
    },
    {
      key: "openFolderAfter",
      label: "Show file in Explorer",
      help: "Reveal the new file once it is written."
    },
    {
      key: "hideWindowOnCapture",
      label: "Hide santi.sharex while capturing",
      help: "Keep this window out of fullscreen shots. Region capture ignores it."
    }
  ];

  const TOOLS: {
    icon: IconName;
    name: string;
    desc: string;
    milestone?: string;
    run?: () => void;
  }[] = [
    {
      icon: "pen",
      name: "Image editor",
      desc: "Open the most recent capture in the annotation editor.",
      run: () => void editLatest()
    },
    {
      icon: "video",
      name: "Screen recorder",
      desc: "Record the screen to GIF or MP4.",
      milestone: "M4"
    },
    {
      icon: "scan",
      name: "OCR",
      desc: "Read the text out of a capture.",
      milestone: "M5"
    },
    {
      icon: "scroll",
      name: "Scrolling capture",
      desc: "Stitch a long page into one image.",
      milestone: "M5"
    }
  ];

  const GROUPS: MenuRow[][] = [
    [
      { kind: "pane", label: "Capture", icon: "camera", tone: "accent", pane: "capture" },
      {
        kind: "soon",
        label: "Upload",
        icon: "upload",
        milestone: "M3",
        note: "Uploading arrives in M3, with the destinations that receive it."
      },
      {
        kind: "soon",
        label: "Workflows",
        icon: "workflow",
        milestone: "M5",
        note: "Workflows arrive in M5, alongside OCR and scrolling capture."
      },
      { kind: "pane", label: "Tools", icon: "wrench", tone: "success", pane: "tools" }
    ],
    [
      {
        kind: "pane",
        label: "After capture tasks",
        icon: "list-check",
        tone: "success",
        pane: "tasks"
      },
      {
        kind: "soon",
        label: "Destinations",
        icon: "cloud",
        milestone: "M3",
        note: "Upload destinations arrive in M3."
      }
    ],
    [
      {
        kind: "pane",
        label: "Application settings",
        icon: "settings",
        tone: "accent",
        pane: "settings"
      },
      { kind: "pane", label: "Hotkey settings", icon: "keyboard", tone: "accent", pane: "hotkeys" }
    ],
    [
      {
        kind: "action",
        label: "Screenshots folder",
        icon: "folder",
        tone: "success",
        run: () => void revealSaveDir()
      },
      { kind: "pane", label: "History", icon: "clock", tone: "accent", pane: "history" }
    ]
  ];

  // ShareX opens on its hotkey list, so this shell does too.
  let pane = $state<Pane>("hotkeys");
  let hotkeyStatus = $state<HotkeyStatus[]>([]);
  let version = $state("");

  const s = $derived(settings.current);
  const latest = $derived(history.items[0]);

  onMount(() => {
    getVersion()
      .then((v) => (version = v))
      .catch(() => (version = ""));
  });

  // The snapshot for the first paint, then every rebind. A failure leaves the
  // list empty, which the table reads as "not known yet" rather than asserting
  // a binding it could not confirm.
  $effect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    getHotkeyStatus()
      .then((list) => {
        if (!cancelled) hotkeyStatus = list;
      })
      .catch(() => {});
    onHotkeyStatus((list) => (hotkeyStatus = list)).then((un) => {
      if (cancelled) un();
      else unlisten = un;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  });

  /**
   * The report for one row, or `null` when there is none to trust.
   *
   * Rust reports against the accelerator it registered, so between a rebind
   * being saved and it landing the old report describes the old combo. Showing
   * it then would paint a freshly typed shortcut with the last one's fate.
   */
  function statusFor(action: keyof Hotkeys, accelerator: string): HotkeyStatus | null {
    const found = hotkeyStatus.find((entry) => entry.action === action);
    return found && found.accelerator === accelerator.trim() ? found : null;
  }

  function displayAccel(accel: string | undefined): string {
    if (!accel) return "Not set";
    return accel
      .split("+")
      .map((part) => {
        const key = part.trim();
        if (key === "CmdOrCtrl" || key === "CommandOrControl") return "Ctrl";
        if (key === "Super" || key === "Meta") return "Win";
        if (key === "PrintScreen") return "Print Screen";
        return key;
      })
      .join(" + ");
  }

  async function patch(delta: Partial<Settings>): Promise<void> {
    const ok = await settings.update(delta);
    if (!ok) toast.error(settings.error ?? "Could not save settings");
  }

  function setTask(key: TaskKey, value: boolean): void {
    const delta: Partial<Settings> = {};
    delta[key] = value;
    void patch(delta);
  }

  async function revealSaveDir(): Promise<void> {
    try {
      await openSaveDir();
    } catch (err) {
      toast.error(errorMessage(err));
    }
  }

  async function editLatest(): Promise<void> {
    const record = latest;
    if (!record) return;
    try {
      await openEditor(record.id);
    } catch (err) {
      toast.error(errorMessage(err));
    }
  }

  function isActive(row: MenuRow): boolean {
    return row.kind === "pane" && row.pane === pane;
  }

  function activate(row: MenuRow): void {
    if (row.kind === "pane") pane = row.pane;
    else if (row.kind === "action") row.run();
  }

  /** `View` and `Pane` overlap on exactly the three shared views. */
  function navigate(next: View): void {
    pane = next;
  }
</script>

<div class="shell">
  <nav class="menu scroll" aria-label="Sections">
    <div class="brand">
      <Icon name="region" size={14} />
      <span class="brand-name">santi.sharex</span>
      <span class="brand-ver tnum">{version ? `v${version}` : ""}</span>
    </div>

    {#each GROUPS as group, gi (gi)}
      <div class="group">
        {#each group as row (row.label)}
          {#if row.kind === "soon"}
            <!-- Not `disabled`: a disabled button shows no native tooltip, and
                 the tooltip is where the milestone is explained. The milestone
                 tag carries it without hovering. -->
            <button type="button" class="item soon" aria-disabled="true" title={row.note}>
              <span class="ico dim"><Icon name={row.icon} size={14} /></span>
              <span class="label">{row.label}</span>
              <span class="tag">{row.milestone}</span>
            </button>
          {:else}
            <button
              type="button"
              class="item"
              class:active={isActive(row)}
              aria-current={isActive(row) ? "page" : undefined}
              onclick={() => activate(row)}
            >
              <span class="ico {row.tone}"><Icon name={row.icon} size={14} /></span>
              <span class="label">{row.label}</span>
            </button>
          {/if}
        {/each}
      </div>
    {/each}
  </nav>

  <main class="pane scroll">
    {#if pane === "capture"}
      <CaptureView onNavigate={navigate} />
    {:else if pane === "history"}
      <HistoryView />
    {:else if pane === "settings"}
      <SettingsView />
    {:else if pane === "tools"}
      <section class="flat">
        <h1 class="flat-title">Tools</h1>
        <div class="grid">
          {#each TOOLS as tool (tool.name)}
            {@const blocked = tool.milestone !== undefined || (!latest && tool.run !== undefined)}
            <button
              type="button"
              class="tool"
              class:blocked
              aria-disabled={blocked ? "true" : undefined}
              title={tool.milestone
                ? `${tool.name} arrives in ${tool.milestone}.`
                : latest
                  ? undefined
                  : "Take a capture first — the editor opens an image you already have."}
              onclick={() => {
                if (!blocked) tool.run?.();
              }}
            >
              <span class="ico {blocked ? 'dim' : 'accent'}"><Icon name={tool.icon} size={16} /></span>
              <span class="tool-text">
                <span class="tool-name">{tool.name}</span>
                <span class="tool-desc">{tool.desc}</span>
              </span>
              {#if tool.milestone}
                <span class="tag">{tool.milestone}</span>
              {:else if !latest}
                <span class="tag">No captures yet</span>
              {/if}
            </button>
          {/each}
        </div>
      </section>
    {:else if pane === "tasks"}
      <section class="flat">
        <h1 class="flat-title">After capture tasks</h1>
        <p class="flat-note">
          What happens to every capture the moment it is taken. These are the same switches as
          Application settings, written to the same file.
        </p>
        {#if !s}
          <p class="flat-note">Loading settings…</p>
        {:else}
          <div class="tasks">
            {#each TASK_ROWS as row (row.key)}
              <div class="task">
                <span class="task-text">
                  <span class="task-label" id="sx-task-{row.key}">{row.label}</span>
                  <span class="task-help">{row.help}</span>
                </span>
                <Toggle
                  checked={s[row.key]}
                  labelledBy="sx-task-{row.key}"
                  onchange={(v) => setTask(row.key, v)}
                />
              </div>
            {/each}
          </div>
        {/if}
      </section>
    {:else}
      <section class="flat">
        <h1 class="flat-title">Hotkeys</h1>
        <table class="hk">
          <thead>
            <tr>
              <th scope="col">Hotkey</th>
              <th scope="col">Description</th>
            </tr>
          </thead>
          <tbody>
            {#each HOTKEY_ROWS as row (row.action)}
              {@const accel = s?.hotkeys[row.action] ?? ""}
              {@const status = statusFor(row.action, accel)}
              <tr>
                <td class="key">
                  <span
                    class="bar"
                    class:ok={status?.bound === true}
                    class:bad={status !== null && !status.bound}
                    title={status
                      ? (status.error ?? MECHANISM_TITLE[status.mechanism])
                      : "Waiting for the registration report."}
                  ></span>
                  <span class="accel">{displayAccel(accel)}</span>
                </td>
                <td>{row.description}</td>
              </tr>
            {/each}
          </tbody>
        </table>

        <div class="legend">
          <span class="swatch ok"></span>
          <span>Bound</span>
          <span class="swatch bad"></span>
          <span>Not bound — Windows gave the combination to something else</span>
          <button type="button" class="btn" onclick={() => (pane = "settings")}>
            Change hotkeys…
          </button>
        </div>
      </section>
    {/if}
  </main>
</div>

<style>
  .shell {
    display: flex;
    height: 100vh;
    overflow: hidden;
    background: var(--bg);
    color: var(--text);
  }

  /* One hairline and a raised fill — the whole separation between menu and
     content. No shadow, no inset, nothing rounded. */
  .menu {
    flex: none;
    width: 200px;
    height: 100%;
    padding: 0 0 12px;
    overflow-y: auto;
    border-right: 1px solid var(--border);
    background: var(--bg-raised);
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 7px;
    height: 34px;
    padding: 0 10px;
    border-bottom: 1px solid var(--border);
    color: var(--accent);
  }

  .brand-name {
    font-size: 12px;
    font-weight: 600;
    color: var(--text);
  }

  .brand-ver {
    margin-left: auto;
    font-size: 11px;
    color: var(--text-faint);
  }

  /* Groups are separated by a hairline and a gap, never by a box. */
  .group {
    padding: 6px 0;
  }

  .group + .group {
    border-top: 1px solid var(--border);
  }

  .item {
    display: flex;
    align-items: center;
    gap: 7px;
    width: 100%;
    height: 24px;
    padding: 0 10px;
    border: 0;
    background: none;
    font: inherit;
    font-size: 12px;
    color: var(--text-dim);
    text-align: left;
    cursor: pointer;
  }

  .item:hover {
    background: var(--surface);
    color: var(--text);
  }

  .item.active {
    background: color-mix(in srgb, var(--accent) 26%, transparent);
    color: var(--text);
  }

  .item:focus-visible {
    outline: 1px solid var(--accent);
    outline-offset: -1px;
  }

  .label {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .item.soon {
    color: var(--text-faint);
    cursor: default;
  }

  .item.soon:hover {
    background: none;
    color: var(--text-faint);
  }

  /* The coloured glyph on each row, tinted from tokens only. */
  .ico {
    display: flex;
    flex: none;
  }

  .ico.accent {
    color: var(--accent);
  }

  .ico.success {
    color: var(--success);
  }

  .ico.dim {
    color: var(--text-faint);
  }

  /* The milestone a row is waiting on, readable without hovering. */
  .tag {
    margin-left: auto;
    flex: none;
    padding: 0 5px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 10px;
    line-height: 15px;
    letter-spacing: 0.02em;
    color: var(--text-faint);
    background: var(--bg-inset);
  }

  /* The pane is the scroll container itself, exactly as in the default shell,
     so the shared views' sticky headers still pin. Views own their padding. */
  .pane {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
    overflow-x: hidden;
  }

  /* Content sits straight on the background: no card, no radius, no shadow. */
  .flat {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 12px;
    padding: 16px 20px 28px;
  }

  .flat-title {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
    color: var(--text);
  }

  .flat-note {
    max-width: 68ch;
    font-size: 12px;
    line-height: 1.6;
    color: var(--text-dim);
  }

  /* ShareX's hotkey list: a bordered grid, centred cells, 1px gridlines, and a
     status bar down the left edge of every row. */
  .hk {
    border-collapse: collapse;
    width: 100%;
    max-width: 720px;
    border: 1px solid var(--border);
    font-size: 12px;
  }

  .hk th,
  .hk td {
    padding: 5px 10px;
    border: 1px solid var(--border);
    text-align: center;
    vertical-align: middle;
  }

  .hk th {
    font-weight: 600;
    color: var(--text-dim);
    background: var(--bg-raised);
  }

  .hk td {
    color: var(--text);
  }

  .hk tbody tr:hover td {
    background: var(--surface);
  }

  .key {
    position: relative;
    width: 44%;
    padding-left: 18px;
  }

  .accel {
    font-variant-numeric: tabular-nums;
  }

  /* Green bound, red not — driven by the real registration report, so an
     unbound hotkey is visible here without opening settings. Neutral while no
     report has arrived: asserting either colour then would be a guess. */
  .bar {
    position: absolute;
    left: 0;
    top: 0;
    bottom: 0;
    width: 5px;
    background: var(--border-strong);
  }

  .bar.ok {
    background: var(--success);
  }

  .bar.bad {
    background: var(--danger);
  }

  .legend {
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 11px;
    color: var(--text-faint);
  }

  .swatch {
    width: 9px;
    height: 9px;
    flex: none;
    background: var(--border-strong);
  }

  .swatch.ok {
    background: var(--success);
  }

  .swatch.bad {
    background: var(--danger);
  }

  .legend .btn {
    height: 24px;
    margin-left: 10px;
    padding: 0 10px;
    font-size: 12px;
    font-weight: 400;
    cursor: pointer;
  }

  .grid {
    display: flex;
    flex-direction: column;
    width: 100%;
    max-width: 720px;
    border: 1px solid var(--border);
  }

  .tool {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 8px 10px;
    border: 0;
    background: none;
    font: inherit;
    color: var(--text);
    text-align: left;
    cursor: pointer;
  }

  .tool + .tool {
    border-top: 1px solid var(--border);
  }

  .tool:hover {
    background: var(--surface);
  }

  .tool:focus-visible {
    outline: 1px solid var(--accent);
    outline-offset: -1px;
  }

  .tool.blocked {
    cursor: default;
  }

  .tool.blocked:hover {
    background: none;
  }

  .tool.blocked .tool-name {
    color: var(--text-faint);
  }

  .tool-text {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }

  .tool-name {
    font-size: 12px;
    font-weight: 600;
  }

  .tool-desc {
    font-size: 12px;
    color: var(--text-faint);
  }

  .tasks {
    display: flex;
    flex-direction: column;
    width: 100%;
    max-width: 720px;
    border: 1px solid var(--border);
  }

  .task {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 20px;
    padding: 8px 10px;
  }

  .task + .task {
    border-top: 1px solid var(--border);
  }

  .task-text {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }

  .task-label {
    font-size: 12px;
    color: var(--text);
  }

  .task-help {
    font-size: 12px;
    color: var(--text-faint);
  }
</style>
