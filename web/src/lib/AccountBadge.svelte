<script>
  import { t } from './i18n.svelte.js';

  let { account } = $props();
  const kinds = {
    running: 'ok',
    starting: 'info pulse',
    loggingIn: 'info pulse',
    backoff: 'warn',
    needsLogin: 'err',
    stopped: '',
  };
</script>

{#if account.degraded && account.state === 'running'}
  <span class="badge warn" title={t('acc.degraded')}>{t('acc.degraded')}</span>
{:else}
  <span class="badge {kinds[account.state] ?? ''}">{t(`acc.state.${account.state}`)}</span>
{/if}
