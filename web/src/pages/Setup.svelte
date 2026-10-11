<script>
  import { host } from '../lib/api.js';
  import { app, toast, refreshInfo, engineUp } from '../lib/state.svelte.js';
  import { t } from '../lib/i18n.svelte.js';
  import Icon from '../lib/Icon.svelte';

  refreshInfo();

  const info = $derived(app.info);
  const dl = $derived(app.download);
  const active = $derived(dl && (dl.status === 'downloading' || dl.status === 'extracting'));
  const pct = $derived(dl?.total ? Math.min(100, (dl.downloaded / dl.total) * 100) : null);

  const items = $derived(
    info
      ? [
          { key: 'setup.qemu', ok: !!info.qemu.qemu, detail: info.qemu.qemu },
          { key: 'setup.kernel', ok: info.qemu.kernel },
          { key: 'setup.initrd', ok: info.qemu.initrd },
          { key: 'setup.disk', ok: info.qemu.dataImg, optional: true, detail: info.qemu.dataImg ? null : t('setup.diskHint') },
          ...(info.nativeSupported ? [{ key: 'setup.rootfs', ok: info.rootfsReady }] : []),
        ]
      : [],
  );

  function mb(n) {
    return (n / 1048576).toFixed(1);
  }

  async function download() {
    try {
      app.download = { status: 'downloading', downloaded: 0, total: null, speed: 0 };
      await host.download();
    } catch (e) {
      app.download = null;
      toast(e.message, 'error', 6000);
    }
  }
</script>

{#if info}
  <div class="grid">
    <section class="card">
      <div class="card-title"><h2>{t('nav.setup')}</h2><span class="muted small">{info.os} · {info.arch}</span></div>
      <ul class="items">
        {#each items as it (it.key)}
          <li>
            <span class="ic {it.ok ? 'ok' : it.optional ? 'opt' : 'bad'}"><Icon name={it.ok ? 'check' : it.optional ? 'plus' : 'x'} size={14} /></span>
            <span class="label">{t(it.key)}</span>
            <span class="detail muted small mono">{it.detail || ''}</span>
            <span class="badge {it.ok ? 'ok' : it.optional ? '' : 'err'}">{it.ok ? t('setup.found') : t('setup.missing')}</span>
          </li>
        {/each}
      </ul>
    </section>

    <section class="card">
      <div class="card-title"><h2>{t('setup.package')}</h2></div>
      {#if info.packageName}
        <div class="stack">
          <div class="muted">{t('setup.packageHint', { name: info.packageName })}</div>
          <div class="small mono muted url">{info.packageUrl}</div>
          {#if active}
            <div class="row small">
              <span class="spinner"></span>
              <span>{dl.status === 'extracting' ? t('setup.extracting') : t('setup.downloading')}</span>
              <span class="spacer"></span>
              <span class="muted">
                {mb(dl.downloaded)} MB{#if dl.total} / {mb(dl.total)} MB{/if}
                {#if dl.status === 'downloading' && dl.speed} · {mb(dl.speed)} MB/s{/if}
              </span>
            </div>
            <div class="progress"><div style="width: {dl.status === 'extracting' ? 100 : pct ?? 5}%"></div></div>
          {:else if dl?.status === 'done'}
            <div class="callout info">{t('setup.done')} · {dl.message}</div>
          {:else if dl?.status === 'error'}
            <div class="callout err">{t('setup.error')}: {dl.message}</div>
          {/if}
          {#if engineUp()}<div class="callout warn">{t('setup.stopFirst')}</div>{/if}
          <div class="row">
            <button class="primary" onclick={download} disabled={active || engineUp()}>
              <Icon name="download" size={16} />{t('setup.download')}
            </button>
            <a href="#/settings" class="small">{t('set.mirror')} →</a>
          </div>
        </div>
      {:else}
        <div class="muted">{t('setup.noPackage')}</div>
      {/if}
    </section>

    <section class="card">
      <div class="card-title"><h2>{t('setup.paths')}</h2></div>
      <dl>
        <dt>data</dt><dd class="mono">{info.dataDir}</dd>
        <dt>qemu</dt><dd class="mono">{info.assetsDir}</dd>
      </dl>
    </section>
  </div>
{/if}

<style>
  .items {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .items li {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 0;
    border-top: 1px solid var(--border);
  }

  .items li:first-child {
    border-top: none;
  }

  .ic {
    width: 22px;
    height: 22px;
    border-radius: 50%;
    display: grid;
    place-items: center;
  }

  .ic.ok {
    background: var(--ok-soft);
    color: var(--ok);
  }

  .ic.bad {
    background: var(--err-soft);
    color: var(--err);
  }

  .ic.opt {
    background: var(--surface-2);
    color: var(--muted);
  }

  .label {
    font-weight: 500;
  }

  .detail {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    text-align: right;
  }

  .url {
    word-break: break-all;
  }

  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 6px 14px;
    margin: 0;
  }

  dt {
    color: var(--muted);
  }

  dd {
    margin: 0;
    word-break: break-all;
  }
</style>
