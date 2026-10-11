<script>
  import { app } from '../lib/state.svelte.js';
  import { t } from '../lib/i18n.svelte.js';

  let tab = $state('agent');
  let source = $state('');
  let level = $state('');
  let query = $state('');
  let follow = $state(true);
  let box = $state(null);

  const levels = ['debug', 'info', 'warn', 'error'];
  const names = $derived(Object.fromEntries(app.accounts.map((a) => [a.id, a.username])));

  function sourceLabel(src) {
    if (src === 'agent') return 'agent';
    const id = src.startsWith('login:') ? src.slice(6) : src;
    const name = names[id] || app.logins[id]?.username || id.slice(0, 8);
    return src.startsWith('login:') ? `login ${name}` : name;
  }

  function matchesSource(src) {
    if (!source) return true;
    return src === source || src === `login:${source}`;
  }

  const lines = $derived.by(() => {
    const q = query.trim().toLowerCase();
    if (tab === 'console') {
      return app.console.filter((l) => !q || l.text.toLowerCase().includes(q)).slice(-2000);
    }
    const min = levels.indexOf(level);
    return app.logs
      .filter((l) => matchesSource(l.source))
      .filter((l) => min < 0 || levels.indexOf(l.level) >= min)
      .filter((l) => !q || l.msg.toLowerCase().includes(q))
      .slice(-2000);
  });

  $effect(() => {
    lines.length;
    if (follow && box) queueMicrotask(() => (box.scrollTop = box.scrollHeight));
  });

  const time = (ts) => new Date(ts).toLocaleTimeString([], { hour12: false });
</script>

<div class="card">
  <div class="row bar">
    <div class="tabs">
      <button class:active={tab === 'agent'} onclick={() => (tab = 'agent')}>{t('logs.agent')}</button>
      <button class:active={tab === 'console'} onclick={() => (tab = 'console')}>{t('logs.console')}</button>
    </div>
    {#if tab === 'agent'}
      <select bind:value={source} style="width: 200px">
        <option value="">{t('logs.allSources')}</option>
        <option value="agent">{t('logs.agentSource')}</option>
        {#each app.accounts as a (a.id)}<option value={a.id}>{a.username}</option>{/each}
      </select>
      <select bind:value={level} style="width: 130px">
        <option value="">{t('logs.allLevels')}</option>
        {#each levels as l (l)}<option value={l}>{l}+</option>{/each}
      </select>
    {/if}
    <input placeholder={t('logs.search')} bind:value={query} style="width: 200px" />
    <span class="spacer"></span>
    <label class="row small"><input type="checkbox" bind:checked={follow} />{t('logs.follow')}</label>
  </div>

  <div class="logbox view" bind:this={box}>
    {#if lines.length === 0}
      <div class="muted">{t('logs.empty')}</div>
    {:else if tab === 'agent'}
      {#each lines as l (l.seq)}
        <div class="line {l.level}">
          <span class="ts">{time(l.ts)}</span>
          <span class="lv">{l.level.toUpperCase()}</span>
          <span class="src" title={l.source}>{sourceLabel(l.source)}</span>
          <span class="msg">{l.msg}</span>
        </div>
      {/each}
    {:else}
      {#each lines as l (l.seq)}
        <div class="line"><span class="ts">{time(l.ts)}</span><span class="msg">{l.text}</span></div>
      {/each}
    {/if}
  </div>
</div>

<style>
  .bar {
    margin-bottom: 12px;
  }

  .tabs {
    display: inline-flex;
    background: var(--surface-2);
    border-radius: 10px;
    padding: 3px;
  }

  .tabs button {
    height: 28px;
    border: none;
    background: transparent;
    color: var(--muted);
  }

  .tabs button.active {
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow);
  }

  .view {
    height: calc(100vh - 230px);
    min-height: 320px;
  }

  .line {
    display: flex;
    gap: 10px;
  }

  .ts {
    color: var(--muted);
    flex: none;
  }

  .lv {
    flex: none;
    width: 44px;
    color: var(--muted);
  }

  .src {
    flex: none;
    width: 150px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--info);
  }

  .msg {
    min-width: 0;
  }

  .warn .lv,
  .warn .msg {
    color: var(--warn);
  }

  .error .lv,
  .error .msg {
    color: var(--err);
  }

  .debug {
    opacity: 0.7;
  }

  @media (max-width: 760px) {
    .src {
      width: 90px;
    }

    .lv {
      display: none;
    }
  }
</style>
