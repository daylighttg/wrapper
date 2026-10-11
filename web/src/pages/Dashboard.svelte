<script>
  import { app } from '../lib/state.svelte.js';
  import { t, duration } from '../lib/i18n.svelte.js';
  import { busy, engineAction } from '../lib/engine.svelte.js';
  import Icon from '../lib/Icon.svelte';
  import EngineBadge from '../lib/EngineBadge.svelte';
  import CopyField from '../lib/CopyField.svelte';
  import AccountBadge from '../lib/AccountBadge.svelte';
  import ApiTester from '../lib/ApiTester.svelte';

  let now = $state(Date.now());
  $effect(() => {
    const id = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(id);
  });

  const state = $derived(app.status?.state || 'stopped');
  const up = $derived(state === 'running' || state === 'starting');
  const cfg = $derived(app.config);
  const serving = $derived(app.accounts.filter((a) => a.state === 'running'));
  const regions = $derived([...new Set(serving.map((a) => a.region).filter(Boolean))].sort());
  const qemuMissing = $derived(cfg?.engine === 'qemu' && app.info && !(app.info.qemu?.qemu && app.info.qemu?.kernel && app.info.qemu?.initrd));
  const lanUrl = $derived(
    cfg && cfg.apiHost === '0.0.0.0' && app.info?.lanAddress ? `http://${app.info.lanAddress}:${cfg.apiPort}` : null,
  );
  const recommendedMb = $derived(400 + 150 * Math.max(1, app.accounts.length));
  const lowMemory = $derived(cfg?.engine === 'qemu' && app.accounts.length > 1 && cfg.memoryMb < recommendedMb);
</script>

<div class="grid top">
  <section class="card engine">
    <div class="card-title">
      <h2>{t('dash.engine')}</h2>
      <EngineBadge />
    </div>
    <div class="stack">
      <div class="muted">
        {cfg ? t(`engine.${cfg.engine}`) : ''}
        {#if app.status && state !== 'stopped'}
          · {t('dash.since', { time: duration(now - app.status.since) })}
        {/if}
      </div>
      {#if state === 'failed' && app.status?.lastError}
        <div class="callout err">{app.status.lastError}</div>
      {:else if state === 'starting'}
        <div class="callout info">{t('dash.startingHint')}</div>
      {:else if !up}
        <div class="muted small">{t('dash.stopped')}</div>
      {/if}
      {#if qemuMissing}
        <div class="callout warn row">
          <span>{t('dash.assetsMissing')}</span>
          <span class="spacer"></span>
          <a href="#/setup">{t('dash.goSetup')} →</a>
        </div>
      {/if}
      {#if lowMemory}
        <div class="callout warn">
          {t('dash.memoryHint', { n: app.accounts.length, mb: recommendedMb, cur: cfg.memoryMb })}
        </div>
      {/if}
      <div class="row">
        {#if up}
          <button onclick={() => engineAction('stop')} disabled={busy.engine}><Icon name="stop" size={16} />{t('action.stop')}</button>
          <button onclick={() => engineAction('restart')} disabled={busy.engine}><Icon name="restart" size={16} />{t('action.restart')}</button>
        {:else}
          <button class="primary" onclick={() => engineAction('start')} disabled={busy.engine || state === 'stopping'}>
            {#if busy.engine}<span class="spinner"></span>{:else}<Icon name="play" size={16} />{/if}
            {t('action.start')}
          </button>
        {/if}
      </div>
    </div>
  </section>

  <section class="card">
    <div class="card-title"><h2>{t('dash.endpoint')}</h2></div>
    {#if cfg}
      <div class="stack">
        <CopyField value={app.status?.apiUrl || `http://${cfg.apiHost === '0.0.0.0' ? '127.0.0.1' : cfg.apiHost}:${cfg.apiPort}`} />
        {#if lanUrl}
          <div class="small muted">{t('dash.lan')}</div>
          <CopyField value={lanUrl} />
        {/if}
        <div class="hint">{t('dash.endpointHint')}</div>
        <div class="small muted">{t('dash.token')}</div>
        <CopyField value={cfg.apiToken} secret />
        <div class="hint">{t('dash.tokenHint')}</div>
      </div>
    {/if}
  </section>
</div>

<div class="grid stats">
  <section class="card stat">
    <div class="muted small">{t('dash.accounts')}</div>
    <div class="big">{serving.length}<span class="muted"> / {app.accounts.length}</span></div>
    <div class="small muted">{t('dash.runningOf', { running: serving.length, total: app.accounts.length })}</div>
  </section>
  <section class="card stat">
    <div class="muted small">{t('dash.regions')}</div>
    <div class="row regions">
      {#each regions as r (r)}
        <span class="badge plain info">{r.toUpperCase()}</span>
      {:else}
        <span class="muted">{t('dash.noRegions')}</span>
      {/each}
    </div>
  </section>
</div>

<section class="card">
  <div class="card-title">
    <h2>{t('nav.accounts')}</h2>
    <a href="#/accounts">{t('dash.viewAll')} →</a>
  </div>
  {#if !up}
    <div class="muted small">{t('acc.engineOff')}</div>
  {:else if app.accounts.length === 0}
    <div class="row">
      <span class="muted">{t('dash.noAccounts')}</span>
      <a class="btn primary" href="#/accounts?add">{t('action.addAccount')}</a>
    </div>
  {:else}
    <ul class="mini">
      {#each app.accounts.slice(0, 6) as a (a.id)}
        <li>
          <span class="user">{a.username}</span>
          <span class="badge plain">{(a.region || '—').toUpperCase()}</span>
          <span class="spacer"></span>
          <AccountBadge account={a} />
        </li>
      {/each}
    </ul>
  {/if}
</section>

{#if up}
  <ApiTester />
{/if}

<style>
  .top {
    grid-template-columns: 1fr 1fr;
    margin-bottom: 16px;
  }

  .stats {
    grid-template-columns: 1fr 2fr;
    margin-bottom: 16px;
  }

  .stat .big {
    font-size: 28px;
    font-weight: 650;
    letter-spacing: -0.02em;
    margin: 2px 0;
  }

  .regions {
    margin-top: 10px;
  }

  .mini {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .mini li {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 9px 0;
    border-top: 1px solid var(--border);
  }

  .mini li:first-child {
    border-top: none;
  }

  .user {
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  a.btn.primary {
    background: var(--accent);
    color: #fff;
    border-color: var(--accent);
  }

  @media (max-width: 860px) {
    .top,
    .stats {
      grid-template-columns: 1fr;
    }
  }
</style>
