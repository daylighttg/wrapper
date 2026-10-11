<script>
  import { host } from '../lib/api.js';
  import { app, toast, refreshInfo } from '../lib/state.svelte.js';
  import { t } from '../lib/i18n.svelte.js';
  import { busy, engineAction } from '../lib/engine.svelte.js';
  import CopyField from '../lib/CopyField.svelte';

  // Edit a copy; save sends the whole object.
  let form = $state(app.config ? structuredClone($state.snapshot(app.config)) : null);
  let saving = $state(false);
  let needsRestart = $state(false);

  $effect(() => {
    if (!form && app.config) form = structuredClone($state.snapshot(app.config));
  });

  const native = $derived(app.info?.nativeSupported);
  const hostChoice = $derived(form && ['127.0.0.1', '0.0.0.0'].includes(form.apiHost) ? form.apiHost : 'custom');

  async function save(e) {
    e?.preventDefault();
    saving = true;
    try {
      const res = await host.saveConfig($state.snapshot(form));
      app.config = res.config;
      form = structuredClone(res.config);
      needsRestart = res.restartRequired;
      toast(t('set.saved'), 'ok');
      refreshInfo();
    } catch (err) {
      toast(err.message, 'error', 6000);
    } finally {
      saving = false;
    }
  }

  function regenerate() {
    form.apiToken = '';
    save();
  }

  async function restart() {
    await engineAction('restart');
    needsRestart = false;
  }
</script>

{#if form}
  <form class="grid" onsubmit={save}>
    {#if needsRestart}
      <div class="callout warn row">
        <span>{t('set.restartRequired')}</span>
        <span class="spacer"></span>
        <button type="button" class="small" onclick={restart} disabled={busy.engine}>{t('action.restart')}</button>
      </div>
    {/if}

    <section class="card">
      <div class="card-title"><h2>{t('set.engineSection')}</h2></div>
      <div class="fields">
        <label class="field wide">
          <span>{t('set.engine')}</span>
          <select bind:value={form.engine}>
            <option value="qemu">{t('engine.qemu')}</option>
            <option value="native" disabled={!native}>{t('engine.native')}</option>
          </select>
          {#if !native}<div class="hint">{t('set.nativeUnsupported')}</div>{/if}
        </label>
        {#if form.engine === 'qemu'}
          <label class="field">
            <span>{t('set.memory')}</span>
            <input type="number" min="256" max="65536" step="128" bind:value={form.memoryMb} />
            <div class="hint">{t('set.memoryHint')}</div>
          </label>
          <label class="field">
            <span>{t('set.smp')}</span>
            <input type="number" min="1" max="64" bind:value={form.smp} />
          </label>
          <label class="field">
            <span>{t('set.accel')}</span>
            <select bind:value={form.accel}>
              {#each ['auto', 'kvm', 'whpx', 'hvf', 'tcg'] as a (a)}<option value={a}>{a}</option>{/each}
            </select>
          </label>
          <label class="field">
            <span>{t('set.disk')}</span>
            <input type="number" min="64" step="64" bind:value={form.diskSizeMb} />
            <div class="hint">{t('set.diskHint')}</div>
          </label>
        {/if}
      </div>
    </section>

    <section class="card">
      <div class="card-title"><h2>{t('set.network')}</h2></div>
      <div class="fields">
        <label class="field">
          <span>{t('set.apiHost')}</span>
          <select value={hostChoice} onchange={(e) => (form.apiHost = e.currentTarget.value === 'custom' ? '' : e.currentTarget.value)}>
            <option value="127.0.0.1">{t('set.apiHostLocal')}</option>
            <option value="0.0.0.0">{t('set.apiHostLan')}</option>
            <option value="custom">…</option>
          </select>
          {#if hostChoice === 'custom'}<input class="mt" bind:value={form.apiHost} placeholder="192.168.1.10" />{/if}
        </label>
        <label class="field">
          <span>{t('set.apiPort')}</span>
          <input type="number" min="1" max="65535" bind:value={form.apiPort} />
        </label>
        <label class="field wide">
          <span>{t('set.proxy')}</span>
          <input bind:value={form.proxy} placeholder="http://127.0.0.1:7890" />
          <div class="hint">{t('set.proxyHint')}</div>
        </label>
      </div>
    </section>

    <section class="card">
      <div class="card-title"><h2>{t('set.lite')}</h2></div>
      <div class="fields">
        <label class="field wide">
          <span>{t('set.deviceInfo')}</span>
          <input bind:value={form.deviceInfo} placeholder="Music/5.0.2/Android/10/Pixel 8/7663314/en-US/en-US/…" />
          <div class="hint">{t('login.deviceInfoHint')}</div>
        </label>
        <label class="field">
          <span>{t('set.logLevel')}</span>
          <select bind:value={form.liteLogLevel}>
            {#each ['debug', 'info', 'warn', 'error'] as l (l)}<option value={l}>{l}</option>{/each}
          </select>
        </label>
        <label class="field">
          <span>{t('set.refresh')}</span>
          <input type="number" min="60" bind:value={form.tokenRefreshInterval} />
        </label>
        <label class="field">
          <span>{t('set.concurrency')}</span>
          <input type="number" min="1" max="16" bind:value={form.instanceConcurrency} />
        </label>
      </div>
    </section>

    <section class="card">
      <div class="card-title"><h2>{t('set.security')}</h2></div>
      <div class="field">
        <span class="small muted">{t('set.apiToken')}</span>
        <div class="row token">
          <div class="spacer"><CopyField value={form.apiToken} secret /></div>
          <button type="button" onclick={regenerate}>{t('set.regenerate')}</button>
        </div>
        <div class="hint">{t('dash.tokenHint')}</div>
      </div>
    </section>

    <section class="card">
      <div class="card-title"><h2>{t('set.misc')}</h2></div>
      <div class="stack">
        <label class="row"><input type="checkbox" bind:checked={form.autostart} />{t('set.autostart')}</label>
        <label class="row"><input type="checkbox" bind:checked={form.mirror} />{t('set.mirror')}</label>
        <label class="field">
          <span>{t('set.packageBase')}</span>
          <input bind:value={form.packageBaseUrl} />
        </label>
      </div>
    </section>

    <div class="row savebar">
      <span class="spacer"></span>
      <button type="button" onclick={() => (form = structuredClone($state.snapshot(app.config)))}>{t('action.cancel')}</button>
      <button class="primary" type="submit" disabled={saving}>
        {#if saving}<span class="spinner"></span>{/if}{t('action.save')}
      </button>
    </div>
  </form>
{/if}

<style>
  .fields {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: 14px 16px;
  }

  .wide {
    grid-column: 1 / -1;
  }

  .mt {
    margin-top: 6px;
  }

  .token {
    margin-top: 4px;
    flex-wrap: nowrap;
  }

  .token .spacer {
    min-width: 0;
  }

  .savebar {
    position: sticky;
    bottom: 0;
    padding: 12px 0;
    background: linear-gradient(transparent, var(--bg) 40%);
  }
</style>
