<script>
  import { pub } from './api.js';
  import { t } from './i18n.svelte.js';
  import { remember } from './owned.svelte.js';
  import Icon from './Icon.svelte';
  import CopyField from './CopyField.svelte';

  /** `ondone` runs after a login settles (to refresh the overview). */
  let { ondone } = $props();

  let username = $state('');
  let password = $state('');
  let code = $state('');
  let error = $state('');
  let busy = $state(false);
  let pending = $state(null); // {ticket, ref, name, key, username}
  let phase = $state('form');
  let message = $state('');
  let timer = null;

  function settle(login) {
    phase = login.state;
    message = login.message || '';
    clearTimeout(timer);
    if (phase === 'running') timer = setTimeout(poll, 2000);
    if (phase === 'success') {
      remember({ ref: pending.ref, name: pending.name, username: pending.username, key: pending.key, at: Date.now() });
    }
    if (phase === 'success' || phase === 'failed') ondone?.();
  }

  async function poll() {
    try {
      settle(await pub.status(pending.ticket));
    } catch (e) {
      error = e.message;
      phase = 'failed';
    }
  }

  $effect(() => () => clearTimeout(timer));

  async function submit(e) {
    e.preventDefault();
    error = '';
    busy = true;
    try {
      const r = await pub.login(username.trim(), password);
      pending = { ticket: r.ticket, ref: r.ref, name: r.name, key: r.ownerKey, username: username.trim().toLowerCase() };
      password = '';
      settle(r.login);
    } catch (err) {
      error = err.message;
    } finally {
      busy = false;
    }
  }

  async function verify(e) {
    e.preventDefault();
    error = '';
    busy = true;
    try {
      settle(await pub.code(pending.ticket, code.trim()));
      code = '';
    } catch (err) {
      error = err.message;
    } finally {
      busy = false;
    }
  }

  async function cancel() {
    clearTimeout(timer);
    if (pending && (phase === 'running' || phase === 'need2fa')) {
      try {
        await pub.cancel(pending.ticket);
      } catch {
        /* already over */
      }
    }
    reset();
  }

  function reset() {
    pending = null;
    phase = 'form';
    message = '';
    error = '';
  }
</script>

{#if phase === 'form'}
  <form class="stack" onsubmit={submit}>
    <div class="fields">
      <label class="field">
        <span>{t('login.username')}</span>
        <input bind:value={username} autocomplete="username" required />
      </label>
      <label class="field">
        <span>{t('login.password')}</span>
        <input type="password" bind:value={password} autocomplete="current-password" required />
      </label>
    </div>
    {#if error}<div class="callout err">{error}</div>{/if}
    <div class="hint">{t('pub.loginNote')}</div>
    <div class="row end">
      <button class="primary" type="submit" disabled={busy || !username.trim() || !password}>
        {#if busy}<span class="spinner"></span>{:else}<Icon name="login" size={16} />{/if}{t('login.submit')}
      </button>
    </div>
  </form>
{:else if phase === 'running'}
  <div class="stack">
    <div class="callout info row"><span class="spinner"></span>{message || t('login.signingIn')}</div>
    <div class="row end"><button onclick={cancel}>{t('action.cancel')}</button></div>
  </div>
{:else if phase === 'need2fa'}
  <form class="stack" onsubmit={verify}>
    <div class="callout info">{t('login.needCode')}</div>
    <label class="field">
      <span>{t('login.code')}</span>
      <!-- svelte-ignore a11y_autofocus -->
      <input class="code" bind:value={code} inputmode="numeric" autocomplete="one-time-code" maxlength="6" pattern={'[0-9]{6}'} autofocus required />
    </label>
    {#if error}<div class="callout err">{error}</div>{/if}
    <div class="row end">
      <button type="button" onclick={cancel}>{t('action.cancel')}</button>
      <button class="primary" type="submit" disabled={busy || code.trim().length !== 6}>
        {#if busy}<span class="spinner"></span>{/if}{t('login.verify')}
      </button>
    </div>
  </form>
{:else if phase === 'success'}
  <div class="stack">
    <div class="callout ok row"><Icon name="check" />{t('login.success')} · {pending.name}</div>
    <div class="small"><b>{t('pub.keyTitle')}</b></div>
    <CopyField value={pending.key} />
    <div class="callout warn small">{t('pub.keyHint')}</div>
    <div class="row end"><button class="primary" onclick={reset}>{t('action.close')}</button></div>
  </div>
{:else}
  <div class="stack">
    <div class="callout err"><b>{t('login.failed')}:</b> {message || error}</div>
    <div class="row end"><button class="primary" onclick={reset}>{t('pub.tryAgain')}</button></div>
  </div>
{/if}

<style>
  .fields {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: 12px;
  }

  .end {
    justify-content: flex-end;
  }

  .code {
    font-family: var(--mono);
    font-size: 22px;
    letter-spacing: 0.4em;
    text-align: center;
    height: 48px;
    max-width: 240px;
  }

  .callout.ok {
    background: var(--ok-soft);
    color: var(--ok);
  }
</style>
