<script>
  import { t } from './i18n.svelte.js';
  import { ENDPOINTS, total, okRate, fmt } from './usage.js';

  /** `summary`: {total, today} from /api/stats or the public overview. */
  let { summary } = $props();
</script>

<div class="tiles">
  <div class="tile all">
    <div class="name">{t('usage.all')}</div>
    <div class="big">{fmt(total(summary?.today))}</div>
    <div class="sub">{t('usage.today')} · {t('usage.totalN', { n: fmt(total(summary?.total)) })}</div>
  </div>
  {#each ENDPOINTS as ep (ep)}
    {@const rate = okRate(summary?.total, ep)}
    <div class="tile">
      <div class="name mono">/{ep}</div>
      <div class="big">{fmt(total(summary?.today, ep))}</div>
      <div class="sub">
        {t('usage.totalN', { n: fmt(total(summary?.total, ep)) })}{#if rate !== null}{' · '}{t('usage.okRate', { p: rate })}{/if}
      </div>
    </div>
  {/each}
</div>

<style>
  .tiles {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
    gap: 10px;
  }

  .tile {
    background: var(--surface-2);
    border-radius: 10px;
    padding: 10px 12px;
    min-width: 0;
  }

  .name {
    font-size: 12px;
    color: var(--muted);
  }

  .big {
    font-size: 22px;
    font-weight: 650;
    letter-spacing: -0.02em;
    font-variant-numeric: tabular-nums;
  }

  .sub {
    font-size: 11.5px;
    color: var(--muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
