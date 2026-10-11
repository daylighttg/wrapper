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
  // Result of the agent's /key probe; `busy` says nothing, so it gets no badge.
  const verdictKinds = {
    ok: 'ok',
    broken: 'err',
    networkDown: 'warn',
    liteDown: 'warn',
  };

  const verdict = $derived(account.state === 'running' ? account.health?.verdict : null);
  const verdictTitle = $derived.by(() => {
    const h = account.health;
    if (!h?.checkedAt) return '';
    const at = new Date(h.checkedAt).toLocaleString();
    const lines = [t('acc.health.checkedAt', { at, ms: h.tookMs ?? '?' })];
    if (h.detail) lines.push(h.detail);
    if (h.consecutiveFailures) lines.push(t('acc.health.failures', { n: h.consecutiveFailures }));
    return lines.join('\n');
  });
</script>

{#if account.degraded && account.state === 'running'}
  <span class="badge warn" title={t('acc.degraded')}>{t('acc.degraded')}</span>
{:else}
  <span class="badge {kinds[account.state] ?? ''}">{t(`acc.state.${account.state}`)}</span>
{/if}
{#if verdict && verdictKinds[verdict]}
  <span class="badge {verdictKinds[verdict]}" title={verdictTitle}>{t(`acc.health.${verdict}`)}</span>
{/if}
