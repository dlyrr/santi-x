<script lang="ts">
  /**
   * One workflow, opened for editing (M6 §4).
   *
   * Rendered in place of the list rather than over it: a workflow has six or
   * seven decisions in it, several of which need a picker of their own, and a
   * dialog that has to scroll is a dialog that hides the thing it is validating.
   *
   * **Everything is validated here, not at run time.** A workflow that looks
   * fine and fails when you press its hotkey is the outcome this screen is built
   * against — by then the user is in another app, the overlay has already come
   * and gone, and the only evidence is a toast nobody is looking at. So the
   * chain preview updates live, `workflowIssues()` runs on every keystroke, and
   * Save is refused for a chain that cannot run at all.
   *
   * The trigger control is the same click-to-record gesture Settings uses, down
   * to the shared `acceleratorFromEvent()` — a combination typed here has to be
   * spelled exactly the way Settings would have spelled it, or the conflict
   * check could not compare the two at all.
   */
  import Icon, { type IconName } from '$lib/components/Icon.svelte';
  import {
    acceleratorFromEvent,
    displayAccelerator,
    listMonitors,
    listWindows,
    recordFormatOf,
    workflowCanSave,
    workflowChainLine,
    workflowDestinationOf,
    workflowIssues,
    workflowUploads,
    DESTINATION_KINDS,
    DESTINATION_LABEL,
    RECORD_FORMAT_LABEL,
    RECORD_FORMATS,
    WORKFLOW_ACTION_HELP,
    WORKFLOW_ACTION_KINDS,
    WORKFLOW_ACTION_NAME,
    WORKFLOW_CAPTURE_KINDS,
    WORKFLOW_CAPTURE_NAME,
    type DestinationChoice,
    type DestinationStatus,
    type MonitorInfo,
    type Settings,
    type WindowInfo,
    type Workflow,
    type WorkflowAction,
    type WorkflowActionKind,
    type WorkflowCapture,
    type WorkflowCaptureKind,
    type WorkflowIssue
  } from '$lib/api';
  import { untrack } from 'svelte';
  import { DEFAULT_RECORD_FORMAT } from '$lib/types';

  interface Props {
    /** The workflow to edit. Copied on mount; the caller's object is untouched. */
    workflow: Workflow;
    /** Every workflow, for the hotkey conflict check. Includes this one. */
    workflows: Workflow[];
    /** `destination_status()`, or `null` while it has not been read. */
    destinations: DestinationStatus[] | null;
    /** The live settings, for the built-in hotkeys the trigger may collide with. */
    settings: Settings | null;
    /** Whether this draft has never been saved — the header and Save say so. */
    isNew?: boolean;
    onsave: (workflow: Workflow) => void;
    oncancel: () => void;
    /** Route to the Destinations screen. Each shell knows where its own is. */
    onopendestinations?: () => void;
  }

  let {
    workflow,
    workflows,
    destinations,
    settings,
    isNew = false,
    onsave,
    oncancel,
    onopendestinations
  }: Props = $props();

  /** Which glyph stands for each step. Icon-only would be a puzzle; these sit
   *  beside their own names and only carry the shape of the step. */
  const CAPTURE_ICON: Record<WorkflowCaptureKind, IconName> = {
    region: 'region',
    fullscreen: 'fullscreen',
    activeWindow: 'window',
    monitor: 'monitor',
    window: 'window',
    scrolling: 'scroll',
    record: 'video'
  };

  const ACTION_ICON: Record<WorkflowActionKind, IconName> = {
    annotate: 'pen',
    saveToDisk: 'download',
    copyImage: 'copy',
    ocr: 'scan',
    openFolder: 'folder'
  };

  /** What each capture kind grabs, one line, beside the kind itself. */
  const CAPTURE_HELP: Record<WorkflowCaptureKind, string> = {
    region: 'Freeze the desktop and drag out a selection. Cancelling ends the workflow.',
    fullscreen: 'Every monitor composited into one image.',
    activeWindow: 'Whichever window has focus when the workflow runs.',
    monitor: 'One display, chosen now.',
    window: 'One window, chosen now.',
    scrolling: 'Scroll a window and stitch the frames into one tall image.',
    record: 'Record the screen. The workflow continues once the recording is stopped.'
  };

  /**
   * The draft. Copied so backing out really does back out, and deeply enough
   * that reordering the actions cannot reach into the caller's array.
   *
   * Seeded once: the parent mounts this component per edit, so there is no
   * reseeding effect to fight with a half-typed name.
   */
  // `untrack` states the intent the warning asks about: this reads `workflow`
  // once and must NOT re-seed, or a keystroke would be undone the moment the
  // parent re-rendered. The parent keys this component by workflow id, so a
  // different workflow arrives as a fresh instance rather than a new prop.
  let draft = $state<Workflow>(untrack(() => clone(workflow)));

  /** True while the trigger button is listening for a combination. */
  let recordingHotkey = $state(false);

  /**
   * The last combination typed, kept so flipping to Run manually and back does
   * not read as a request to forget it.
   */
  let lastAccelerator = $state(
    untrack(() => (workflow.trigger.type === 'hotkey' ? workflow.trigger.accelerator : ''))
  );

  let monitors = $state<MonitorInfo[]>([]);
  let windows = $state<WindowInfo[]>([]);
  let sourcesError = $state('');

  const captureKind = $derived(draft.capture.type);

  /** Only the kinds that name a specific thing need the enumeration. */
  const needsMonitors = $derived(captureKind === 'monitor');
  const needsWindows = $derived(captureKind === 'window' || captureKind === 'scrolling');

  /**
   * Narrowed here rather than with `{@const}` inside the block that shows them:
   * each Svelte block compiles to its own function, so a discriminant tested in
   * the `{#if}` is not still narrowed inside it as far as `svelte-check` is
   * concerned.
   */
  const triggerAccelerator = $derived(
    draft.trigger.type === 'hotkey' ? draft.trigger.accelerator : ''
  );
  const recordFormat = $derived(
    draft.capture.type === 'record' ? recordFormatOf(draft.capture.format) : DEFAULT_RECORD_FORMAT
  );

  /**
   * The source id the capture names, or `0` when it names none. Kept apart from
   * `workflowIssues` because it is the one check that needs the live monitor and
   * window enumerations, which are this screen's business rather than the
   * workflow's.
   */
  const sourceId = $derived.by(() => {
    const capture = draft.capture;
    if (capture.type === 'monitor' || capture.type === 'window') return capture.id;
    if (capture.type === 'scrolling') return capture.window;
    return 0;
  });

  /** Refusals this screen makes that the shared check cannot see. */
  const localErrors = $derived.by(() => {
    const out: string[] = [];
    if ((needsMonitors || needsWindows) && sourceId === 0) {
      out.push(
        needsMonitors ? 'Choose which display to capture.' : 'Choose which window to capture.'
      );
    }
    return out;
  });

  const issues = $derived<WorkflowIssue[]>(
    workflowIssues(draft, { workflows, destinations, settings })
  );
  const errors = $derived(issues.filter((issue) => issue.level === 'error'));
  const warnings = $derived(issues.filter((issue) => issue.level === 'warning'));
  const canSave = $derived(workflowCanSave(issues) && localErrors.length === 0);

  const chain = $derived(workflowChainLine(draft, Number.MAX_SAFE_INTEGER));

  /**
   * The chosen source is not there right now. A note, never a refusal: window
   * ids do not survive a window being closed and reopened, and refusing to save
   * a workflow whose window happens to be shut would make it uneditable.
   */
  const sourceMissing = $derived.by(() => {
    if (sourceId === 0) return false;
    if (needsMonitors) return monitors.length > 0 && !monitors.some((m) => m.id === sourceId);
    if (needsWindows) return windows.length > 0 && !windows.some((w) => w.id === sourceId);
    return false;
  });

  // Enumerated only for the kinds that need it, and again whenever one of those
  // is selected: the window list is a snapshot of the desktop a moment ago.
  $effect(() => {
    if (!needsMonitors && !needsWindows) return;
    void loadSources(needsMonitors, needsWindows);
  });

  function clone(source: Workflow): Workflow {
    return {
      ...source,
      trigger: { ...source.trigger },
      capture: { ...source.capture },
      actions: source.actions.map((action) => ({ ...action }))
    };
  }

  async function loadSources(wantMonitors: boolean, wantWindows: boolean): Promise<void> {
    sourcesError = '';
    try {
      if (wantMonitors) {
        monitors = await listMonitors();
        // A display picker with nothing chosen is a step the user has to take
        // for no reason; a window picker is not, because there is no obvious
        // window and guessing one would be a workflow pointed at the wrong app.
        if (draft.capture.type === 'monitor' && draft.capture.id === 0) {
          const primary = monitors.find((m) => m.isPrimary) ?? monitors[0];
          if (primary) draft.capture = { type: 'monitor', id: primary.id };
        }
      }
      if (wantWindows) windows = await listWindows();
    } catch {
      sourcesError = 'Could not read the list of screens. Try again in a moment.';
    }
  }

  /* --------------------------------------------------------------- trigger */

  function onWindowKeydown(event: KeyboardEvent): void {
    if (!recordingHotkey) return;
    event.preventDefault();
    event.stopPropagation();
    if (event.key === 'Escape') {
      recordingHotkey = false;
      return;
    }
    const accel = acceleratorFromEvent(event);
    if (accel) setAccelerator(accel);
  }

  function onWindowKeyup(event: KeyboardEvent): void {
    if (!recordingHotkey) return;
    // Windows hands PrintScreen to the webview as a keyup only — the keydown is
    // swallowed by the OS snipping handler — so that combo has to be read here.
    if (event.key !== 'PrintScreen' && event.code !== 'PrintScreen') return;
    event.preventDefault();
    const accel = acceleratorFromEvent(event);
    if (accel) setAccelerator(accel);
  }

  function setAccelerator(accelerator: string): void {
    recordingHotkey = false;
    lastAccelerator = accelerator;
    draft.trigger = { type: 'hotkey', accelerator };
  }

  function setTriggerKind(kind: 'hotkey' | 'manual'): void {
    recordingHotkey = false;
    draft.trigger =
      kind === 'manual' ? { type: 'manual' } : { type: 'hotkey', accelerator: lastAccelerator };
  }

  /* --------------------------------------------------------------- capture */

  function captureFor(kind: WorkflowCaptureKind): WorkflowCapture {
    switch (kind) {
      case 'monitor':
        return {
          type: 'monitor',
          id: monitors.find((m) => m.isPrimary)?.id ?? monitors[0]?.id ?? 0
        };
      case 'window':
        return { type: 'window', id: 0 };
      case 'scrolling':
        return { type: 'scrolling', window: 0 };
      case 'record':
        return { type: 'record', format: DEFAULT_RECORD_FORMAT };
      default:
        return { type: kind };
    }
  }

  function setCaptureKind(kind: WorkflowCaptureKind): void {
    if (draft.capture.type === kind) return;
    draft.capture = captureFor(kind);
  }

  function setSourceId(id: number): void {
    const capture = draft.capture;
    if (capture.type === 'monitor') draft.capture = { type: 'monitor', id };
    else if (capture.type === 'window') draft.capture = { type: 'window', id };
    else if (capture.type === 'scrolling') draft.capture = { type: 'scrolling', window: id };
  }

  function setRecordFormat(format: string): void {
    draft.capture = { type: 'record', format };
  }

  /* --------------------------------------------------------------- actions */

  /** Whether the chain already has this step. Each one earns its place once. */
  function hasAction(kind: WorkflowActionKind): boolean {
    return draft.actions.some((action) => action.type === kind);
  }

  function addAction(kind: WorkflowActionKind): void {
    if (hasAction(kind)) return;
    draft.actions = [...draft.actions, kind === 'ocr' ? { type: 'ocr', copyText: true } : { type: kind }];
  }

  function removeAction(index: number): void {
    draft.actions = draft.actions.filter((_, i) => i !== index);
  }

  function moveAction(index: number, delta: number): void {
    const target = index + delta;
    if (target < 0 || target >= draft.actions.length) return;
    const next = [...draft.actions];
    const [moved] = next.splice(index, 1);
    next.splice(target, 0, moved);
    draft.actions = next;
  }

  function setOcrCopy(index: number, copyText: boolean): void {
    draft.actions = draft.actions.map((action, i) =>
      i === index && action.type === 'ocr' ? { type: 'ocr', copyText } : action
    );
  }

  function actionName(action: WorkflowAction): string {
    return WORKFLOW_ACTION_NAME[action.type] ?? action.type;
  }

  /* ----------------------------------------------------------- destination */

  function setDestination(choice: DestinationChoice): void {
    draft.destination = choice === 'none' ? null : choice;
  }

  function destinationStatusFor(choice: DestinationChoice): DestinationStatus | null {
    if (choice === 'none') return null;
    return destinations?.find((entry) => entry.kind === choice) ?? null;
  }

  /**
   * The card to draw as selected, or `null` when the workflow names a
   * destination this build has no uploader for — no card is right then, and the
   * error above the form is what explains it.
   */
  const chosenDestination = $derived<DestinationChoice | null>(
    workflowUploads(draft) ? workflowDestinationOf(draft.destination) : 'none'
  );

  function submit(): void {
    if (!canSave) return;
    onsave({ ...clone(draft), name: draft.name.trim() });
  }
