<script>
  import { t } from './i18n.svelte.js';
  import Icon from './Icon.svelte';

  let { value, secret = false } = $props();
  let revealed = $state(false);
  let copied = $state(false);

  async function copy() {
    try {
      await navigator.clipboard.writeText(value);
    } catch {
      // Insecure context (LAN over http): fall back to a hidden textarea.
      const ta = document.createElement('textarea');
      ta.value = value;
      document.body.append(ta);
      ta.select();
      document.execCommand('copy');
      ta.remove();
    }
    copied = true;
    setTimeout(() => (copied = false), 1500);
  }
</script>

<div class="copy">
  <code>{secret && !revealed ? '•'.repeat(Math.min(value.length, 24)) : value}</code>
  {#if secret}
    <button class="ghost small" onclick={() => (revealed = !revealed)}>{revealed ? t('action.hide') : t('action.show')}</button>
  {/if}
  <button class="small" onclick={copy}>
    <Icon name={copied ? 'check' : 'copy'} size={14} />
    {copied ? t('action.copied') : t('action.copy')}
  </button>
</div>

<style>
  .copy {
    display: flex;
    align-items: center;
    gap: 6px;
    background: var(--surface-2);
    border-radius: 9px;
    padding: 4px 4px 4px 12px;
    min-width: 0;
  }

  code {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
