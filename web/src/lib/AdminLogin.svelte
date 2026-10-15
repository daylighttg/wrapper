<script>
  import { session } from './api.js';
  import { t } from './i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** `offline`: the service is not running (only the admin can start it). */
  let { onback = null, offline = false } = $props();
  let token = $state('');
  let error = $state('');
  let busy = $state(false);

  async function submit(e) {
    e.preventDefault();
    busy = true;
    error = '';
    try {
      await session.signIn(token.trim());
      location.hash = '#/dashboard';
      location.reload();
    } catch (err) {
      error = err.message;
      busy = false;
    }
  }
</script>

<div class="wrap">
  <form class="card stack" onsubmit={submit}>
    <div class="brand">
      <img src="/favicon.svg" alt="" width="32" height="32" />
      <h2>{t('signin.title')}</h2>
    </div>
    {#if offline}<div class="callout warn">{t('signin.offline')}</div>{/if}
    <label class="field">
      <span>{t('signin.token')}</span>
      <!-- svelte-ignore a11y_autofocus -->
      <input type="password" bind:value={token} autocomplete="current-password" autofocus required />
      <div class="hint">{t('signin.hint')}</div>
    </label>
    {#if error}<div class="callout err">{error}</div>{/if}
    <div class="row end">
      {#if onback}<button type="button" class="ghost" onclick={onback}>{t('signin.back')}</button>{/if}
      <button class="primary" type="submit" disabled={busy || !token.trim()}>
        {#if busy}<span class="spinner"></span>{:else}<Icon name="login" size={16} />{/if}{t('signin.submit')}
      </button>
    </div>
  </form>
</div>

<style>
  .wrap {
    min-height: 100vh;
    display: grid;
    place-items: center;
    padding: 16px;
  }

  form {
    width: min(400px, 100%);
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .end {
    justify-content: flex-end;
  }
</style>
