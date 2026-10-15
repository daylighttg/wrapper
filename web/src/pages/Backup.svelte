<script>
  import { agent } from '../lib/api.js';
  import { app, toast, engineUp, refreshAgent } from '../lib/state.svelte.js';
  import { t } from '../lib/i18n.svelte.js';
  import Icon from '../lib/Icon.svelte';

  const up = $derived(engineUp());

  let file = $state(null);
  let mode = $state('merge');
  let withSettings = $state(true);
  let pull = $state({ url: '', token: '' });
  let busy = $state('');
  let report = $state(null);

  function confirmMode() {
    return mode !== 'replace' || confirm(t('backup.confirmReplace', { n: app.accounts.length }));
  }

  async function run(kind, action) {
    if (!confirmMode()) return;
    busy = kind;
    report = null;
    try {
      report = await action();
      toast(t('backup.restored', { n: report.accounts.length }), 'ok');
      refreshAgent();
    } catch (e) {
      toast(e.message, 'error', 8000);
    } finally {
      busy = '';
    }
  }

  const restoreFile = (e) => {
    e.preventDefault();
    run('file', () => agent.restore(file, mode, withSettings));
  };
  const restorePull = (e) => {
    e.preventDefault();
    run('pull', () => agent.pull(pull.url.trim(), pull.token.trim(), mode, withSettings));
  };
</script>

{#if !up}
  <div class="card empty">{t('acc.engineOff')}</div>
{:else}
  <div class="grid">
    <section class="card">
      <div class="card-title"><h2>{t('backup.download')}</h2></div>
      <div class="stack">
        <p class="muted small">{t('backup.downloadHint')}</p>
        <div class="row">
          <a class="btn primary" href="/api/backup" download><Icon name="download" size={16} />{t('backup.downloadButton')}</a>
          <span class="small muted">{t('backup.contains', { n: app.accounts.length })}</span>
        </div>
        <div class="callout warn small">{t('backup.private')}</div>
      </div>
    </section>

    <section class="card">
      <div class="card-title"><h2>{t('backup.restore')}</h2></div>
      <div class="stack">
        <div class="fields">
          <label class="field">
            <span>{t('backup.mode')}</span>
            <select bind:value={mode}>
              <option value="merge">{t('backup.merge')}</option>
              <option value="replace">{t('backup.replace')}</option>
            </select>
            <div class="hint">{mode === 'merge' ? t('backup.mergeHint') : t('backup.replaceHint')}</div>
          </label>
          <label class="row check"><input type="checkbox" bind:checked={withSettings} />{t('backup.withSettings')}</label>
        </div>

        <form class="sub stack" onsubmit={restoreFile}>
          <h3>{t('backup.fromFile')}</h3>
          <input type="file" accept=".gz,.tgz,application/gzip" onchange={(e) => (file = e.currentTarget.files?.[0] || null)} />
          <div class="row end">
            <button class="primary" type="submit" disabled={!file || !!busy}>
              {#if busy === 'file'}<span class="spinner"></span>{/if}{t('backup.restoreButton')}
            </button>
          </div>
        </form>

        <form class="sub stack" onsubmit={restorePull}>
          <h3>{t('backup.fromServer')}</h3>
          <p class="muted small">{t('backup.fromServerHint')}</p>
          <div class="fields">
            <label class="field">
              <span>{t('backup.oldUrl')}</span>
              <input bind:value={pull.url} placeholder="http://203.0.113.5:12340" required />
            </label>
            <label class="field">
              <span>{t('backup.oldToken')}</span>
              <input type="password" bind:value={pull.token} autocomplete="off" />
            </label>
          </div>
          <div class="row end">
            <button class="primary" type="submit" disabled={!pull.url.trim() || !!busy}>
              {#if busy === 'pull'}<span class="spinner"></span>{/if}{t('backup.pullButton')}
            </button>
          </div>
        </form>

        {#if report}
          <div class="callout ok-box">
            <b>{t('backup.restored', { n: report.accounts.length })}</b>
            · {t('backup.reportCounts', { added: report.added, replaced: report.replaced, removed: report.removed })}
            {#if report.accounts.length}<div class="small mono">{report.accounts.join(', ')}</div>{/if}
            {#if report.skipped.length}<div class="small">{t('backup.skipped')}: {report.skipped.join(', ')}</div>{/if}
            {#if report.settings}<div class="small">{t('backup.settingsRestored')}</div>{/if}
          </div>
        {/if}
      </div>
    </section>

    <section class="card">
      <div class="card-title"><h2>{t('backup.howto')}</h2></div>
      <ol class="small muted steps">
        <li>{t('backup.step1')}</li>
        <li>{t('backup.step2')}</li>
        <li>{t('backup.step3')}</li>
      </ol>
      <pre class="logbox small">wrapper backup --url http://old:12340 --token &lt;token&gt;
wrapper restore --url http://new:12340 --token &lt;token&gt; wrapper-backup-*.tar.gz
wrapper restore --url http://new:12340 --token &lt;token&gt; --from http://old:12340 --from-token &lt;old token&gt;</pre>
    </section>
  </div>
{/if}

<style>
  .fields {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
    gap: 12px 16px;
    align-items: start;
  }

  .check {
    margin-top: 24px;
  }

  .sub {
    border-top: 1px solid var(--border);
    padding-top: 14px;
  }

  .sub h3 {
    font-size: 14px;
  }

  .sub p {
    margin: 0;
  }

  input[type='file'] {
    height: auto;
    padding: 8px 10px;
  }

  .end {
    justify-content: flex-end;
  }

  a.btn.primary {
    background: var(--accent);
    color: #fff;
    border-color: var(--accent);
  }

  .ok-box {
    background: var(--ok-soft);
    color: var(--ok);
  }

  .steps {
    margin: 0 0 12px;
    padding-left: 18px;
  }

  .steps li + li {
    margin-top: 4px;
  }

  pre {
    margin: 0;
    overflow-x: auto;
    white-space: pre;
  }
</style>
