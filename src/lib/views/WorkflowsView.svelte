<script lang="ts">
  /**
   * Workflows (M6 §4): every chain the user has built, and what each one does.
   *
   * The row's **summary line is the feature**, not decoration. A user has to be
   * able to tell what a workflow does without opening it, because that is what
   * stops them binding a hotkey that uploads their screen when they meant it to
   * save locally. So it names every step, it marks the destination as the one
   * part with consequences off this machine, and when it does not fit it is
   * truncated in the *middle* — the destination survives whatever else goes.
   *
   * Validation is the row's other job. `workflowIssues()` runs against the same
   * rules the editor uses, so a workflow that would fail is loud here too rather
   * than only inside the form nobody has open.
   *
   * M6 adds no new capability: every step a workflow runs already exists, and
   * this screen only composes and names them.
   */
  import Icon from '$lib/components/Icon.svelte';
  import Toggle from '$lib/components/Toggle.svelte';
  import { toast } from '$lib/components/Toast.svelte';
  import WorkflowEditor from '$lib/workflows/WorkflowEditor.svelte';
  import { settings } from '$lib/stores/settings.svelte';
  import {
    cancelWorkflow,
    destinationStatus,
    displayAccelerator,
    errorMessage,
    getHotkeyStatus,
    getWorkflows,
    onHotkeyStatus,
    onWorkflowDone,
    onWorkflowError,
    onWorkflowProgress,
    onWorkflowsChanged,
    runWorkflow,
    saveWorkflows,
    workflowCanEnable,
    workflowChainLine,
    workflowHotkeyAction,
    workflowIssues,
    workflowStepLabel,
    type DestinationStatus,
    type HotkeyMechanism,
    type HotkeyStatus,
    type Workflow,
    type WorkflowIssue,
    type WorkflowProgress
  } from '$lib/api';

  interface Props {
    /**
     * Route to the Destinations screen. Each shell knows where its own lives —
     * a pane of its own in the ShareX shell, a section of Settings in the other
     * — so neither is hard-coded here.
     */
    onopendestinations?: () => void;
  }

  let { onopendestinations }: Props = $props();

  /** The same three chips Settings shows, worded identically (M2.6 §1). */
  const MECHANISM_META: Record<HotkeyMechanism, { label: string; title: string }> = {
    plugin: {
      label: 'Bound',
      title: 'Registered with Windows the ordinary way (RegisterHotKey).'
    },
    hook: {
      label: 'Bound via hook',
      title:
        'Another app already owns this combination, so santi.sharex claims it with the low-level keyboard hook.'
    },
    none: {
      label: 'Not bound',
      title: 'Nothing happens when you press this combination.'
    }
  };

  let workflows = $state<Workflow[]>([]);
  let loaded = $state(false);
  let loadError = $state('');
  let destinations = $state<DestinationStatus[] | null>(null);
  let hotkeyStatus = $state<HotkeyStatus[]>([]);

  /** The draft being edited, or `null` while the list is showing. */
  let editing = $state<{ workflow: Workflow; isNew: boolean } | null>(null);

  /** The run in flight, from `workflow://progress`. `null` when nothing runs. */
  let running = $state<WorkflowProgress | null>(null);

  /** Which row has a round trip out, so exactly one control is busy. */
  let busy = $state('');
  let confirmDelete = $state('');

  const s = $derived(settings.current);
  const runningWorkflow = $derived(workflows.find((w) => w.id === running?.id) ?? null);

  $effect(() => {
    void load();
  });

  $effect(() => {
    let cancelled = false;
    const stops: (() => void)[] = [];
    const track = (promise: Promise<() => void>) => {
      promise
        .then((un) => (cancelled ? un() : stops.push(un)))
        .catch(() => {});
    };

    track(onWorkflowsChanged((list) => (workflows = list)));
    track(onHotkeyStatus((list) => (hotkeyStatus = list)));
    track(onWorkflowProgress((progress) => (running = progress)));
    track(
      onWorkflowDone((done) => {
        if (running?.id === done.id || done.id === '') running = null;
        // A cancelled run is not a failure: the user backed out of the region
        // overlay or the editor, which M6 §2 ends the chain cleanly for. A red
        // toast for something they just asked for is a bug.
        if (done.cancelled) toast.info(done.message || 'Workflow cancelled.');
        else if (done.message) toast.success(done.message);
      })
    );
    track(
      onWorkflowError((error) => {
        if (running?.id === error.id || error.id === '') running = null;
        // Named step first: "workflow failed" leaves the user guessing whether
        // their capture was taken, saved, or sent.
        toast.error(`${workflowStepLabel(error.step)} failed — ${error.message}`);
      })
    );

    return () => {
      cancelled = true;
      for (const stop of stops) stop();
    };
  });

  async function load(): Promise<void> {
    loadError = '';
    try {
      workflows = await getWorkflows();
    } catch (err) {
      loadError = errorMessage(err);
    } finally {
      loaded = true;
    }
    // Both are advisory: a failure leaves the chips absent and the destination
    // check silent, which reads as "not known yet" rather than asserting
    // something that was never confirmed.
    try {
      destinations = await destinationStatus();
    } catch {
      destinations = null;
    }
    try {
      hotkeyStatus = await getHotkeyStatus();
    } catch {
      /* leave the chips off */
    }
  }

  function issuesFor(workflow: Workflow): WorkflowIssue[] {
    return workflowIssues(workflow, { workflows, destinations, settings: s });
  }

  /**
   * The registration report for a workflow's trigger, or `null` when there is
   * none to trust.
   *
   * Rust reports against the accelerator it registered, so between a rebind
   * being saved and it landing the old report describes the old combination.
   * Showing it then would paint a freshly typed shortcut with the last one's
   * fate — the same rule Settings' own table follows.
   */
  function statusFor(workflow: Workflow): HotkeyStatus | null {
    if (workflow.trigger.type !== 'hotkey') return null;
    const action = workflowHotkeyAction(workflow.id);
    const found = hotkeyStatus.find((entry) => entry.action === action);
    return found && found.accelerator === workflow.trigger.accelerator.trim() ? found : null;
  }

  /** Persist the whole list, adopting what Rust stored rather than the draft. */
  async function commit(next: Workflow[], key: string): Promise<boolean> {
    busy = key;
    const previous = workflows;
    workflows = next;
    try {
      workflows = await saveWorkflows(next);
      return true;
    } catch (err) {
      workflows = previous;
      toast.error(errorMessage(err));
      return false;
    } finally {
      busy = '';
    }
  }

  function newWorkflow(): Workflow {
    return {
      id: crypto.randomUUID(),
      name: '',
      enabled: true,
      trigger: { type: 'manual' },
      capture: { type: 'region' },
      actions: [{ type: 'saveToDisk' }],
      destination: null
    };
  }

  function startNew(): void {
    confirmDelete = '';
    editing = { workflow: newWorkflow(), isNew: true };
  }

  function startEdit(workflow: Workflow): void {
    confirmDelete = '';
    editing = { workflow, isNew: false };
  }

  async function saveDraft(draft: Workflow): Promise<void> {
    const exists = workflows.some((w) => w.id === draft.id);
    const next = exists
      ? workflows.map((w) => (w.id === draft.id ? draft : w))
      : [...workflows, draft];
    const ok = await commit(next, draft.id);
    if (ok) {
      editing = null;
      toast.success(exists ? 'Workflow saved' : `“${draft.name}” created`);
    }
  }

  async function duplicate(workflow: Workflow): Promise<void> {
    // The copy keeps every step and drops the trigger: two workflows on one
    // combination is exactly the collision the editor refuses, and silently
    // inheriting it would hand the user a conflict they never chose.
    const copy: Workflow = {
      ...workflow,
      id: crypto.randomUUID(),
      name: `${workflow.name} copy`,
      trigger: { type: 'manual' },
      capture: { ...workflow.capture },
      actions: workflow.actions.map((action) => ({ ...action }))
    };
    await commit([...workflows, copy], workflow.id);
  }

  async function remove(workflow: Workflow): Promise<void> {
    confirmDelete = '';
    const ok = await commit(
      workflows.filter((w) => w.id !== workflow.id),
      workflow.id
    );
    if (ok) toast.success(`“${workflow.name}” deleted`);
  }

  async function setEnabled(workflow: Workflow, enabled: boolean): Promise<void> {
    if (enabled) {
      const blocking = issuesFor(workflow).find((issue) => issue.level === 'error');
      if (blocking) {
        // Refused rather than saved-and-broken: a disabled workflow has no
        // hotkey registered, so there is nothing to press and nothing to fail.
        toast.error(blocking.message);
        return;
      }
    }
    await commit(
      workflows.map((w) => (w.id === workflow.id ? { ...w, enabled } : w)),
      workflow.id
    );
  }

  async function run(workflow: Workflow): Promise<void> {
    try {
      await runWorkflow(workflow.id);
    } catch (err) {
      toast.error(errorMessage(err));
    }
  }

  async function stopRun(): Promise<void> {
    try {
      await cancelWorkflow();
    } catch (err) {
      toast.error(errorMessage(err));
    }
  }
