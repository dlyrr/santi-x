<script lang="ts">
  /**
   * Upload destinations (M3 §§3–5): pick where captures go, and configure each
   * of the three.
   *
   * Two rules shape every control on this page.
   *
   * 1. **Nothing uploads unless asked.** Choosing a destination here does not
   *    upload anything — it only says where the explicit Upload action will
   *    send a capture. The one switch that changes that, `autoUpload`, lives in
   *    Settings behind a confirmation, not here.
   * 2. **Secret fields are write-only.** Every credential input below writes and
   *    is then emptied. This page learns *whether* a secret is set, from
   *    `destination_status()`, and shows "Configured" or "Not configured". It
   *    never learns a value, because no command returns one — `secrets::get` is
   *    `pub(in crate::upload)` and no `#[tauri::command]` can reach it. A
   *    settings page that can display the password is a settings page that
   *    leaks it to anyone standing behind you.
   *
   * The credential rows are built from `DestinationStatus.secrets` rather than
   * from a list typed out here: a custom uploader's slots are whichever headers
   * its `.sxcu` declared, so Rust is the only thing that can know them.
   *
   * Rendered in two places, which is why `variant` exists: as its own pane in
   * the ShareX shell, and as a section inside Settings for the shell whose
   * sidebar has no room for a fourth entry.
   */
  import Icon from '$lib/components/Icon.svelte';
  import Toggle from '$lib/components/Toggle.svelte';
  import { toast } from '$lib/components/Toast.svelte';
  import { settings } from '$lib/stores/settings.svelte';
  import {
    clearDestinationSecret,
    destinationStatus,
    errorMessage,
    importCustomUploader,
    setDestinationSecret,
    testDestination,
    DESTINATION_LABEL,
    EMPTY_CUSTOM_UPLOADER,
    FTP_DEFAULTS,
    FTP_SECURITY,
    ftpSecurityOf,
    IMGUR_REGISTER_URL,
    type DestinationChoice,
    type DestinationKind,
    type DestinationStatus,
    type FtpSettings,
    type SecretStatus,
    type Settings
  } from '$lib/api';

  interface Props {
    /**
     * `page` gives it a pinned header and its own padding, for the ShareX
     * shell's Destinations pane. `section` drops both, so Settings can drop it
     * straight into its own column.
     */
    variant?: 'page' | 'section';
  }

  let { variant = 'page' }: Props = $props();

  /** The picker's rows, in the order a user meets them. */
  const CHOICES: { id: DestinationChoice; name: string; blurb: string }[] = [
    {
      id: 'none',
      name: DESTINATION_LABEL.none,
      blurb: 'Captures stay on this machine. Nothing can be uploaded while this is selected.'
    },
    {
      id: 'imgur',
      name: DESTINATION_LABEL.imgur,
      blurb: 'Anonymous image hosting, using a Client ID you register yourself.'
    },
    {
      id: 'custom',
      name: DESTINATION_LABEL.custom,
      blurb: 'A ShareX .sxcu file — your own server, or any host you already have set up.'
    },
    {
      id: 'ftp',
      name: 'FTP / FTPS',
      blurb: 'Write the file into a directory on your own server. SFTP is a different protocol and is not supported.'
    }
  ];

  const s = $derived(settings.current);
  const active = $derived<DestinationChoice>(s?.destination ?? 'none');

  let status = $state<DestinationStatus[] | null>(null);
  let statusError = $state('');

  /**
   * The one credential being typed, and its value. Deliberately singular: only
   * one plaintext secret can exist in this window at a time, and it is dropped
   * the instant the write lands or the user backs out.
   */
  let editing = $state<{ kind: DestinationKind; field: string; label: string } | null>(null);
  let secretDraft = $state('');

  /** Key of whatever round trip is in flight, so exactly one control is busy. */
  let busy = $state('');

  type TestResult = { ok: boolean; text: string };
  let tested = $state<Partial<Record<DestinationKind, TestResult>>>({});

  function setTested(kind: DestinationKind, value: TestResult | undefined): void {
    const next: Partial<Record<DestinationKind, TestResult>> = { ...tested };
    next[kind] = value;
    tested = next;
  }

  /**
   * Why an imported `.sxcu` was refused, exactly as Rust worded it. Shown
   * verbatim — every rejection names the field that is not supported, and a
   * paraphrase would throw away the only thing that explains the refusal.
   */
  let importError = $state('');

  let ftpDraft = $state<FtpSettings>({ ...FTP_DEFAULTS });

  /**
   * The transfer mode the *stored* settings actually describe, or `null` when
   * `security` holds something this build has no meaning for.
   *
   * Read through this rather than off `ftpDraft.security` directly, because
   * `null` is a state the form has to render: `ftp.rs` refuses an unknown mode
   * outright instead of falling back to plain, so drawing the toggle "off" and
   * warning about clear text would describe a connection that is never made.
   */
  const ftpMode = $derived(ftpSecurityOf(ftpDraft.security));

  // Same idiom as the Settings drafts: re-seeded from the store whenever a save
  // lands, so a rejected write rolls the form back along with the value.
  $effect(() => {
    if (s) ftpDraft = { ...FTP_DEFAULTS, ...s.ftp };
  });

  $effect(() => {
    void refreshStatus();
  });

  /**
   * The imported uploader, or `null`. Rust never sends `null` — the struct is
   * always there and an empty `requestUrl` is the "nothing imported" state — so
   * that is the test, not the object's existence.
   */
  const uploader = $derived.by(() => {
    const config = s?.customUploader;
    return config && config.requestUrl.trim() !== '' ? config : null;
  });

  function statusFor(id: DestinationChoice): DestinationStatus | null {
    if (id === 'none') return null;
    return status?.find((d) => d.kind === id) ?? null;
  }

  const activeStatus = $derived<DestinationStatus | null>(statusFor(active));

  async function refreshStatus(): Promise<void> {
    try {
      status = await destinationStatus();
      statusError = '';
    } catch (err) {
      statusError = errorMessage(err);
    }
  }

  /** The stores report failures through `error` rather than rejecting. */
  async function patch(delta: Partial<Settings>): Promise<boolean> {
    const ok = await settings.update(delta);
    if (!ok) toast.error(settings.error ?? 'Could not save settings');
    return ok;
  }

  async function choose(id: DestinationChoice): Promise<void> {
    if (id === active) return;
    if (await patch({ destination: id })) await refreshStatus();
  }

  function beginEdit(kind: DestinationKind, secret: SecretStatus): void {
    editing = { kind, field: secret.field, label: secret.label };
    secretDraft = '';
  }

  function cancelEdit(): void {
    editing = null;
    secretDraft = '';
  }

  function isEditing(kind: DestinationKind, field: string): boolean {
    return editing?.kind === kind && editing.field === field;
  }

  async function saveSecret(): Promise<void> {
    const target = editing;
    const value = secretDraft.trim();
    if (!target || value === '') return;
    busy = `secret:${target.kind}:${target.field}`;
    try {
      await setDestinationSecret(target.kind, target.field, value);
      toast.success(`${target.label} saved to Windows Credential Manager.`);
    } catch (err) {
      toast.error(errorMessage(err));
    } finally {
      // Dropped whether the write landed or not: a failed save is no reason to
      // leave a credential sitting in a reactive field.
      secretDraft = '';
      editing = null;
      busy = '';
      await refreshStatus();
    }
  }

  async function clearSecret(kind: DestinationKind, secret: SecretStatus): Promise<void> {
    busy = `secret:${kind}:${secret.field}`;
    try {
      await clearDestinationSecret(kind, secret.field);
      toast.success(`${secret.label} removed from Windows Credential Manager.`);
    } catch (err) {
      toast.error(errorMessage(err));
    } finally {
      busy = '';
      if (isEditing(kind, secret.field)) cancelEdit();
      await refreshStatus();
    }
  }

  async function runTest(kind: DestinationKind): Promise<void> {
    busy = `test:${kind}`;
    setTested(kind, undefined);
    try {
      const message = await testDestination(kind);
      setTested(kind, { ok: true, text: message || 'Connected.' });
    } catch (err) {
      // The server's own refusal, verbatim: "Test failed" says nothing the user
      // can act on, and Rust has already kept credentials out of the text.
      setTested(kind, { ok: false, text: errorMessage(err) });
    } finally {
      busy = '';
    }
  }

  /**
   * The hidden `<input type="file">` the Import button clicks.
   *
   * A plain file input rather than the dialog plugin, because
   * `import_custom_uploader` takes the file's *text*: this app has no
   * filesystem plugin, so a path would be a path nothing could open. The
   * element opens the same native picker either way.
   */
  let fileInput: HTMLInputElement | undefined = $state();

  async function importSxcu(event: Event): Promise<void> {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    // Cleared immediately so picking the *same* file again still fires
    // `change`, which it would not with the value left in place.
    input.value = '';
    if (!file) return;

    importError = '';
    busy = 'import';
    try {
      // Rust validates and splits the credential headers out; the settings echo
      // arrives on `settings://changed`, so the store updates itself.
      const saved = await importCustomUploader(await file.text());
      toast.success(`Imported “${saved.customUploader.name || file.name}”.`);
      setTested('custom', undefined);
    } catch (err) {
      // Verbatim, and kept on screen: it names the field that is not supported.
      importError = errorMessage(err);
    } finally {
      busy = '';
      await refreshStatus();
    }
  }

  /**
   * Forget the imported uploader.
   *
   * Credentials first, configuration second, and that order is the whole point:
   * the stored config is what *names* the credential slots, so dropping it
   * first would leave somebody's API key in Windows Credential Manager with
   * nothing left to find it by. The field list comes from the live status
   * rather than from `secretHeaders`, because the status is what Rust actually
   * keyed the store with.
   */
  async function forgetUploader(): Promise<void> {
    busy = 'import';
    const secrets = statusFor('custom')?.secrets ?? [];
    try {
      for (const secret of secrets) {
        await clearDestinationSecret('custom', secret.field);
      }
      await patch({ customUploader: { ...EMPTY_CUSTOM_UPLOADER } });
      importError = '';
      setTested('custom', undefined);
    } catch (err) {
      toast.error(errorMessage(err));
    } finally {
      busy = '';
      await refreshStatus();
    }
  }

  /**
   * Save, then re-read the status.
   *
   * The re-read is not bookkeeping: Rust decides `configured` from the
   * *non-secret* config as well as the credentials — FTP needs a `host`, the
   * custom uploader a `requestUrl` — so a save that fills one of those in
   * changes an answer only `destination_status()` can give. Without this the
   * row keeps saying "Not configured" and the red "an upload will fail" banner
   * stays up over a destination that is now fine, until the pane is remounted.
   */
  async function patchAndRefresh(delta: Partial<Settings>): Promise<void> {
    if (await patch(delta)) await refreshStatus();
  }

  function commitFtp(delta: Partial<FtpSettings>): void {
    if (!s) return;
    const next: FtpSettings = { ...ftpDraft, ...delta };
    ftpDraft = next;
    const current = { ...FTP_DEFAULTS, ...s.ftp };
    const unchanged = (Object.keys(FTP_DEFAULTS) as (keyof FtpSettings)[]).every(
      (key) => current[key] === next[key]
    );
    if (unchanged) return;
    void patchAndRefresh({ ftp: next });
  }

  /** Out of range or non-numeric would be a save that silently invents a port. */
  function commitPort(raw: string): void {
    const value = Number.parseInt(raw, 10);
    if (!Number.isFinite(value) || value < 1 || value > 65535) {
      ftpDraft = { ...ftpDraft, port: s?.ftp?.port ?? FTP_DEFAULTS.port };
      return;
    }
    commitFtp({ port: value });
  }
