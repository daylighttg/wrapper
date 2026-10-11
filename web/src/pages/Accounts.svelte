<script>
  import { agent } from '../lib/api.js';
  import { app, toast, engineUp } from '../lib/state.svelte.js';
  import { t, duration } from '../lib/i18n.svelte.js';
  import Icon from '../lib/Icon.svelte';
  import AccountBadge from '../lib/AccountBadge.svelte';
  import LoginDialog from '../lib/LoginDialog.svelte';

  let dialog = $state(location.hash.includes('?add') ? { username: '' } : null);
  let busy = $state({});
  let now = $state(Date.now());
  $effect(() => {
    const id = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(id);
  });

  const up = $derived(engineUp());
  const ago = (ts) => (now - ts < 5000 ? t('common.justNow') : t('common.ago', { time: duration(now - ts) }));
  const anyRunning = $derived(app.accounts.some((a) => a.state === 'running'));

  // Live /key probe of every running account; the results arrive through
  // the account events, the summary goes to a toast.
  let probing = $state(false);
  async function probe() {
    probing = true;
    try {
      const r = await agent.probeHealth();
      toast(t(`acc.probe.${r.status}`), r.status === 'ok' ? 'ok' : r.healthy ? 'info' : 'error', 6000);
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      probing = false;
    }
  }

  async function act(acc, action) {
    if (action === 'remove' && !confirm(t('acc.confirmDelete', { user: acc.username }))) return;
    busy[acc.id] = true;
    try {
      await agent[action](acc.id);
      if (action === 'remove') app.accounts = app.accounts.filter((a) => a.id !== acc.id);
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      busy[acc.id] = false;
    }
  }

  function closeDialog() {
    dialog = null;
    if (location.hash.includes('?')) history.replaceState(null, '', '#/accounts');
  }
</script>

<div class="row toolbar">
  <span class="muted">{app.accounts.length ? `${app.accounts.filter((a) => a.state === 'running').length} / ${app.accounts.length}` : ''}</span>
  <span class="spacer"></span>
  <button disabled={!up || !anyRunning || probing} title={t('acc.probe.hint')} onclick={probe}>
    <Icon name="key" size={16} />{probing ? t('acc.probe.running') : t('action.probe')}
  </button>
  <button class="primary" disabled={!up} onclick={() => (dialog = { username: '' })}>
    <Icon name="plus" size={16} />{t('action.addAccount')}
  </button>
</div>

{#if !up}
  <div class="card empty">{t('acc.engineOff')}</div>
{:else if app.accounts.length === 0}
  <div class="card empty">
    <h3>{t('acc.empty')}</h3>
    <p>{t('acc.emptyHint')}</p>
    <button class="primary" onclick={() => (dialog = { username: '' })}><Icon name="plus" size={16} />{t('action.addAccount')}</button>
  </div>
{:else}
  <div class="list">
    {#each app.accounts as a (a.id)}
      <div class="card acc">
        <div class="main">
          <div class="row head">
            <span class="user">{a.username}</span>
            <span class="badge plain info">{(a.region || '—').toUpperCase()}</span>
            <AccountBadge account={a} />
          </div>
          <div class="row meta small muted">
            {#if a.runningSince}<span>{t('acc.uptime', { time: duration(now - a.runningSince) })}</span>{/if}
            {#if a.restarts}<span>{t('acc.restarts', { n: a.restarts })}</span>{/if}
            {#if a.inFlight}<span>{t('acc.inFlight', { n: a.inFlight })}</span>{/if}
            {#if a.health?.lastKeyOkAt}<span>{t('acc.health.lastKeyOk', { time: ago(a.health.lastKeyOkAt) })}</span>{/if}
            {#if a.health?.checkedAt}
              <span title={a.health.detail || ''}>{t('acc.health.probed', { time: ago(a.health.checkedAt), ms: a.health.tookMs ?? '?' })}</span>
            {/if}
            {#if a.health?.healthRestarts}<span>{t('acc.health.restarts', { n: a.health.healthRestarts })}</span>{/if}
            <span class="mono id">{a.id.slice(0, 8)}</span>
          </div>
          {#if a.lastError && a.state !== 'running'}
            <div class="callout {a.state === 'needsLogin' ? 'err' : 'warn'} small err-line">{a.lastError}</div>
          {:else if a.state === 'running' && ['broken', 'networkDown', 'liteDown'].includes(a.health?.verdict) && a.health.detail}
            <div class="callout {a.health.verdict === 'broken' ? 'err' : 'warn'} small err-line">{a.health.detail}</div>
          {/if}
        </div>
        <div class="actions row">
          {#if a.state === 'needsLogin'}
            <button class="primary small" onclick={() => (dialog = { username: a.username })}><Icon name="login" size={14} />{t('action.relogin')}</button>
          {/if}
          {#if a.enabled && a.state !== 'stopped' && a.state !== 'needsLogin'}
            <button class="small" disabled={busy[a.id]} onclick={() => act(a, 'restart')}><Icon name="restart" size={14} />{t('action.restart')}</button>
            <button class="small" disabled={busy[a.id]} onclick={() => act(a, 'stop')}><Icon name="stop" size={14} />{t('action.stop')}</button>
          {:else if a.state !== 'loggingIn'}
            <button class="small" disabled={busy[a.id]} onclick={() => act(a, 'start')}><Icon name="play" size={14} />{t('action.start')}</button>
          {/if}
          {#if a.state !== 'needsLogin'}
            <button class="small ghost" title={t('action.relogin')} onclick={() => (dialog = { username: a.username })}><Icon name="login" size={14} /></button>
          {/if}
          <button class="small ghost danger" title={t('action.delete')} disabled={busy[a.id]} onclick={() => act(a, 'remove')}><Icon name="trash" size={14} /></button>
        </div>
      </div>
    {/each}
  </div>
{/if}

{#if dialog}
  <LoginDialog username={dialog.username} onclose={closeDialog} />
{/if}

<style>
  .toolbar {
    margin-bottom: 14px;
  }

  .list {
    display: grid;
    gap: 10px;
  }

  .acc {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 14px 16px;
  }

  .main {
    flex: 1;
    min-width: 0;
  }

  .head {
    gap: 10px;
  }

  .user {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .meta {
    margin-top: 4px;
    gap: 14px;
  }

  .id {
    opacity: 0.7;
  }

  .err-line {
    margin-top: 8px;
  }

  .actions {
    flex-wrap: nowrap;
  }

  .empty h3 {
    color: var(--text);
    margin-bottom: 4px;
  }

  .empty p {
    margin: 0 0 16px;
  }

  @media (max-width: 760px) {
    .acc {
      flex-direction: column;
      align-items: stretch;
    }

    .actions {
      flex-wrap: wrap;
    }
  }
</style>
