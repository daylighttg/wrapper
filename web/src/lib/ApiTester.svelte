<script>
  import { t } from './i18n.svelte.js';

  const endpoints = [
    { path: '/status', params: [] },
    { path: '/m3u8', params: ['adamId'] },
    { path: '/webplayback', params: ['adamId'] },
    { path: '/lyrics', params: ['adamId', 'language', 'syllable'] },
    { path: '/key', params: ['adamId', 'uri'] },
  ];
  let endpoint = $state('/status');
  let values = $state({ adamId: '', language: 'en', syllable: '1', uri: '' });
  let result = $state('');
  let elapsed = $state(null);
  let running = $state(false);
  const params = $derived(endpoints.find((e) => e.path === endpoint).params);

  async function send() {
    running = true;
    const q = new URLSearchParams();
    for (const p of params) if (values[p]) q.set(p, values[p]);
    const started = performance.now();
    try {
      const res = await fetch(`/api/public${endpoint}?${q}`);
      const text = await res.text();
      try {
        const json = JSON.parse(text);
        // Long fields (m3u8, ctx/state) would flood the panel.
        result = JSON.stringify(json, (k, v) => (typeof v === 'string' && v.length > 600 ? `${v.slice(0, 600)}… (${v.length} chars)` : v), 2);
      } catch {
        result = text;
      }
    } catch (e) {
      result = String(e);
    }
    elapsed = Math.round(performance.now() - started);
    running = false;
  }
</script>

<section class="card tester">
  <div class="card-title">
    <h2>{t('dash.tester')}</h2>
    <span class="muted small">{t('dash.testerHint')}</span>
  </div>
  <div class="row form">
    <select bind:value={endpoint} style="width: 160px">
      {#each endpoints as e (e.path)}<option value={e.path}>{e.path}</option>{/each}
    </select>
    {#each params as p (p)}
      <input placeholder={p} bind:value={values[p]} style="width: {p === 'uri' ? 300 : 130}px" />
    {/each}
    <button class="primary" onclick={send} disabled={running}>
      {#if running}<span class="spinner"></span>{/if}{t('action.send')}
    </button>
    {#if elapsed !== null}<span class="muted small">{elapsed} ms</span>{/if}
  </div>
  {#if result}
    <pre class="logbox">{result}</pre>
  {/if}
</section>

<style>
  .tester {
    margin-top: 16px;
  }

  .form input,
  .form select {
    max-width: 100%;
  }

  pre {
    margin: 12px 0 0;
    max-height: 320px;
  }
</style>
