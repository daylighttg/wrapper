<script>
  import { app } from './state.svelte.js';
  import { t } from './i18n.svelte.js';

  const kinds = { running: 'ok', starting: 'info pulse', stopping: 'warn pulse', failed: 'err', stopped: '' };
  const state = $derived(app.status?.state || 'stopped');
</script>

<span class="badge {kinds[state]}">{t('dash.engine')} · {t(`engine.state.${state}`)}</span>