</script>

<!--
  One credential row: the state, a way to write a new value, and a way to remove
  it. There is deliberately no third option — nothing here can show what is
  stored, because nothing can read it back.
-->
{#snippet secretRow(kind: DestinationKind, secret: SecretStatus, help: string)}
  {@const working = busy === `secret:${kind}:${secret.field}`}
  <div class="row stack">
    <span class="row-text">
      <span class="label-line">
        <span class="row-label">{secret.label}</span>
        <span class="state" class:ok={secret.set}>
          <Icon name={secret.set ? 'check' : 'alert'} size={12} />
          {secret.set ? 'Configured' : 'Not configured'}
        </span>
        {#if !secret.required && !secret.set}
          <span class="state">Optional</span>
        {/if}
      </span>
      <span class="row-help">{help}</span>
      <span class="row-help">
        Stored in Windows Credential Manager under <code>santi.sharex/{kind}</code>, never in a
        settings file. It cannot be read back into this window — santi.sharex can only replace it
        or remove it.
      </span>
    </span>

    {#if isEditing(kind, secret.field)}
      <div class="field wide">
        <!-- type=password, and emptied the moment the write resolves. -->
        <input
          class="input mono"
          type="password"
          aria-label={secret.label}
          autocomplete="off"
          spellcheck="false"
          placeholder="Paste the value and save"
          bind:value={secretDraft}
          onkeydown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault();
              void saveSecret();
            } else if (e.key === 'Escape') {
              e.preventDefault();
              cancelEdit();
            }
          }}
        />
        <button
          type="button"
          class="btn btn-accent"
          disabled={secretDraft.trim() === '' || working}
          onclick={() => void saveSecret()}
        >
          {working ? 'Saving…' : 'Save'}
        </button>
        <button type="button" class="btn subtle" onclick={cancelEdit}>Cancel</button>
      </div>
    {:else}
      <div class="field">
        <button
          type="button"
          class="btn"
          disabled={working}
          onclick={() => beginEdit(kind, secret)}
        >
          {secret.set ? 'Replace…' : 'Set…'}
        </button>
        <button
          type="button"
          class="btn danger"
          disabled={!secret.set || working}
          onclick={() => void clearSecret(kind, secret)}
        >
          Clear
        </button>
      </div>
    {/if}
  </div>
{/snippet}