</script>

<div class="page">
  <header class="page-header">
    <div class="heading">
      <h1 class="page-title">Workflows</h1>
      <p class="stats">
        {#if editing}
          One chain of steps, bound to one shortcut.
        {:else}
          <span class="num">{workflows.length}</span>
          {workflows.length === 1 ? 'workflow' : 'workflows'}
        {/if}
      </p>
    </div>

    {#if !editing}
      <button type="button" class="btn primary" onclick={startNew}>
        <Icon name="plus" size={14} />
        New workflow
      </button>
    {/if}
  </header>

  {#if running}
    <!-- Live for the whole run, wherever it was started from — a chain that
         goes quiet for eight seconds looks broken, and a Cancel that is not on
         screen is a Cancel that does not exist. -->
    <div class="run" role="status">
      <span class="run-text">
        <span class="run-name">{runningWorkflow?.name || 'Workflow'}</span>
        <span class="run-step">
          Step <span class="num">{Math.min(running.index + 1, running.total)}</span>
          of <span class="num">{running.total}</span> — {workflowStepLabel(running.step)}
        </span>
      </span>
      <span
        class="bar"
        role="progressbar"
        aria-valuemin="0"
        aria-valuemax={running.total}
        aria-valuenow={running.index + 1}
        aria-label="Workflow progress"
      >
        <span
          class="bar-fill"
          style="width: {running.total > 0
            ? Math.round((Math.min(running.index + 1, running.total) / running.total) * 100)
            : 0}%"
        ></span>
      </span>
      <button type="button" class="btn" onclick={stopRun}>Cancel</button>
    </div>
  {/if}

  {#if editing}
    <!-- Keyed so switching straight from editing one workflow to another really
         remounts. The editor seeds its draft once from this prop on purpose, so
         a reused instance would keep the previous workflow's draft and silently
         save it over the one the user thinks they opened. -->
    {#key editing.workflow.id}
      <WorkflowEditor
        workflow={editing.workflow}
        isNew={editing.isNew}
        {workflows}
        {destinations}
        settings={s}
        {onopendestinations}
        onsave={saveDraft}
        oncancel={() => (editing = null)}
      />
    {/key}
  {:else if loadError}
    <p class="err">{loadError}</p>
  {:else if !loaded}
    <p class="loading">Loading workflows…</p>
  {:else if workflows.length === 0}
    <div class="empty">
      <Icon name="workflow" size={26} />
      <h2>No workflows yet</h2>
      <p>
        A workflow chains a capture, the steps that follow it and where it ends up, and binds the
        whole thing to one shortcut — freeze a region, annotate it, save it, upload it, with one
        key. Every step is one santi.sharex already does; a workflow just puts them in order.
      </p>
      <button type="button" class="btn" onclick={startNew}>Build one</button>
    </div>
  {:else}
    <ul class="list">
      {#each workflows as workflow (workflow.id)}
        {@const line = workflowChainLine(workflow)}
        {@const issues = issuesFor(workflow)}
        {@const status = statusFor(workflow)}
        {@const blocked = !workflowCanEnable(issues)}
        {@const active = running?.id === workflow.id}
        <li class="row" class:active>
          <div class="row-main">
            <Toggle
              checked={workflow.enabled && !blocked}
              labelledBy="wf-name-{workflow.id}"
              disabled={busy === workflow.id}
              onchange={(v) => setEnabled(workflow, v)}
            />

            <div class="row-text">
              <div class="name-line">
                <span class="name" id="wf-name-{workflow.id}">{workflow.name || 'Untitled'}</span>
                {#if workflow.trigger.type === 'hotkey'}
                  <span class="keys">
                    {#each displayAccelerator(workflow.trigger.accelerator) as part, i (i)}
                      <kbd>{part}</kbd>
                    {/each}
                  </span>
                  {#if status}
                    <span
                      class="mech"
                      class:hook={status.mechanism === 'hook'}
                      class:unbound={!status.bound}
                      title={status.error ?? MECHANISM_META[status.mechanism].title}
                    >
                      {MECHANISM_META[status.mechanism].label}
                    </span>
                  {/if}
                {:else}
                  <span class="manual">Run manually</span>
                {/if}
              </div>

              <!-- The summary. Middle-truncated so the destination survives. -->
              <p class="chain" title={line.full}>
                {#each line.shown as segment, i (i)}
                  {#if i > 0}<span class="arrow" aria-hidden="true">→</span>{/if}
                  {#if i === 1 && line.hidden > 0}
                    <span class="more" title="{line.hidden} more steps in between">
                      +{line.hidden}
                    </span>
                    <span class="arrow" aria-hidden="true">→</span>
                  {/if}
                  <span class="seg" class:uploads={segment.uploads}>
                    {#if segment.uploads}<Icon name="upload" size={11} />{/if}
                    {segment.text}
                  </span>
                {/each}
              </p>

              {#each issues as issue (issue.message)}
                <p class="issue" class:error={issue.level === 'error'}>
                  <Icon name={issue.level === 'error' ? 'alert' : 'info'} size={13} />
                  <span>{issue.message}</span>
                </p>
              {/each}
            </div>

            <div class="row-tools">
              <button
                type="button"
                class="btn"
                disabled={running !== null || blocked || busy === workflow.id}
                title={blocked
                  ? 'Fix what is wrong with this workflow first.'
                  : running !== null
                    ? 'A workflow is already running. They run one at a time.'
                    : 'Run this workflow now, exactly as its shortcut would.'}
                onclick={() => run(workflow)}
              >
                Run now
              </button>
              <button type="button" class="btn subtle" onclick={() => startEdit(workflow)}>
                Edit
              </button>
              <button
                type="button"
                class="icon-btn"
                aria-label="Duplicate {workflow.name || 'workflow'}"
                title="Duplicate"
                disabled={busy === workflow.id}
                onclick={() => duplicate(workflow)}
              >
                <Icon name="copy" size={14} />
              </button>
              <button
                type="button"
                class="icon-btn danger"
                aria-label="Delete {workflow.name || 'workflow'}"
                title="Delete"
                aria-expanded={confirmDelete === workflow.id}
                disabled={busy === workflow.id}
                onclick={() =>
                  (confirmDelete = confirmDelete === workflow.id ? '' : workflow.id)}
              >
                <Icon name="trash" size={14} />
              </button>
            </div>
          </div>

          {#if confirmDelete === workflow.id}
            <div class="confirm">
              <span>Delete “{workflow.name || 'Untitled'}”? Its shortcut is released too.</span>
              <button type="button" class="btn subtle" onclick={() => (confirmDelete = '')}>
                Keep it
              </button>
              <button type="button" class="btn danger" onclick={() => remove(workflow)}>
                Delete
              </button>
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .page {
    display: flex;
    flex-direction: column;
    gap: 16px;
    padding: 0 32px 40px;
  }

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

  .loading,
  .err {
    margin: 0;
    font-size: 13px;
    color: var(--text-dim);
  }

  .err {
    color: var(--danger);
  }

  /* The run banner. Sits above the list rather than inside the running row, so
     it is in the same place whether the run came from a button here or from a
     hotkey pressed while this window was hidden. */
  .run {
    display: flex;
    align-items: center;
    gap: 14px;
    flex-wrap: wrap;
    padding: 10px 14px;
    border: 1px solid var(--accent);
    border-radius: var(--radius-lg);
    background: color-mix(in srgb, var(--accent) 8%, var(--surface));
  }

  .run-text {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }

  .run-name {
    font-size: 13px;
    font-weight: 600;
    color: var(--text);
  }

  .run-step {
    font-size: 12px;
    color: var(--text-dim);
  }

  .bar {
    flex: 1;
    min-width: 120px;
    height: 5px;
    border-radius: 999px;
    background: var(--bg-inset);
    overflow: hidden;
  }

  .bar-fill {
    display: block;
    height: 100%;
    background: var(--accent);
    transition: width 180ms ease;
  }

  .list {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .row {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 12px 14px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--surface);
  }

  .row.active {
    border-color: var(--accent);
  }

  .row-main {
    display: flex;
    align-items: flex-start;
    gap: 12px;
  }

  .row-text {
    display: flex;
    flex-direction: column;
    gap: 5px;
    flex: 1;
    min-width: 0;
  }

  .name-line {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }

  .name {
    font-size: 13px;
    font-weight: 600;
    color: var(--text);
  }

  .keys {
    display: inline-flex;
    align-items: center;
    gap: 3px;
  }

  kbd {
    padding: 1px 5px;
    border: 1px solid var(--border-strong);
    border-radius: 5px;
    background: var(--bg-inset);
    font: inherit;
    font-size: 10px;
    color: var(--text-dim);
  }

  .manual {
    font-size: 11px;
    color: var(--text-faint);
  }

  /* Quiet for the two mechanisms that work, loud for the one that does not — a
     hotkey that silently does nothing is the confusion this chip exists to end.
     Worded exactly as Settings words it. */
  .mech {
    padding: 1px 6px;
    border: 1px solid var(--border);
    border-radius: 5px;
    font-size: 10px;
    color: var(--text-faint);
  }

  .mech.hook {
    color: var(--text-dim);
  }

  .mech.unbound {
    color: var(--danger);
    border-color: color-mix(in srgb, var(--danger) 50%, var(--border));
  }

  /* One line, never wrapped: the summary earns its place by being scannable
     down the column, and a chain that reflows to three lines is not. */
  .chain {
    display: flex;
    align-items: center;
    gap: 5px;
    margin: 0;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    font-size: 12px;
    color: var(--text-dim);
  }

  .seg {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex: none;
  }

  /* The destination — the one segment with consequences off this machine. It
     carries a glyph as well as the colour, because colour alone is not a
     message and this is the segment the whole line exists for. */
  .seg.uploads {
    color: var(--accent);
    font-weight: 600;
  }

  .arrow {
    flex: none;
    color: var(--text-faint);
  }

  /* What the middle of the chain was folded into. Deliberately not the tail:
     the destination is the part with consequences and never gives way. */
  .more {
    flex: none;
    padding: 0 5px;
    border: 1px solid var(--border);
    border-radius: 5px;
    font-size: 10px;
    color: var(--text-faint);
  }

  .issue {
    display: flex;
    align-items: flex-start;
    gap: 6px;
    margin: 0;
    font-size: 11px;
    line-height: 1.5;
    color: var(--text-dim);
  }

  .issue.error {
    color: var(--danger);
  }

  .issue :global(svg) {
    margin-top: 2px;
    flex: none;
  }

  .row-tools {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: none;
  }

  .confirm {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
    padding: 8px 10px;
    border: 1px solid color-mix(in srgb, var(--danger) 45%, var(--border));
    border-radius: var(--radius);
    background: var(--bg-inset);
    font-size: 12px;
    color: var(--text-dim);
  }

  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    text-align: center;
    padding: 64px 24px;
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
    max-width: 440px;
    font-size: 12px;
    line-height: 1.6;
    color: var(--text-dim);
  }

  .empty .btn {
    margin-top: 8px;
  }

  .btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
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

  .btn.danger {
    color: var(--danger);
    border-color: color-mix(in srgb, var(--danger) 50%, var(--border));
  }

  .icon-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 30px;
    height: 30px;
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
    background: var(--bg-inset);
    color: var(--text);
  }

  .icon-btn.danger:hover:not(:disabled) {
    color: var(--danger);
  }

  .icon-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .icon-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
</style>
