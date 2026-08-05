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
   *   1. Every colour is a token, with exactly one documented exception —
   *      `SHAREX_ICON_INK` below. The theme does the rest of the colouring.
   *   2. Nothing is faked. Rows santi.sharex cannot honour yet are visibly disabled
   *      and name the milestone they land in; everything else routes into the
   *      same view components the default shell uses.
   */
  import { onMount } from "svelte";
  import { getVersion } from "@tauri-apps/api/app";
  import Icon, { type IconName } from "$lib/components/Icon.svelte";
  import OcrPanel from "$lib/components/OcrPanel.svelte";
  import Toggle from "$lib/components/Toggle.svelte";
  import { toast } from "$lib/components/Toast.svelte";
  import CaptureView from "$lib/views/CaptureView.svelte";
  import DestinationsView from "$lib/views/DestinationsView.svelte";
  import HistoryView from "$lib/views/HistoryView.svelte";
  import WorkflowsView from "$lib/views/WorkflowsView.svelte";
  import SettingsView from "$lib/views/SettingsView.svelte";
  import { settings } from "$lib/stores/settings.svelte";
  import { history } from "$lib/stores/history.svelte";
  import {
    captureIsRecording,
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
   * The `View` members route into the shared views; the rest are pages that
   * exist only in this shell, because only ShareX's menu asks for them.
   *
   * `destinations` is not in `View` on purpose: the default shell's sidebar has
   * three fixed entries and reaches the same form through Settings, so widening
   * `View` would add a route only one of the two shells can navigate to.
   */
  type Pane = View | "hotkeys" | "tools" | "tasks" | "destinations";

  /**
   * ShareX's menu icons, and the one place this file is allowed a literal colour.
   *
   * The signature of ShareX's main window is that *every* menu glyph is its own
   * saturated colour — a blue camera, an orange up-arrow, a green grid, a red
   * wrench, a yellow folder — read as a column of small coloured marks against
   * grey. A menu tinted from `--accent`/`--success` is the single loudest tell
   * that a replica is not the real thing, and no token set can express "ten
   * unrelated hues" without inventing ten tokens that mean nothing in the other
   * three themes.
   *
   * So this is a **scoped exception to the no-hard-coded-colour rule**, exactly
   * analogous to the two that already exist: the annotation palette (M2 §…) and
   * the theme picker's four preview cards (M2.6 §4), both of which are literal
   * for the same reason — they depict something rather than wearing the theme.
   * These values only ever render inside `ShareXShell`, which only ever mounts
   * when `theme === "sharex"`, so no other palette can be polluted by them.
   *
   * They are all mid-to-light hues chosen to clear the `sharex` background
   * (#1c1c1c) comfortably; each also sits beside a `--text-dim`/`--text` label,
   * so no meaning rests on the colour alone.
   *
   * Keep every literal in this table. Nothing below it may name a colour.
   */
  const SHAREX_ICON_INK = {
    capture: "#5aa9e6", // camera — Windows blue
    upload: "#f0902b", // up arrow — orange
    workflows: "#5cb85c", // grid — green
    tools: "#e0605e", // wrench — red/pink
    tasks: "#6fbf73", // checklist — green
    destinations: "#4aa3dc", // cloud — blue
    settings: "#93a6b8", // gear — grey-blue
    hotkeys: "#5aa9e6", // keyboard — blue
    folder: "#e3b23c", // folder — yellow
    history: "#57a6dd", // clock — blue
    editor: "#c98ade", // pen — violet
    recorder: "#e0605e", // camcorder — red
    ocr: "#4fb3a5", // scan — teal
    scrolling: "#8fb04a" // scroll — olive
  } as const;

  /** One entry from the table above. */
  type Ink = (typeof SHAREX_ICON_INK)[keyof typeof SHAREX_ICON_INK];

  type MenuRow =
    | { kind: "pane"; label: string; icon: IconName; ink: Ink; pane: Pane; hint?: string }
    | {
        kind: "action";
        label: string;
        icon: IconName;
        ink: Ink;
        run: () => void;
        /** Shown on hover when the row goes somewhere its label does not name. */
        hint?: string;
      }
    | {
        kind: "soon";
        label: string;
        icon: IconName;
        ink: Ink;
        milestone: string;
        note: string;
      };

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
    ink: Ink;
    name: string;
    desc: string;
    milestone?: string;
    /** Acts on an existing capture, so it is dead until there is one. */
    needsCapture?: boolean;
    /** Shown on hover when the row is live and does something non-obvious. */
    hint?: string;
    run?: () => void;
  }[] = [
    {
      icon: "pen",
      ink: SHAREX_ICON_INK.editor,
      name: "Image editor",
      desc: "Open the most recent capture in the annotation editor.",
      needsCapture: true,
      run: () => void editLatest()
    },
    {
      icon: "video",
      ink: SHAREX_ICON_INK.recorder,
      name: "Screen recorder",
      desc: "Record a region, a window or a display to MP4 or GIF. Opens the Capture screen.",
      // M4 shipped recording, so this row can no longer wear an "M4" tag — a
      // milestone that has already landed is as inaccurate as a fake feature.
      // It routes to Capture rather than duplicating the flow, for the same
      // reason Scrolling capture does: the source pickers, the ffmpeg state and
      // the Stop already live there, and one screen is the honest version.
      hint: "Opens Capture, where the source pickers and the recorder's controls live.",
      run: () => {
        pane = "capture";
      }
    },
    {
      icon: "scan",
      ink: SHAREX_ICON_INK.ocr,
      name: "OCR",
      desc: "Read the text out of the most recent capture.",
      needsCapture: true,
      run: () => {
        // Same gate the Lightbox uses: a record with no file on disk has only a
        // thumbnail, and OCR on a 480px thumbnail is not worth offering.
        if (!latest?.path) {
          toast.error("That capture was not saved to disk, so there is no image to read.");
          return;
        }
        if (captureIsRecording(latest)) {
          toast.error("There is no page of text in a video. Your most recent capture is a recording.");
          return;
        }
        ocrOpen = true;
      }
    },
    {
      icon: "scroll",
      ink: SHAREX_ICON_INK.scrolling,
      name: "Scrolling capture",
      desc: "Stitch a long page into one image. Opens the Capture screen.",
      // Routes to Capture rather than duplicating the flow: it needs a window
      // picker, live progress and a working Cancel, all of which already live
      // there. ShareX opens a dialog here; one screen is the honest version —
      // but the row has to say so rather than looking like it starts a capture.
      hint: "Opens Capture, where the window picker and the progress readout live.",
      run: () => {
        pane = "capture";
      }
    }
  ];

  const GROUPS: MenuRow[][] = [
    [
      {
        kind: "pane",
        label: "Capture",
        icon: "camera",
        ink: SHAREX_ICON_INK.capture,
        pane: "capture"
      },
      // M3 shipped uploading, so this can no longer sit here wearing an "M3"
      // tag — a milestone that has already landed is as inaccurate as a fake
      // feature. Uploading is an action *on a capture* rather than a screen of
      // its own, so the row opens the screen where those captures are, the same
      // way Tools › Scrolling capture opens Capture. An `action` and not a
      // `pane`, so History's own row keeps the selected highlight to itself.
      {
        kind: "action",
        label: "Upload",
        icon: "upload",
        ink: SHAREX_ICON_INK.upload,
        hint: "Opens History, where every capture has an Upload action. Nothing is uploaded until you press it.",
        run: () => {
          pane = "history";
        }
      },
      {
        kind: "pane",
        label: "Workflows",
        icon: "workflow",
        ink: SHAREX_ICON_INK.workflows,
        pane: "workflows"
      },
      { kind: "pane", label: "Tools", icon: "wrench", ink: SHAREX_ICON_INK.tools, pane: "tools" }
    ],
    [
      {
        kind: "pane",
        label: "After capture tasks",
        icon: "list-check",
        ink: SHAREX_ICON_INK.tasks,
        pane: "tasks"
      },
      // Genuinely live as of M3, not merely enabled: it routes into the real
      // destination picker and the real credential forms.
      {
        kind: "pane",
        label: "Destinations",
        icon: "cloud",
        ink: SHAREX_ICON_INK.destinations,
        pane: "destinations"
      }
    ],
    [
      {
        kind: "pane",
        label: "Application settings",
        icon: "settings",
        ink: SHAREX_ICON_INK.settings,
        pane: "settings"
      },
      {
        kind: "pane",
        label: "Hotkey settings",
        icon: "keyboard",
        ink: SHAREX_ICON_INK.hotkeys,
        pane: "hotkeys"
      }
    ],
    [
      {
        kind: "action",
        label: "Screenshots folder",
        icon: "folder",
        ink: SHAREX_ICON_INK.folder,
        run: () => void revealSaveDir()
      },
      { kind: "pane", label: "History", icon: "clock", ink: SHAREX_ICON_INK.history, pane: "history" }
    ]
  ];

  // ShareX opens on its hotkey list, so this shell does too.
  let pane = $state<Pane>("hotkeys");
  let ocrOpen = $state(false);
  let hotkeyStatus = $state<HotkeyStatus[]>([]);
  let version = $state("");

  const s = $derived(settings.current);
  const latest = $derived(history.items[0]);

  /**
   * Both `needsCapture` rows — the image editor and OCR — are image operations,
   * and the most recent capture is exactly where a recording lands (M4 §5). So
   * "there is a capture" is not enough of a question to ask: a row that acts on
   * the latest capture has to know whether that capture is a picture, or it ends
   * in one of Rust's `could not reopen …` errors from `image::open` on an MP4.
   */
  const latestIsImage = $derived(latest !== undefined && !captureIsRecording(latest));

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
    // The row is already blocked for a recording; this is the second half of the
    // same guard, and the same sentence the lightbox and the history card use.
    if (captureIsRecording(record)) {
      toast.error("The editor works on still images. Your most recent capture is a recording.");
      return;
    }
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
                 tag carries it without hovering. The glyph keeps its colour but
                 dimmed, so the row still reads as part of ShareX's coloured
                 column while the faint label and the tag say it is inert. -->
            <button type="button" class="item soon" aria-disabled="true" title={row.note}>
              <span class="ico" style="--sx-ink: {row.ink}"><Icon name={row.icon} size={14} /></span>
              <span class="label">{row.label}</span>
              <span class="tag">{row.milestone}</span>
            </button>
          {:else}
            <button
              type="button"
              class="item"
              class:active={isActive(row)}
              aria-current={isActive(row) ? "page" : undefined}
              title={row.hint}
              onclick={() => activate(row)}
            >
              <span class="ico" style="--sx-ink: {row.ink}"><Icon name={row.icon} size={14} /></span>
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
      <!-- This shell has its own Destinations row, so Settings does not repeat
           the form: one screen owns it, and the menu says which. -->
      <SettingsView showDestinations={false} />
    {:else if pane === "workflows"}
      <WorkflowsView />
    {:else if pane === "destinations"}
      <DestinationsView />
    {:else if pane === "tools"}
      <section class="flat">
        <h1 class="flat-title">Tools</h1>
        <div class="grid">
          {#each TOOLS as tool (tool.name)}
            {@const blocked =
              tool.milestone !== undefined ||
              (tool.needsCapture === true && (!latest || !latestIsImage))}
            <button
              type="button"
              class="tool"
              class:blocked
              aria-disabled={blocked ? "true" : undefined}
              title={tool.milestone
                ? `${tool.name} arrives in ${tool.milestone}.`
                : tool.needsCapture && !latest
                  ? "Take a capture first — this acts on an image you already have."
                  : tool.needsCapture && !latestIsImage
                    ? "Your most recent capture is a screen recording, and this acts on a still image."
                    : tool.hint}
              onclick={() => {
                if (!blocked) tool.run?.();
              }}
            >
              <span class="ico" style="--sx-ink: {tool.ink}"><Icon name={tool.icon} size={16} /></span>
              <span class="tool-text">
                <span class="tool-name">{tool.name}</span>
                <span class="tool-desc">{tool.desc}</span>
              </span>
              {#if tool.milestone}
                <span class="tag">{tool.milestone}</span>
              {:else if tool.needsCapture && !latest}
                <span class="tag">No captures yet</span>
              {:else if tool.needsCapture && !latestIsImage}
                <span class="tag">Latest is a recording</span>
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
                  <!-- The bar's colour must not be the only thing carrying the
                       state. A `title` on a bare <span> is a hover affordance,
                       not an accessible name — most screen readers will never
                       announce it — so the state is also here as real text,
                       clipped out of the visual layout but in the accessibility
                       tree and in the row's reading order. -->
                  <span class="sr">
                    {status ? (status.bound ? "Bound." : "Not bound.") : "Not reported yet."}
                  </span>
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

<!-- Tools › OCR. `latest.path` is re-checked here because the record can be
     deleted from History while the panel is open. -->
{#if ocrOpen && latest?.path && latestIsImage}
  <OcrPanel record={latest} onclose={() => (ocrOpen = false)} />
{/if}

<style>
  /* 12px Segoe UI, tight leading: ShareX is a dense WinForms app and the type
     scale is most of why it feels like one. Everything below inherits this
     rather than restating it. */
  .shell {
    display: flex;
    height: 100vh;
    overflow: hidden;
    background: var(--bg);
    color: var(--text);
    font-size: 12px;
    line-height: 1.35;
  }

  /* The menu is one flat slab a shade DARKER than the content, parted from it
     by a single hairline. Not a card, not an inset, not a gap — ShareX has no
     elevation anywhere, and the darker-not-lighter direction is what makes the
     content read as the form's face rather than as a panel dropped onto it. */
  .menu {
    flex: none;
    width: 200px;
    height: 100%;
    padding: 0 0 10px;
    overflow-y: auto;
    border-right: 1px solid var(--border);
    background: var(--bg-inset);
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 6px;
    height: 28px;
    padding: 0 8px;
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

  /* Groups are parted by a gap and nothing else — no rule, no box. ShareX
     leaves air between clusters and lets the eye do the grouping. */
  .group + .group {
    margin-top: 12px;
  }

  .brand + .group {
    margin-top: 8px;
  }

  /* 24px, full-bleed to both edges of the column, tightly stacked. */
  .item {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    height: 24px;
    padding: 0 8px;
    border: 0;
    background: none;
    font: inherit;
    line-height: 1;
    color: var(--text-dim);
    text-align: left;
    cursor: pointer;
  }

  .item:hover {
    background: var(--surface);
    color: var(--text);
  }

  /* Flat, slightly lighter fill. No rounding, no left accent bar — ShareX
     selects a row by lifting its grey, nothing more. Mixed from --text rather
     than --accent so it stays a grey highlight instead of a blue one. */
  .item.active {
    background: color-mix(in srgb, var(--text) 13%, transparent);
    color: var(--text);
  }

  /* Inset so the ring is not clipped by the scrolling column, and 2px because
     density is not a reason to ship a focus indicator you have to hunt for. */
  .item:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
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

  /* Each glyph wears its own colour from `SHAREX_ICON_INK` — the column of
     small coloured marks is ShareX's signature. The fallback is the label's own
     colour, so a row that ever ships without an ink degrades to monochrome
     rather than to nothing. */
  .ico {
    display: flex;
    flex: none;
    color: var(--sx-ink, currentColor);
  }

  /* Dimmed, not drained: the hue survives so the column still reads, while the
     row plainly says it is not available. */
  .item.soon .ico,
  .tool.blocked .ico {
    opacity: 0.45;
  }

  /* The milestone a row is waiting on, readable without hovering. Square, like
     everything else here. */
  .tag {
    margin-left: auto;
    flex: none;
    padding: 0 4px;
    border: 1px solid var(--border);
    border-radius: 0;
    font-size: 10px;
    line-height: 14px;
    letter-spacing: 0.02em;
    color: var(--text-faint);
    background: var(--bg);
  }

  /* The pane is the scroll container itself, exactly as in the default shell,
     so the shared views' sticky headers still pin. Views own their padding.

     The gradient is the small thing that does a lot of the "this is a WinForms
     form" work: a hair lighter at the top, settling into --bg well before the
     fold. Mixed from --text so it is still the palette doing the colouring, and
     left at the default background-attachment so it stays pinned to the top of
     the pane instead of sliding away with the content. */
  .pane {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
    overflow-x: hidden;
    background:
      linear-gradient(
        180deg,
        color-mix(in srgb, var(--text) 5%, var(--bg)) 0,
        var(--bg) 260px
      )
      no-repeat;
  }

  /* Whatever the shared views bring with them, they sit straight on the form
     while they are inside this shell: no elevated cards, no rounded panels, no
     pills.

     The `sharex` theme already sets --radius/--radius-lg to 0, so everything
     that reads those tokens is square before this rule runs. What is left is
     the short list of *hard literal* radii in the shared components — a 999px
     switch track, a 50% knob, a 12px hero tile, a few 5–6px keycaps and
     badges — none of which any token can reach. They are enumerated rather
     than swept up with `:global(*)` on purpose: the theme picker's preview
     mocks scale their corners from `--m-radius` to depict the theme they are
     advertising, and a blanket rule would square the very thing whose job is
     to show that the other three themes are not square.

     The shadow half of the rule is why `.card` is listed at all: the `sharex`
     palette still defines --shadow for the editor and preview windows, and a
     drop shadow on content sitting straight on a WinForms form is the one
     thing ShareX never does. */
  .pane :global(.card),
  .pane :global(.panel),
  .pane :global(.empty),
  .pane :global(.btn),
  .pane :global(.input),
  .pane :global(.select),
  .pane :global(.chip),
  .pane :global(.kbd),
  .pane :global(kbd),
  .pane :global(code),
  .pane :global(.toggle),
  .pane :global(.toggle .knob),
  .pane :global(.shot-icon),
  .pane :global(.thumb-meta) {
    border-radius: 0;
    box-shadow: none;
  }

  /* Content sits straight on the background: no card, no radius, no shadow. */
  .flat {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 10px;
    padding: 12px 14px 24px;
  }

  .flat-title {
    margin: 0;
    font-size: 12px;
    font-weight: 600;
    line-height: 1.3;
    color: var(--text);
  }

  .flat-note {
    max-width: 62ch;
    line-height: 1.5;
    color: var(--text-dim);
  }

  /* ShareX's hotkey list: a real bordered grid — 1px lines on every cell in a
     mid grey that is plainly visible against the form — with a header row,
     centred cells, and a status bar down the left edge of every row.

     Natural width, not stretched: it is a block sitting near the top-left with
     the rest of the form left empty, which is exactly how ShareX shows it. A
     `table` shrink-wraps by default, so the job here is not to make it grow. */
  .hk {
    border-collapse: collapse;
    border: 1px solid var(--border-strong);
    line-height: 1.3;
  }

  .hk th,
  .hk td {
    padding: 4px 14px;
    border: 1px solid var(--border-strong);
    text-align: center;
    vertical-align: middle;
    white-space: nowrap;
  }

  /* Slightly bolder, slightly lighter than the cells below it. */
  .hk th {
    font-weight: 600;
    color: var(--text);
    background: var(--bg-raised);
  }

  .hk td {
    color: var(--text-dim);
  }

  .hk tbody tr:hover td {
    background: var(--surface);
  }

  /* Qualified so it outranks the `.hk td` padding above — the status bar needs
     the room, and a bare `.key` would lose the cascade and sit under the text.
     Padded on both sides so the cell is still genuinely centred: widening only
     the left would push every accelerator 4px off centre, in a table whose
     whole point is that ShareX centres its cells. */
  .hk td.key {
    position: relative;
    padding-left: 22px;
    padding-right: 22px;
  }

  /* Text that exists for the accessibility tree only — same pattern as
     ColorPicker's `.sr`. */
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }

  .accel {
    font-variant-numeric: tabular-nums;
    color: var(--text);
  }

  /* Green bound, red not — driven by the real registration report, so an
     unbound hotkey is visible here without opening settings. Neutral while no
     report has arrived: asserting either colour then would be a guess. */
  .bar {
    position: absolute;
    left: 0;
    top: 0;
    bottom: 0;
    width: 6px;
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
    gap: 6px;
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

  /* A WinForms push button: square, short, no weight. */
  .legend .btn {
    height: 23px;
    margin-left: 10px;
    padding: 0 12px;
    border-radius: 0;
    font-size: 12px;
    font-weight: 400;
    cursor: pointer;
  }

  /* The other panes get the same treatment as the table: a natural-width
     bordered block near the top-left, gridlines in the same visible grey, and
     no container of its own around it. */
  .grid {
    display: flex;
    flex-direction: column;
    width: 100%;
    max-width: 540px;
    border: 1px solid var(--border-strong);
  }

  .tool {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 6px 10px;
    border: 0;
    background: none;
    font: inherit;
    color: var(--text);
    text-align: left;
    cursor: pointer;
  }

  .tool + .tool {
    border-top: 1px solid var(--border-strong);
  }

  .tool:hover {
    background: var(--surface);
  }

  .tool:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
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
    min-width: 0;
  }

  .tool-name {
    font-weight: 600;
  }

  .tool-desc {
    color: var(--text-faint);
  }

  .tasks {
    display: flex;
    flex-direction: column;
    width: 100%;
    max-width: 540px;
    border: 1px solid var(--border-strong);
  }

  .task {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 6px 10px;
  }

  .task + .task {
    border-top: 1px solid var(--border-strong);
  }

  .task-text {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .task-label {
    color: var(--text);
  }

  .task-help {
    color: var(--text-faint);
  }
</style>
