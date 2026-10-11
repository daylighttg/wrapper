<script>
  import { agent } from './api.js';
  import { app, toast } from './state.svelte.js';
  import { t } from './i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** `username` pre-fills a re-login; `onclose` is called when done. */
  let { username: initialUser = '', onclose } = $props();

  // svelte-ignore state_referenced_locally
  let username = $state(initialUser);
  let password = $state('');
  let deviceInfo = $state('');
  let advanced = $state(false);
  let code = $state('');
  let loginId = $state(null);
  let error = $state('');
  let submitting = $state(false);
  let logEl = $state(null);

  // Live state from SSE (falls back to the last HTTP answer).
  let lastView = $state(null);
  const view = $derived((loginId && app.logins[loginId]) || lastView);
  const phase = $derived(view?.state ?? (submitting ? 'running' : 'form'));
  const logLines = $derived(loginId ? app.logs.filter((l) => l.source === `login:${loginId}`).slice(-80) : []);

  $effect(() => {
    logLines.length;
    if (logEl) logEl.scrollTop = logEl.scrollHeight;
  });

  async function submit(e) {
    e.preventDefault();
    error = '';
    submitting = true;
    try {
      const v = await agent.login(username.trim(), password, advanced ? deviceInfo.trim() : '');
      loginId = v.loginId;
      lastView = v;
      password = '';
    } catch (err) {
      error = err.message;
    } finally {
      submitting = false;
    }
  }

  async function verify(e) {
    e.preventDefault();
    error = '';
    submitting = true;
    try {
      lastView = await agent.code(loginId, code.trim());
      code = '';
    } catch (err) {
      error = err.message;
    } finally {
      submitting = false;
    }
  }

  async function close() {
    if (loginId && (phase === 'running' || phase === 'need2fa')) {
      try {
        await agent.cancelLogin(loginId);
      } catch {
        /* already finished */
      }
    }
    if (phase === 'success') toast(`${t('login.success')}: ${view.username}`, 'ok');
    onclose?.();
  }

  function onkey(e) {
    if (e.key === 'Escape') close();
  }
</script>

<svelte:window onkeydown={onkey} />

<div class="backdrop" role="presentation" onclick={(e) => e.target === e.currentTarget && close()}>
  <div class="dialog card" role="dialog" aria-modal="true">
    <div class="card-title">
      <h2>{initialUser ? t('login.reloginTitle') : t('login.title')}</h2>
      <button class="ghost icon" onclick={close} aria-label={t('action.close')}><Icon name="x" /></button>
    </div>

    {#if phase === 'form' || (phase === 'failed' && !loginId)}
      <form class="stack" onsubmit={submit}>
        <label class="field">
          <span>{t('login.username')}</span>
          <input bind:value={username} autocomplete="username" required disabled={!!initialUser} />
        </label>
        <label class="field">
          <span>{t('login.password')}</span>
          <input type="password" bind:value={password} autocomplete="current-password" required />
        </label>
        <button type="button" class="ghost small adv" onclick={() => (advanced = !advanced)}>
          {advanced ? '▾' : '▸'} {t('login.advanced')}
        </button>
        {#if advanced}
          <label class="field">
            <span>{t('login.deviceInfo')}</span>
            <input bind:value={deviceInfo} placeholder="Music/5.0.2/Android/10/Pixel 8/7663314/en-US/en-US/…" />
            <div class="hint">{t('login.deviceInfoHint')}</div>
          </label>
        {/if}
        {#if error}<div class="callout err">{error}</div>{/if}
        <div class="hint">{t('login.note')}</div>
        <div class="row end">
          <button type="button" onclick={close}>{t('action.cancel')}</button>
          <button class="primary" type="submit" disabled={submitting || !username || !password}>
            {#if submitting}<span class="spinner"></span>{/if}{t('login.submit')}
          </button>
        </div>
      </form>
    {:else}
      <div class="stack">
        {#if phase === 'running'}
          <div class="callout info row"><span class="spinner"></span>{view?.message || t('login.signingIn')}</div>
        {:else if phase === 'need2fa'}
          <form class="stack" onsubmit={verify}>
            <div class="callout info">{t('login.needCode')}</div>
            <label class="field">
              <span>{t('login.code')}</span>
              <!-- svelte-ignore a11y_autofocus -->
              <input
                class="code"
                bind:value={code}
                inputmode="numeric"
                autocomplete="one-time-code"
                maxlength="6"
                pattern="[0-9]{6}"
                autofocus
                required
              />
            </label>
            {#if error}<div class="callout err">{error}</div>{/if}
            <div class="row end">
              <button type="button" onclick={close}>{t('action.cancel')}</button>
              <button class="primary" type="submit" disabled={submitting || code.trim().length !== 6}>
                {#if submitting}<span class="spinner"></span>{/if}{t('login.verify')}
              </button>
            </div>
          </form>
        {:else if phase === 'success'}
          <div class="callout row ok"><Icon name="check" />{t('login.success')} · {view.message}</div>
          <div class="row end"><button class="primary" onclick={close}>{t('action.close')}</button></div>
        {:else if phase === 'failed'}
          <div class="callout err"><b>{t('login.failed')}:</b> {view.message}</div>
          <div class="row end">
            <button onclick={close}>{t('action.close')}</button>
            <button class="primary" onclick={() => { loginId = null; lastView = null; error = ''; }}>{t('login.submit')}</button>
          </div>
        {/if}
        {#if logLines.length}
          <div class="small muted">{t('login.log')}</div>
          <div class="logbox log" bind:this={logEl}>
            {#each logLines as l (l.seq)}<div class={l.level}>{l.msg}</div>{/each}
          </div>
        {/if}
      </div>
    {/if}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(10, 10, 15, 0.45);
    display: grid;
    place-items: center;
    z-index: 50;
    padding: 16px;
  }

  .dialog {
    width: min(480px, 100%);
    max-height: calc(100vh - 32px);
    overflow: auto;
  }

  .end {
    justify-content: flex-end;
  }

  .adv {
    align-self: flex-start;
    padding: 0 4px;
  }

  .code {
    font-family: var(--mono);
    font-size: 22px;
    letter-spacing: 0.4em;
    text-align: center;
    height: 48px;
  }

  .callout.ok {
    background: var(--ok-soft);
    color: var(--ok);
  }

  .log {
    max-height: 180px;
  }

  .log .warn {
    color: var(--warn);
  }

  .log .error {
    color: var(--err);
  }
</style>
