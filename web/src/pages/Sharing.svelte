<script>
  import { agent } from '../lib/api.js';
  import { app, toast, engineUp } from '../lib/state.svelte.js';
  import { t } from '../lib/i18n.svelte.js';
  import Icon from '../lib/Icon.svelte';
  import CopyField from '../lib/CopyField.svelte';

  let form = $state(null);
  let saved = $state(null);
  let saving = $state(false);
  let error = $state('');
  const up = $derived(engineUp());

  async function load() {
    try {
      saved = await agent.settings();
      form = { publicMode: saved.publicMode, maxAccounts: saved.maxAccounts, publicNotice: saved.publicNotice };
      error = '';
    } catch (e) {
      error = e.message;
    }
  }

  $effect(() => {
    if (up && !form) load();
  });

  async function save(e) {
    e.preventDefault();
    saving = true;
    try {
      saved = await agent.saveSettings({
        publicMode: form.publicMode,
        maxAccounts: Math.max(0, Math.floor(Number(form.maxAccounts) || 0)),
        publicNotice: form.publicNotice,
      });
      form.maxAccounts = saved.maxAccounts;
      toast(t('set.saved'), 'ok');
    } catch (err) {
      toast(err.message, 'error', 6000);
    } finally {
      saving = false;
    }
  }

  // QEMU: about 400 MB for the guest plus 150 MB per account.
  const suggested = $derived(
    app.mode === 'gui' && app.config?.engine === 'qemu' ? Math.max(1, Math.floor((app.config.memoryMb - 400) / 150)) : null,
  );
  const localOnly = $derived(app.mode === 'gui' && app.config?.apiHost === '127.0.0.1');
  const publicUrl = $derived(`${app.info?.apiUrl || location.origin}/#/public`);
</script>

{#if !up}
  <div class="card empty">{t('acc.engineOff')}</div>
{:else if error}
  <div class="callout err">{error}</div>
{:else if form}
  <form class="grid" onsubmit={save}>
    <section class="card">
      <div class="card-title">
        <h2>{t('share.title')}</h2>
        {#if saved?.publicMode}<span class="badge ok">{t('share.on')}</span>{:else}<span class="badge">{t('share.off')}</span>{/if}
      </div>
      <div class="stack">
        <p class="muted small intro">{t('share.intro')}</p>
        <label class="row"><input type="checkbox" bind:checked={form.publicMode} />{t('share.enable')}</label>
        {#if form.publicMode && localOnly}
          <div class="callout warn row">
            <span>{t('share.localOnly')}</span>
            <span class="spacer"></span>
            <a href="#/settings">{t('nav.settings')} →</a>
          </div>
        {/if}
        <div class="fields">
          <label class="field">
            <span>{t('share.max')}</span>
            <input type="number" min="0" step="1" bind:value={form.maxAccounts} />
            <div class="hint">
              {t('share.maxHint', { n: saved.activeAccounts })}
              {#if suggested !== null}<br />{t('share.maxSuggest', { mb: app.config.memoryMb, n: suggested })}{/if}
            </div>
          </label>
          <label class="field wide">
            <span>{t('share.notice')}</span>
            <textarea rows="3" maxlength="2000" bind:value={form.publicNotice} placeholder={t('share.noticePlaceholder')}></textarea>
          </label>
        </div>
        <div class="row end">
          <button class="primary" type="submit" disabled={saving}>
            {#if saving}<span class="spinner"></span>{/if}{t('action.save')}
          </button>
        </div>
      </div>
    </section>

    <section class="card">
      <div class="card-title">
        <h2>{t('share.page')}</h2>
        <a href="#/public">{t('share.open')} →</a>
      </div>
      <div class="stack">
        <CopyField value={publicUrl} />
        <ul class="rules small muted">
          <li>{t('share.rule1')}</li>
          <li>{t('share.rule2', { n: saved.pendingLoginLimit })}</li>
          <li>{t('share.rule3')}</li>
          <li>{t('share.rule4')}</li>
        </ul>
      </div>
    </section>
  </form>
{:else}
  <div class="card empty"><span class="spinner"></span></div>
{/if}

<style>
  .intro {
    margin: 0;
  }

  .fields {
    display: grid;
    grid-template-columns: minmax(200px, 280px) 1fr;
    gap: 14px 16px;
  }

  .end {
    justify-content: flex-end;
  }

  .rules {
    margin: 0;
    padding-left: 18px;
  }

  .rules li + li {
    margin-top: 4px;
  }

  @media (max-width: 760px) {
    .fields {
      grid-template-columns: 1fr;
    }
  }
</style>