<!-- Every credential slot a destination declares, straight from Rust's list. -->
{#snippet secretsFor(kind: DestinationKind, help: string)}
  {#each statusFor(kind)?.secrets ?? [] as secret (secret.field)}
    {@render secretRow(kind, secret, help)}
  {/each}
{/snippet}

<!-- Test connection, plus whatever the destination said last time. -->
{#snippet testRow(kind: DestinationKind, help: string)}
  {@const result = tested[kind]}
  <div class="row stack">
    <span class="row-text">
      <span class="row-label">Test connection</span>
      <span class="row-help">{help}</span>
    </span>
    <div class="field">
      <button
        type="button"
        class="btn"
        disabled={busy === `test:${kind}`}
        onclick={() => void runTest(kind)}
      >
        {busy === `test:${kind}` ? 'Testing…' : 'Test connection'}
      </button>
    </div>
    {#if result}
      <p class="result" class:bad={!result.ok} role="status">
        <Icon name={result.ok ? 'check' : 'alert'} size={14} />
        <span class="selectable">{result.text}</span>
      </p>
    {/if}
  </div>
{/snippet}

<div class="dest" class:page={variant === 'page'}>
  {#if variant === 'page'}
    <header class="page-header">
      <h1 class="page-title">Destinations</h1>
    </header>
  {/if}

  {#if !s}
    <p class="loading">
      <span>Loading settings…</span>
      {#if settings.error}<span class="err">{settings.error}</span>{/if}
    </p>
  {:else}
    <section class="section" aria-labelledby="sec-dest-active">
      <h2 class="section-title" id="sec-dest-active">Where uploads go</h2>
      <p class="section-help">
        The <strong>Upload</strong> action on a capture sends it here, and nowhere else. Choosing a
        destination does not upload anything by itself — captures leave this machine only when you
        ask, one at a time, unless you turn on auto-upload in Settings.
      </p>

      {#if statusError}
        <p class="warn" role="status">
          <Icon name="alert" size={14} />
          <span>Could not read which credentials are stored: {statusError}</span>
        </p>
      {/if}

      <div class="choices" role="radiogroup" aria-labelledby="sec-dest-active">
        {#each CHOICES as choice (choice.id)}
          {@const state = statusFor(choice.id)}
          <button
            type="button"
            class="choice"
            class:selected={active === choice.id}
            role="radio"
            aria-checked={active === choice.id}
            onclick={() => void choose(choice.id)}
          >
            <span class="dot" aria-hidden="true"></span>
            <span class="choice-text">
              <span class="choice-name">
                {choice.name}
                {#if state}
                  <span class="state" class:ok={state.configured}>
                    <Icon name={state.configured ? 'check' : 'alert'} size={12} />
                    {state.configured ? 'Configured' : 'Not configured'}
                  </span>
                {/if}
              </span>
              <span class="choice-blurb">{choice.blurb}</span>
            </span>
          </button>
        {/each}
      </div>

      <!-- Named rather than offered: half an SFTP implementation would be worse
           than saying it is not there (M3 §5). -->
      <p class="note">
        <Icon name="info" size={14} />
        <span>
          SFTP is not supported. It needs an SSH stack santi.sharex does not carry, and a picker
          entry that always failed would be worse than saying so plainly.
        </span>
      </p>

      {#if activeStatus && !activeStatus.configured}
        <p class="warn" role="status">
          <Icon name="alert" size={14} />
          <span>
            {activeStatus.label} is selected but not ready to use, so an upload will fail. Finish
            setting it up below.
          </span>
        </p>
      {/if}
    </section>

    <section class="section" aria-labelledby="sec-dest-imgur">
      <h2 class="section-title" id="sec-dest-imgur">Imgur</h2>
      <p class="section-help">
        Uploads are anonymous — no Imgur account is signed in, and nothing is tied to one.
      </p>

      <!-- santi.sharex ships no client ID of its own (M3 §3), so this is not an
           empty box with a shrug: it is the actual path to a working one.
           Without it the field just looks broken. -->
      <div class="howto">
        <p class="howto-title">Getting a Client ID</p>
        <ol class="howto-steps">
          <li>
            Open <code class="selectable">{IMGUR_REGISTER_URL}</code> and sign in to Imgur.
          </li>
          <li>
            Choose <strong>“Anonymous usage without user authorization”</strong>. That is the one
            that works here; the OAuth options are for apps that sign a user in, which
            santi.sharex does not do.
          </li>
          <li>Fill in any application name and your email. No callback URL is needed.</li>
          <li>Paste the <strong>Client ID</strong> below. The Client secret is not used.</li>
        </ol>
        <p class="howto-why">
          santi.sharex does not embed one of its own on purpose: this repository is public, so a
          bundled ID would be extracted, abused, and rate-limited into uselessness for everyone
          within a week.
        </p>
      </div>

      {@render secretsFor(
        'imgur',
        'The identifier from the registration page above — the Client secret is never used.'
      )}

      <!-- What this checks, and no more. Imgur has no "is this ID valid"
           endpoint that does not upload a picture, and a connection test must
           not leave one behind — so `Imgur::test` checks the saved ID is
           present and usable in a header and says so. Describing it as asking
           Imgur would make "passed" read as "verified", which is the one thing
           it is not. -->
      {@render testRow(
        'imgur',
        'Checks a Client ID is saved and can be sent in a request header. It does not ask Imgur — the only request Imgur offers uploads a picture, and a test must not leave one behind.'
      )}
    </section>

    <section class="section" aria-labelledby="sec-dest-custom">
      <h2 class="section-title" id="sec-dest-custom">Custom uploader</h2>
      <p class="section-help">
        Import a ShareX <code>.sxcu</code> file and santi.sharex will use it as-is. The supported
        subset is the request method and URL, headers and parameters, multipart or binary bodies,
        the file form name, and <code>$json:…$</code> / <code>$response$</code> response templates.
      </p>

      <div class="row stack">
        <span class="row-text">
          <span class="row-label">Uploader file</span>
          <span class="row-help">
            Anything the file needs that is not in that list — an OAuth flow, a
            <code>RegexList</code>, another <code>RequestBodyType</code>, a destination that is not
            an image uploader — is refused here, with the field named, rather than accepted and
            then failing at the worst possible moment.
          </span>
        </span>
        <div class="field">
          <!-- Off-screen rather than display:none — a hidden input is not
               clickable in every engine, and this one is clicked. -->
          <input
            class="filepick"
            type="file"
            accept=".sxcu,.json,application/json"
            tabindex="-1"
            aria-hidden="true"
            bind:this={fileInput}
            onchange={(e) => void importSxcu(e)}
          />
          <button
            type="button"
            class="btn"
            disabled={busy === 'import'}
            onclick={() => fileInput?.click()}
          >
            {busy === 'import' ? 'Working…' : uploader ? 'Import another…' : 'Import .sxcu…'}
          </button>
          {#if uploader}
            <button
              type="button"
              class="btn danger"
              disabled={busy === 'import'}
              onclick={() => void forgetUploader()}
            >
              Remove
            </button>
          {/if}
        </div>
      </div>

      {#if importError}
        <!-- Verbatim. The message names the unsupported field, and that name is
             the only thing that tells the user what to change. -->
        <p class="warn" role="status">
          <Icon name="alert" size={14} />
          <span>
            <strong>That file was not imported.</strong>
            <span class="selectable">{importError}</span>
          </span>
        </p>
      {/if}

      {#if uploader}
        <dl class="summary">
          <dt>Name</dt>
          <dd class="selectable">{uploader.name || '(unnamed)'}</dd>
          <dt>Request</dt>
          <dd class="mono selectable">{uploader.requestMethod} {uploader.requestUrl}</dd>
          <dt>Body</dt>
          <dd>
            {uploader.body === 'Binary' ? 'Binary' : 'Multipart form data'}
            {#if uploader.body !== 'Binary' && uploader.fileFormName}
              <span class="dim">as <code>{uploader.fileFormName}</code></span>
            {/if}
          </dd>
          <dt>Link from</dt>
          <dd class="mono selectable">{uploader.url || 'the raw response'}</dd>
          {#if Object.keys(uploader.headers ?? {}).length}
            <dt>Headers</dt>
            <dd class="mono selectable">{Object.keys(uploader.headers).join(', ')}</dd>
          {/if}
          {#if Object.keys(uploader.parameters ?? {}).length}
            <dt>Parameters</dt>
            <dd class="mono selectable">{Object.keys(uploader.parameters).join(', ')}</dd>
          {/if}
        </dl>

        {#if uploader.secretHeaders?.length}
          <p class="note">
            <Icon name="info" size={14} />
            <span>
              {uploader.secretHeaders.length === 1
                ? 'One header looked like a credential, so its value was moved out of the settings file into Windows Credential Manager.'
                : `${uploader.secretHeaders.length} headers looked like credentials, so their values were moved out of the settings file into Windows Credential Manager.`}
              They are not shown here and cannot be read back.
            </span>
          </p>
        {/if}

        {@render secretsFor('custom', 'Sent as this request header on every upload.')}

        <!-- Same rule as Imgur's, for the same reason: the only request a
             .sxcu describes is the one that puts a file on somebody's server.
             `CustomUploader::test` re-validates the configuration and the
             credentials and states plainly that it has not contacted the
             server; claiming it ran the request would turn "looks complete"
             into a promise nothing made. -->
        {@render testRow(
          'custom',
          'Re-checks the imported configuration and that every credential header has a value saved. It does not contact the server — the only request this file describes is the one that uploads.'
        )}
      {/if}
    </section>

    <section class="section" aria-labelledby="sec-dest-ftp">
      <h2 class="section-title" id="sec-dest-ftp">FTP / FTPS</h2>
      <p class="section-help">
        Writes the PNG into a directory on your own server. SFTP is a different protocol and is not
        supported.
      </p>

      <div class="row stack">
        <div class="row-head">
          <span class="row-text">
            <span class="row-label" id="lbl-ftp-ftps">Encrypt the connection (FTPS)</span>
            <span class="row-help">
              Explicit FTPS upgrades the connection with <code>AUTH TLS</code> before anything is
              sent. Turn it on unless the server cannot do it — with it off, the password and the
              capture both cross the network in the clear.
            </span>
          </span>
          <Toggle
            checked={ftpMode === FTP_SECURITY.explicitTls}
            labelledBy="lbl-ftp-ftps"
            onchange={(v) =>
              commitFtp({ security: v ? FTP_SECURITY.explicitTls : FTP_SECURITY.plain })}
          />
        </div>

        <!-- At the point of the choice, not buried in docs (M3 §5). Each branch
             describes the connection that would actually be made, which is why
             the two unsupported modes are called out separately: santi.sharex
             refuses both before it reads the password, so warning about clear
             text there would name a risk that never gets the chance to happen. -->
        {#if ftpMode === FTP_SECURITY.sftp}
          <p class="warn" role="status">
            <Icon name="alert" size={14} />
            <span>
              <strong>This destination is set to SFTP, which santi.sharex cannot do.</strong>
              SFTP is a subsystem of SSH and shares nothing with FTPS beyond the name, and this
              build carries no SSH stack. Uploads are refused outright rather than attempted, so
              nothing is sent. Use the switch above to pick FTPS, or plain FTP if the server cannot
              do it.
            </span>
          </p>
        {:else if ftpMode === null}
          <p class="warn" role="status">
            <Icon name="alert" size={14} />
            <span>
              <strong>
                “<span class="selectable">{ftpDraft.security}</span>” is not a transfer mode
                santi.sharex knows.
              </strong>
              It is refused rather than guessed at, so uploads to this destination will fail until
              the switch above is used to choose FTPS or plain FTP.
            </span>
          </p>
        {:else if ftpMode === FTP_SECURITY.plain}
          <p class="warn" role="status">
            <Icon name="alert" size={14} />
            <span>
              <strong>Plain FTP sends your password across the network in clear text.</strong>
              The username, the password and the capture itself all travel unencrypted, so anyone
              able to watch the connection — on the same Wi-Fi, or anywhere along the route — can
              read all three. Turn this on unless the server genuinely cannot do FTPS.
            </span>
          </p>
        {/if}
      </div>

      <div class="row stack">
        <span class="row-text">
          <label class="row-label" for="ftp-host">Server</label>
          <span class="row-help">
            Host name and port. 21 is the usual port for both plain FTP and explicit FTPS.
          </span>
        </span>
        <div class="field wide">
          <input
            id="ftp-host"
            class="input mono"
            type="text"
            spellcheck="false"
            autocomplete="off"
            placeholder="ftp.example.com"
            bind:value={ftpDraft.host}
            onchange={() => commitFtp({ host: ftpDraft.host.trim() })}
            onblur={() => commitFtp({ host: ftpDraft.host.trim() })}
          />
          <input
            class="input mono port"
            type="number"
            min="1"
            max="65535"
            aria-label="Port"
            bind:value={ftpDraft.port}
            onchange={(e) => commitPort(e.currentTarget.value)}
            onblur={(e) => commitPort(e.currentTarget.value)}
          />
        </div>
      </div>

      <div class="row stack">
        <span class="row-text">
          <label class="row-label" for="ftp-user">Username</label>
          <span class="row-help">
            Kept in the settings file; only the password is treated as a credential.
          </span>
        </span>
        <div class="field wide">
          <input
            id="ftp-user"
            class="input mono"
            type="text"
            spellcheck="false"
            autocomplete="off"
            bind:value={ftpDraft.username}
            onchange={() => commitFtp({ username: ftpDraft.username.trim() })}
            onblur={() => commitFtp({ username: ftpDraft.username.trim() })}
          />
        </div>
      </div>

      {@render secretsFor(
        'ftp',
        'Sent when connecting. With FTPS turned off above, it crosses the network unencrypted.'
      )}

      <div class="row stack">
        <span class="row-text">
          <label class="row-label" for="ftp-dir">Remote directory</label>
          <span class="row-help">
            Where the file is written, as a path on the server — for example
            <code>/public_html/screenshots</code>.
          </span>
        </span>
        <div class="field wide">
          <input
            id="ftp-dir"
            class="input mono"
            type="text"
            spellcheck="false"
            autocomplete="off"
            placeholder="/public_html/screenshots"
            bind:value={ftpDraft.remoteDir}
            onchange={() => commitFtp({ remoteDir: ftpDraft.remoteDir.trim() })}
            onblur={() => commitFtp({ remoteDir: ftpDraft.remoteDir.trim() })}
          />
        </div>
      </div>

      <div class="row stack">
        <span class="row-text">
          <label class="row-label" for="ftp-prefix">Public URL prefix</label>
          <span class="row-help">
            What the copied link starts with. The filename is appended to it, so
            <code>https://files.example.com/shots/</code> gives a link pointing at the web host
            rather than at the FTP path. Leave it empty and the upload reports the remote path
            instead of a link.
          </span>
        </span>
        <div class="field wide">
          <input
            id="ftp-prefix"
            class="input mono"
            type="text"
            spellcheck="false"
            autocomplete="off"
            placeholder="https://files.example.com/shots/"
            bind:value={ftpDraft.urlPrefix}
            onchange={() => commitFtp({ urlPrefix: ftpDraft.urlPrefix.trim() })}
            onblur={() => commitFtp({ urlPrefix: ftpDraft.urlPrefix.trim() })}
          />
        </div>
      </div>

      <div class="row">
        <span class="row-text">
          <span class="row-label" id="lbl-ftp-passive">Passive mode</span>
          <span class="row-help">
            The client opens the data connection. Leave this on unless the server asks otherwise —
            active mode needs the server to reach back through your router.
          </span>
        </span>
        <Toggle
          checked={ftpDraft.passive}
          labelledBy="lbl-ftp-passive"
          onchange={(v) => commitFtp({ passive: v })}
        />
      </div>

      {@render testRow(
        'ftp',
        'Connects and signs in with the details above. Nothing is written and no capture is sent.'
      )}
    </section>
  {/if}
</div>

<style>
  .dest {
    display: flex;
    flex-direction: column;
    gap: 32px;
  }

  /* Only the standalone pane owns padding; the embedded form inherits the
     Settings column it is dropped into. */
  .dest.page {
    padding: 0 32px 48px;
  }

  .dest > .section {
    width: 100%;
    max-width: 760px;
  }

  .page-header {
    position: sticky;
    top: 0;
    z-index: 4;
    margin: 0 -32px;
    padding: 24px 32px 16px;
    background: var(--bg);
    border-bottom: 1px solid var(--border);
  }

  .page-title {
    margin: 0;
    font-family: var(--font-display);
    font-size: 20px;
    font-weight: 600;
    line-height: 1.2;
    color: var(--text);
  }

  .loading {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 40px 0;
    color: var(--text-dim);
  }

  .loading .err {
    color: var(--danger);
  }

  .section {
    display: flex;
    flex-direction: column;
  }

  .section-title {
    margin: 0 0 4px;
    font-family: var(--font-display);
    font-size: 13px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-faint);
  }

  .section-help {
    margin: 0 0 12px;
    font-size: 12px;
    line-height: 1.6;
    color: var(--text-dim);
  }

  /* ------------------------------------------------------------ the picker */

  .choices {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .choice {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 12px;
    font: inherit;
    text-align: left;
    color: var(--text);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    cursor: pointer;
    transition:
      border-color 140ms ease,
      background-color 140ms ease;
  }

  .choice:hover {
    border-color: var(--border-strong);
  }

  .choice:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .choice.selected {
    border-color: var(--accent);
    box-shadow: inset 0 0 0 1px var(--accent);
  }

  /* A real radio mark: the selection must not rest on the border colour alone. */
  .dot {
    flex: none;
    width: 14px;
    height: 14px;
    margin-top: 2px;
    border: 1px solid var(--border-strong);
    border-radius: 50%;
    background: var(--bg-inset);
  }

  .choice.selected .dot {
    border-color: var(--accent);
    background: var(--accent);
    box-shadow: inset 0 0 0 3px var(--bg-inset);
  }

  .choice-text {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }

  .choice-name {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    font-size: 13px;
    font-weight: 500;
    color: var(--text);
  }

  .choice-blurb {
    font-size: 12px;
    line-height: 1.5;
    color: var(--text-faint);
  }

  /* ------------------------------------------------------------- the rows */

  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 24px;
    padding: 14px 0;
    border-bottom: 1px solid var(--border);
  }

  .row.stack {
    flex-direction: column;
    align-items: stretch;
    gap: 8px;
  }

  .row:last-child {
    border-bottom: none;
  }

  /* A stacked row whose first line is still a side-by-side label + control:
     the FTPS switch needs its warning underneath, not beside it. */
  .row-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 24px;
  }

  .row-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .row-label {
    font-size: 13px;
    color: var(--text);
  }

  .row-help {
    font-size: 12px;
    line-height: 1.6;
    color: var(--text-faint);
  }

  .label-line {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
  }

  .field {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: none;
  }

  .field.wide {
    flex: 1;
  }

  .input {
    flex: 1;
    min-width: 0;
    height: 34px;
    padding: 0 10px;
    font: inherit;
    font-size: 13px;
    color: var(--text);
    background: var(--bg-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    transition: border-color 140ms ease;
  }

  .input:hover {
    border-color: var(--border-strong);
  }

  .input:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
    border-color: var(--accent);
  }

  .input.port {
    flex: none;
    width: 92px;
  }

  /* Clipped out of the layout but still focusable and clickable — the Import
     button forwards its click here. */
  .filepick {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    opacity: 0;
  }

  .mono {
    font-family: var(--font-mono);
    font-size: 12px;
  }

  .btn {
    height: 34px;
    padding: 0 12px;
    flex: none;
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

  .btn.btn-accent {
    color: var(--accent-text);
    background: var(--accent);
    border-color: var(--accent);
  }

  .btn.btn-accent:hover:not(:disabled) {
    background: var(--accent-hover);
    border-color: var(--accent-hover);
  }

  .btn.subtle {
    color: var(--text-dim);
    background: transparent;
    border-color: transparent;
  }

  .btn.subtle:hover:not(:disabled) {
    color: var(--text);
    border-color: var(--border);
  }

  .btn.danger:hover:not(:disabled) {
    color: var(--danger);
    border-color: var(--danger);
  }

  /* ------------------------------------------------- states and messages */

  /* "Configured" / "Not configured": the whole of what this page is allowed to
     say about a credential. Never the value, and never a masked stand-in for
     it — a row of dots is still a claim about its length. */
  .state {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex: none;
    padding: 0 7px;
    font-size: 11px;
    line-height: 18px;
    white-space: nowrap;
    color: var(--text-dim);
    background: var(--bg-inset);
    border: 1px solid var(--border);
    border-radius: 999px;
  }

  .state.ok {
    color: var(--success);
    background: color-mix(in srgb, var(--success) 12%, transparent);
    border-color: color-mix(in srgb, var(--success) 40%, transparent);
  }

  .warn {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    margin: 8px 0 0;
    padding: 10px 12px;
    font-size: 12px;
    line-height: 1.55;
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 10%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 40%, transparent);
    border-radius: var(--radius);
  }

  .note {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    margin: 8px 0 0;
    padding: 10px 12px;
    font-size: 12px;
    line-height: 1.55;
    color: var(--text-dim);
    background: var(--bg-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .warn :global(svg),
  .note :global(svg),
  .result :global(svg) {
    flex: none;
    margin-top: 2px;
  }

  .result {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    margin: 0;
    font-size: 12px;
    line-height: 1.55;
    color: var(--success);
  }

  .result.bad {
    color: var(--danger);
  }

  /* --------------------------------------------------- the Imgur setup path */

  .howto {
    margin: 0 0 4px;
    padding: 12px 14px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
  }

  .howto-title {
    margin: 0 0 6px;
    font-size: 13px;
    font-weight: 600;
    color: var(--text);
  }

  .howto-steps {
    margin: 0;
    padding-left: 18px;
    display: flex;
    flex-direction: column;
    gap: 5px;
    font-size: 12px;
    line-height: 1.6;
    color: var(--text-dim);
  }

  .howto-why {
    margin: 10px 0 0;
    font-size: 12px;
    line-height: 1.6;
    color: var(--text-faint);
  }

  /* ------------------------------------------------ the .sxcu summary table */

  .summary {
    display: grid;
    grid-template-columns: minmax(96px, auto) 1fr;
    gap: 6px 14px;
    margin: 10px 0 0;
    padding: 12px 14px;
    font-size: 12px;
    line-height: 1.5;
    background: var(--bg-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .summary dt {
    color: var(--text-faint);
  }

  .summary dd {
    margin: 0;
    min-width: 0;
    color: var(--text);
    overflow-wrap: anywhere;
  }

  .summary .dim {
    color: var(--text-faint);
  }

  code {
    font-family: var(--font-mono);
    font-size: 11px;
    padding: 0 4px;
    color: var(--text-dim);
    background: var(--bg-inset);
    border: 1px solid var(--border);
    border-radius: 5px;
  }

  .summary code {
    background: var(--bg);
  }
</style>
