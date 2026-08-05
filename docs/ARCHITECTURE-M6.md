# santi.sharex — M6: workflows

The last milestone. A workflow chains **capture → actions → destination** and
binds the whole chain to one hotkey, which is what ShareX's task system is for
and the reason its hotkey list has more than three rows.

Everything a workflow does already exists. M6 adds no new capability — it makes
the existing ones composable. That framing matters: if a step needs new
plumbing, the step is wrong, not the plumbing.

---

## 1. The shape

```rust
pub struct Workflow {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub trigger: Trigger,        // Hotkey { accelerator } | Manual
    pub capture: CaptureStep,    // Region | Fullscreen | ActiveWindow | Monitor { id }
                                 //   | Window { id } | Scrolling { window } | Record { format }
    pub actions: Vec<ActionStep>,
    pub destination: Option<String>,  // a destination id, or None to skip uploading
}

pub enum ActionStep {
    Annotate,                    // open the editor and WAIT for it
    SaveToDisk,
    CopyImage,
    Ocr { copy_text: bool },
    OpenFolder,
}
```

Stored in `workflows.json` beside `settings.json`, not inside it — a corrupt
workflow must not cost the user their hotkeys and save directory.

## 2. Execution

One runner, `src-tauri/src/workflow.rs`, driving steps in order:

1. **Capture** — call the existing capture path for the chosen kind. Region
   opens the overlay and waits for the commit; a cancelled region capture ends
   the workflow cleanly, not as an error.
2. **Actions** — in listed order. `Annotate` opens the editor and **blocks the
   rest of the chain until it is saved or cancelled**; cancelling ends the
   workflow without uploading. That wait is the whole point of the step, and
   getting it wrong means uploading the un-annotated image, which for a redaction
   workflow is a privacy failure, not a bug.
3. **Destination** — upload, subject to every M3 §1 rule. A workflow naming a
   destination is the user's explicit per-run consent, so it does not need
   `auto_upload`; but a workflow whose destination is unconfigured must fail
   loudly at edit time, not silently at run time.

Rules the runner must hold:

- **Sequential, never concurrent.** A second trigger while one is running is
  rejected with a toast, exactly like a second recording. Two overlapping region
  overlays is a lockout.
- **Every step reports.** `workflow://progress` `{ id, step, index, total }` and
  a terminal `workflow://done` / `workflow://error`. A chain that runs silently
  for eight seconds looks broken.
- **Cancellable at every step**, and cancelling never leaves the overlay, the
  editor or the recorder HUD on screen.
- A failing step **stops the chain** and says which step failed and why. It does
  not carry on to the destination with a half-finished result.

## 3. Hotkeys

A workflow with a `Hotkey` trigger registers through the existing M2.6 path —
`RegisterHotKey` first, `WH_KEYBOARD_LL` fallback, per-hotkey status reported.
Workflow hotkeys and the three built-in capture hotkeys share one registry:

- A workflow may not silently steal a built-in's combo. Conflicts are detected
  **at edit time**, naming what already owns it.
- `get_hotkey_status` grows to cover workflow bindings, so Settings › Hotkeys and
  the ShareX shell's hotkey table show them with the same Bound / Bound via hook
  / Not bound chips. The table stops being three fixed rows.
- Disabling a workflow unregisters its hotkey. Deleting one unregisters and
  removes it from the credential-free config.

## 4. UI

A **Workflows** view — the ShareX shell's `M6` row becomes real, and it joins the
default shell's nav.

- List of workflows: name, trigger, a one-line summary of the chain
  (`Region → Annotate → Save → Imgur`), enabled toggle, run-now button.
- An editor: name, trigger with the click-to-record hotkey control, capture kind,
  a reorderable action list, destination picker.
- **The summary line is the feature.** A user must be able to tell what a
  workflow does without opening it, because that is what stops them binding a
  hotkey that uploads their screen when they meant it to save locally.
- Validation at edit time, not run time: unconfigured destination, conflicting
  hotkey, `Ocr` on a `Record` capture (there is no text in an MP4), `Annotate`
  after `SaveToDisk` (the saved file would predate the annotation — either
  reorder or warn plainly).

## 5. What must not regress

Everything through M5, M3 and M4. In particular the overlay Escape ladder and
its arm/ready handshake — the runner drives the overlay programmatically now,
which is a new caller of a path that has exactly one today — and M3's upload
consent and credential rules, which a workflow must not become a way around.

`cargo check`, `cargo test`, `pnpm check`, `pnpm check:tokens`, `pnpm build`.