</script>

<svelte:window onkeydown={onWindowKeydown} onkeyup={onWindowKeyup} />

<section class="editor" aria-label={isNew ? 'New workflow' : `Edit ${workflow.name || 'workflow'}`}>
  <header class="head">
    <h2 class="title">{isNew ? 'New workflow' : 'Edit workflow'}</h2>
    <div class="head-actions">
      <button type="button" class="btn" onclick={oncancel}>Cancel</button>
      <button type="button" class="btn primary" disabled={!canSave} onclick={submit}>
        {isNew ? 'Create workflow' : 'Save changes'}
      </button>
    </div>
  </header>

  <!-- The same line the list shows, live. It is the one place the whole chain
       is legible at once, and seeing it change as steps are added is what makes
       the list's summary trustworthy later. -->
  <div class="preview" aria-label="What this workflow does">
    {#each chain.shown as segment, i (i)}
      {#if i > 0}<span class="arrow" aria-hidden="true">→</span>{/if}
      <span class="seg" class:uploads={segment.uploads}>
        {#if segment.uploads}<Icon name="upload" size={12} />{/if}
        {segment.text}
      </span>
    {/each}
  </div>

  {#if errors.length > 0 || localErrors.length > 0 || warnings.length > 0}
    <ul class="issues">
      {#each localErrors as message (message)}
        <li class="issue error">
          <Icon name="alert" size={14} />
          <span>{message}</span>
        </li>
      {/each}
      {#each errors as issue (issue.message)}
        <li class="issue error">
          <Icon name="alert" size={14} />
          <span>{issue.message}</span>
          {#if issue.savable && onopendestinations}
            <button type="button" class="btn subtle" onclick={onopendestinations}>
              Open Destinations
            </button>
          {/if}
        </li>
      {/each}
      {#each warnings as issue (issue.message)}
        <li class="issue warn">
          <Icon name="info" size={14} />
          <span>{issue.message}</span>
        </li>
      {/each}
    </ul>
  {/if}

  <div class="field">
    <label class="field-label" for="wf-name">Name</label>
    <input
      id="wf-name"
      class="input"
      type="text"
      placeholder="Redact and share"
      autocomplete="off"
      spellcheck="false"
      bind:value={draft.name}
    />
    <p class="field-help">What the hotkey list and every toast about this workflow will call it.</p>
  </div>

  <div class="field">
    <span class="field-label" id="wf-trigger">Trigger</span>
    <div class="segmented" role="radiogroup" aria-labelledby="wf-trigger">
      <button
        type="button"
        class="seg-btn"
        class:on={draft.trigger.type === 'hotkey'}
        role="radio"
        aria-checked={draft.trigger.type === 'hotkey'}
        onclick={() => setTriggerKind('hotkey')}
      >
        <Icon name="keyboard" size={14} />
        Hotkey
      </button>
      <button
        type="button"
        class="seg-btn"
        class:on={draft.trigger.type === 'manual'}
        role="radio"
        aria-checked={draft.trigger.type === 'manual'}
        onclick={() => setTriggerKind('manual')}
      >
        <Icon name="cursor" size={14} />
        Run manually
      </button>
    </div>

    {#if draft.trigger.type === 'hotkey'}
      <div class="hotkey-row">
        <button
          type="button"
          class="hotkey"
          class:recording={recordingHotkey}
          aria-label="Trigger shortcut"
          onclick={() => (recordingHotkey = !recordingHotkey)}
          onblur={() => (recordingHotkey = false)}
        >
          {#if recordingHotkey}
            <span class="listening">Press keys…</span>
          {:else}
            <span class="keys">
              {#each displayAccelerator(triggerAccelerator) as part, i (i)}
                <kbd>{part}</kbd>
              {/each}
            </span>
          {/if}
        </button>
        <p class="field-help">
          Click and press the combination you want. Escape cancels. It is registered exactly like
          the built-in capture hotkeys, keyboard-hook fallback included.
        </p>
      </div>
    {:else}
      <p class="field-help">
        No shortcut. The workflow runs only from the Run now button on its row.
      </p>
    {/if}
  </div>

  <div class="field">
    <span class="field-label" id="wf-capture">Capture</span>
    <div class="cards" role="radiogroup" aria-labelledby="wf-capture">
      {#each WORKFLOW_CAPTURE_KINDS as kind (kind)}
        <button
          type="button"
          class="card"
          class:on={captureKind === kind}
          role="radio"
          aria-checked={captureKind === kind}
          onclick={() => setCaptureKind(kind)}
        >
          <Icon name={CAPTURE_ICON[kind]} size={15} />
          <span class="card-text">
            <span class="card-name">{WORKFLOW_CAPTURE_NAME[kind]}</span>
            <span class="card-help">{CAPTURE_HELP[kind]}</span>
          </span>
        </button>
      {/each}
    </div>

    {#if needsMonitors}
      <div class="sub">
        <label class="field-label" for="wf-monitor">Display</label>
        <select
          id="wf-monitor"
          class="select"
          value={String(sourceId)}
          onchange={(e) => setSourceId(Number(e.currentTarget.value))}
        >
          <option value="0" disabled>Choose a display…</option>
          {#each monitors as monitor (monitor.id)}
            <option value={String(monitor.id)}>
              {monitor.name} — {monitor.width}×{monitor.height}{monitor.isPrimary ? ' (primary)' : ''}
            </option>
          {/each}
        </select>
      </div>
    {:else if needsWindows}
      <div class="sub">
        <label class="field-label" for="wf-window">Window</label>
        <select
          id="wf-window"
          class="select"
          value={String(sourceId)}
          onchange={(e) => setSourceId(Number(e.currentTarget.value))}
        >
          <option value="0" disabled>Choose a window…</option>
          {#each windows as win (win.id)}
            <option value={String(win.id)}>{win.appName} — {win.title}</option>
          {/each}
        </select>
        <p class="field-help">
          Remembered by the window's id, so a workflow keeps working while that window stays open and
          reports a failure once it is gone.
        </p>
      </div>
    {:else if captureKind === 'record'}
      <div class="sub">
        <span class="field-label" id="wf-format">Format</span>
        <div class="segmented" role="radiogroup" aria-labelledby="wf-format">
          {#each RECORD_FORMATS as option (option)}
            <button
              type="button"
              class="seg-btn"
              class:on={recordFormat === option}
              role="radio"
              aria-checked={recordFormat === option}
              onclick={() => setRecordFormat(option)}
            >
              {RECORD_FORMAT_LABEL[option]}
            </button>
          {/each}
        </div>
      </div>
    {/if}

    {#if sourceMissing}
      <p class="field-note">
        That {needsMonitors ? 'display' : 'window'} is not on the list right now. The workflow keeps
        the choice, and will say so if it cannot find it when it runs.
      </p>
    {/if}
    {#if sourcesError}
      <p class="field-note">{sourcesError}</p>
    {/if}
  </div>

  <div class="field">
    <span class="field-label" id="wf-actions">Steps</span>
    <p class="field-help">
      Run in this order, straight after the capture and before the upload. Reorder them — the order
      is the difference between saving the annotated picture and saving the one without it.
    </p>

    {#if draft.actions.length === 0}
      <p class="field-note">No steps. The capture lands in history and the workflow ends.</p>
    {:else}
      <ol class="steps" aria-labelledby="wf-actions">
        {#each draft.actions as action, index (action.type)}
          <li class="step">
            <span class="step-n tnum">{index + 1}</span>
            <Icon name={ACTION_ICON[action.type] ?? 'check'} size={14} />
            <span class="step-text">
              <span class="step-name">{actionName(action)}</span>
              {#if action.type === 'ocr'}
                <label class="check">
                  <input
                    type="checkbox"
                    checked={action.copyText}
                    onchange={(e) => setOcrCopy(index, e.currentTarget.checked)}
                  />
                  <span>Copy the recognised text to the clipboard</span>
                </label>
              {:else}
                <span class="step-help">{WORKFLOW_ACTION_HELP[action.type] ?? ''}</span>
              {/if}
            </span>
            <span class="step-tools">
              <button
                type="button"
                class="icon-btn"
                aria-label="Move {actionName(action)} earlier"
                disabled={index === 0}
                onclick={() => moveAction(index, -1)}
              >
                <span class="up"><Icon name="chevron-down" size={14} /></span>
              </button>
              <button
                type="button"
                class="icon-btn"
                aria-label="Move {actionName(action)} later"
                disabled={index === draft.actions.length - 1}
                onclick={() => moveAction(index, 1)}
              >
                <Icon name="chevron-down" size={14} />
              </button>
              <button
                type="button"
                class="icon-btn danger"
                aria-label="Remove {actionName(action)}"
                onclick={() => removeAction(index)}
              >
                <Icon name="x" size={14} />
              </button>
            </span>
          </li>
        {/each}
      </ol>
    {/if}

    <div class="add">
      {#each WORKFLOW_ACTION_KINDS as kind (kind)}
        <button
          type="button"
          class="btn subtle"
          disabled={hasAction(kind)}
          title={hasAction(kind) ? 'Already in this workflow' : WORKFLOW_ACTION_HELP[kind]}
          onclick={() => addAction(kind)}
        >
          <Icon name="plus" size={13} />
          {WORKFLOW_ACTION_NAME[kind]}
        </button>
      {/each}
    </div>
  </div>

  <div class="field">
    <span class="field-label" id="wf-destination">Destination</span>
    <p class="field-help">
      Where the capture goes when every step has run. Naming one here is your consent for this
      workflow to upload — it does not turn on uploading anywhere else.
    </p>
    <div class="cards" role="radiogroup" aria-labelledby="wf-destination">
      <button
        type="button"
        class="card"
        class:on={chosenDestination === 'none'}
        role="radio"
        aria-checked={chosenDestination === 'none'}
        onclick={() => setDestination('none')}
      >
        <Icon name="check" size={15} />
        <span class="card-text">
          <span class="card-name">{DESTINATION_LABEL.none}</span>
          <span class="card-help">Nothing leaves this machine.</span>
        </span>
      </button>
      {#each DESTINATION_KINDS as kind (kind)}
        {@const status = destinationStatusFor(kind)}
        <button
          type="button"
          class="card"
          class:on={chosenDestination === kind}
          role="radio"
          aria-checked={chosenDestination === kind}
          onclick={() => setDestination(kind)}
        >
          <Icon name="cloud" size={15} />
          <span class="card-text">
            <span class="card-name">{status?.label ?? DESTINATION_LABEL[kind]}</span>
            <span class="card-help">
              {#if !destinations}
                Reading its setup…
              {:else if status?.configured}
                Set up and ready.
              {:else}
                Not set up yet — a workflow cannot upload to it.
              {/if}
            </span>
          </span>
          {#if destinations && !status?.configured}
            <span class="chip">Not configured</span>
          {/if}
        </button>
      {/each}
    </div>
  </div>
</section>

<style>
  .editor {
    display: flex;
    flex-direction: column;
    gap: 20px;
    max-width: 720px;
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
  }

  .title {
    margin: 0;
    font-family: var(--font-display);
    font-size: 15px;
    font-weight: 600;
    color: var(--text);
  }

  .head-actions {
    display: flex;
    gap: 8px;
  }

  /* The chain, live. Wraps rather than truncates: there is room here, and the
     editor is the one place the whole thing should be readable at once. */
  .preview {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 6px;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-inset);
    font-size: 13px;
    color: var(--text);
  }

  .arrow {
    color: var(--text-faint);
  }

  .seg {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }

  /* The destination is the only segment with consequences off this machine, so
     it is the only one that is coloured — and it carries a glyph too, because
     colour alone is not a message. */
  .seg.uploads {
    color: var(--accent);
    font-weight: 600;
  }

  .issues {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .issue {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 12px;
    line-height: 1.5;
    color: var(--text-dim);
  }

  .issue :global(svg) {
    margin-top: 1px;
  }

  .issue.error {
    color: var(--danger);
    border-color: color-mix(in srgb, var(--danger) 45%, var(--border));
    background: color-mix(in srgb, var(--danger) 8%, transparent);
  }

  .issue.warn {
    border-color: var(--border-strong);
    background: var(--bg-inset);
  }

  .issue .btn {
    margin-left: auto;
    flex: none;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .field-label {
    font-size: 12px;
    font-weight: 600;
    color: var(--text);
  }

  .field-help,
  .field-note {
    margin: 0;
    max-width: 62ch;
    font-size: 12px;
    line-height: 1.5;
    color: var(--text-faint);
  }

  .field-note {
    color: var(--text-dim);
  }

  .input,
  .select {
    height: 34px;
    padding: 0 10px;
    font: inherit;
    font-size: 13px;
    color: var(--text);
    background: var(--bg-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .input {
    max-width: 360px;
  }

  .select {
    max-width: 460px;
    cursor: pointer;
  }

  .input:hover,
  .select:hover {
    border-color: var(--border-strong);
  }

  .input:focus-visible,
  .select:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
    border-color: var(--accent);
  }

  .segmented {
    display: inline-flex;
    align-self: flex-start;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow: hidden;
  }

  .seg-btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 32px;
    padding: 0 12px;
    border: 0;
    background: var(--bg-inset);
    font: inherit;
    font-size: 12px;
    color: var(--text-dim);
    cursor: pointer;
    transition:
      background-color 140ms ease,
      color 140ms ease;
  }

  .seg-btn + .seg-btn {
    border-left: 1px solid var(--border);
  }

  .seg-btn:hover {
    color: var(--text);
  }

  .seg-btn.on {
    background: var(--surface);
    color: var(--text);
    font-weight: 600;
  }

  .seg-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }

  .hotkey-row {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    flex-wrap: wrap;
  }

  .hotkey {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 168px;
    height: 34px;
    padding: 0 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-inset);
    font: inherit;
    font-size: 12px;
    color: var(--text);
    cursor: pointer;
  }

  .hotkey:hover {
    border-color: var(--border-strong);
  }

  .hotkey:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .hotkey.recording {
    border-color: var(--accent);
    color: var(--accent);
  }

  .listening {
    color: var(--accent);
  }

  .keys {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }

  kbd {
    padding: 2px 6px;
    border: 1px solid var(--border-strong);
    border-radius: 5px;
    background: var(--surface);
    font: inherit;
    font-size: 11px;
    color: var(--text);
  }

  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
    gap: 8px;
  }

  .card {
    display: flex;
    align-items: flex-start;
    gap: 9px;
    padding: 9px 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-inset);
    font: inherit;
    color: var(--text-dim);
    text-align: left;
    cursor: pointer;
    transition:
      border-color 140ms ease,
      background-color 140ms ease,
      color 140ms ease;
  }

  .card :global(svg) {
    margin-top: 1px;
  }

  .card:hover {
    border-color: var(--border-strong);
    color: var(--text);
  }

  .card.on {
    border-color: var(--accent);
    background: var(--surface);
    color: var(--text);
  }

  .card:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .card-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .card-name {
    font-size: 12px;
    font-weight: 600;
    color: var(--text);
  }

  .card-help {
    font-size: 11px;
    line-height: 1.45;
    color: var(--text-faint);
  }

  .chip {
    flex: none;
    margin-left: auto;
    padding: 1px 6px;
    border: 1px solid color-mix(in srgb, var(--danger) 50%, var(--border));
    border-radius: 5px;
    font-size: 10px;
    color: var(--danger);
  }

  .sub {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .steps {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .step {
    display: flex;
    align-items: flex-start;
    gap: 9px;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-inset);
    color: var(--text-dim);
  }

  .step :global(svg) {
    margin-top: 1px;
  }

  .step-n {
    flex: none;
    width: 18px;
    font-size: 11px;
    color: var(--text-faint);
  }

  .tnum {
    font-variant-numeric: tabular-nums;
  }

  .step-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    flex: 1;
  }

  .step-name {
    font-size: 12px;
    font-weight: 600;
    color: var(--text);
  }

  .step-help {
    font-size: 11px;
    line-height: 1.45;
    color: var(--text-faint);
  }

  .check {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    color: var(--text-dim);
    cursor: pointer;
  }

  .check input {
    width: 13px;
    height: 13px;
    accent-color: var(--accent);
    cursor: pointer;
  }

  .step-tools {
    display: flex;
    align-items: center;
    gap: 2px;
    margin-left: auto;
  }

  .icon-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 26px;
    height: 26px;
    padding: 0;
    border: 0;
    border-radius: var(--radius);
    background: none;
    color: var(--text-faint);
    cursor: pointer;
    transition:
      background-color 120ms ease,
      color 120ms ease;
  }

  .icon-btn:hover:not(:disabled) {
    background: var(--surface);
    color: var(--text);
  }

  .icon-btn.danger:hover:not(:disabled) {
    color: var(--danger);
  }

  .icon-btn:disabled {
    opacity: 0.35;
    cursor: not-allowed;
  }

  .icon-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }

  /* The set has a chevron-down and no chevron-up; one glyph turned over is
     better than a second nearly-identical path in the icon table. */
  .up {
    display: flex;
    transform: rotate(180deg);
  }

  .add {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 32px;
    padding: 0 12px;
    font: inherit;
    font-size: 12px;
    color: var(--text);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    cursor: pointer;
    transition:
      background-color 140ms ease,
      border-color 140ms ease,
      color 140ms ease;
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

  .btn.subtle {
    background: none;
    color: var(--text-dim);
  }

  .btn.primary {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--accent-text);
    font-weight: 600;
  }

  .btn.primary:hover:not(:disabled) {
    background: var(--accent-hover);
    border-color: var(--accent-hover);
  }
</style>
