<script lang="ts">
  interface Props {
    checked: boolean;
    onchange: (checked: boolean) => void;
    /** Used when there is no visible label element to point at. */
    label?: string;
    labelledBy?: string;
    describedBy?: string;
    disabled?: boolean;
  }

  let {
    checked,
    onchange,
    label,
    labelledBy,
    describedBy,
    disabled = false
  }: Props = $props();
</script>

<button
  type="button"
  role="switch"
  class="toggle"
  aria-checked={checked}
  aria-label={labelledBy ? undefined : label}
  aria-labelledby={labelledBy}
  aria-describedby={describedBy}
  {disabled}
  onclick={() => onchange(!checked)}
>
  <span class="knob"></span>
</button>

<style>
  .toggle {
    flex: none;
    width: 40px;
    height: 22px;
    padding: 0;
    border: 1px solid var(--border-strong);
    border-radius: 999px;
    background: var(--bg-inset);
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    transition:
      background-color 140ms ease,
      border-color 140ms ease;
  }

  .toggle:hover:not(:disabled) {
    border-color: var(--accent);
  }

  .toggle:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .toggle:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .toggle[aria-checked='true'] {
    background: var(--accent);
    border-color: var(--accent);
  }

  .knob {
    width: 16px;
    height: 16px;
    margin-left: 2px;
    border-radius: 50%;
    background: var(--text-dim);
    transition:
      transform 140ms ease,
      background-color 140ms ease;
  }

  .toggle[aria-checked='true'] .knob {
    background: var(--accent-text);
    transform: translateX(18px);
  }
</style>
