<script>
  import { onMount } from 'svelte';
  import { app, init } from './lib/state.svelte.js';
  import { session, pub } from './lib/api.js';
  import { t, locale, setLang } from './lib/i18n.svelte.js';
  import { theme, initTheme, toggleTheme } from './lib/theme.svelte.js';
  import Icon from './lib/Icon.svelte';
  import EngineBadge from './lib/EngineBadge.svelte';
  import AdminLogin from './lib/AdminLogin.svelte';
  import Dashboard from './pages/Dashboard.svelte';
  import Accounts from './pages/Accounts.svelte';
  import Sharing from './pages/Sharing.svelte';
  import Backup from './pages/Backup.svelte';
  import Logs from './pages/Logs.svelte';
  import Settings from './pages/Settings.svelte';
  import Setup from './pages/Setup.svelte';
  import Public from './pages/Public.svelte';

  const allPages = [
    { id: 'dashboard', icon: 'dashboard', component: Dashboard },
    { id: 'accounts', icon: 'accounts', component: Accounts },
    { id: 'sharing', icon: 'globe', component: Sharing },
    { id: 'backup', icon: 'download', component: Backup },
    { id: 'logs', icon: 'logs', component: Logs },
    { id: 'settings', icon: 'settings', component: Settings, gui: true },
    { id: 'setup', icon: 'package', component: Setup, gui: true },
  ];
  // A headless agent has no engine or host settings.
  const pages = $derived(allPages.filter((p) => !p.gui || app.mode === 'gui'));

  const hashId = () => location.hash.replace(/^#\/?/, '').split('?')[0];
  let hash = $state(hashId());
  const route = $derived(pages.some((p) => p.id === hash) ? hash : 'dashboard');
  const current = $derived(pages.find((p) => p.id === route));

  // 'loading' | 'admin' | 'public' | 'signin'
  let view = $state('loading');
  let offline = $state(false);
  let isAdmin = $state(false);

  async function decide() {
    let who = null;
    try {
      who = await session.whoami();
    } catch {
      offline = true;
      view = 'signin';
      return;
    }
    isAdmin = who.admin;
    if (who.admin) {
      init(who);
      view = 'admin';
      return;
    }
    try {
      const ov = await pub.overview();
      view = ov.publicMode ? 'public' : 'signin';
    } catch {
      // The engine is stopped: only an administrator can start it.
      offline = true;
      view = 'signin';
    }
  }

  async function signOut() {
    try {
      await session.signOut();
    } finally {
      location.hash = '';
      location.reload();
    }
  }

  onMount(() => {
    initTheme();
    setLang(locale.lang);
    const onHash = () => (hash = hashId());
    addEventListener('hashchange', onHash);
    decide();
    return () => removeEventListener('hashchange', onHash);
  });
</script>

{#if view === 'loading'}
  <div class="loading"><span class="spinner"></span></div>
{:else if view === 'signin'}
  <AdminLogin {offline} onback={offline ? null : () => (view = 'public')} />
{:else if view === 'public' || hash === 'public'}
  <Public admin={isAdmin} onadmin={() => (view = 'signin')} />
{:else}
<div class="shell">
  <aside>
    <div class="brand">
      <img src="/favicon.svg" alt="" width="28" height="28" />
      <div>
        <div class="name">wrapper</div>
        {#if app.info}<div class="ver">{t('common.version', { v: app.info.version })}</div>{/if}
        {#if app.info?.update}
          <a class="upd" href={app.info.update.notes} target="_blank" rel="noreferrer" title={t('common.updateHint')}>
            {t('common.updateAvailable', { v: app.info.update.version })}
          </a>
        {/if}
      </div>
    </div>
    <nav>
      {#each pages as p (p.id)}
        <a href={`#/${p.id}`} class:active={route === p.id}>
          <Icon name={p.icon} />
          <span>{t(`nav.${p.id}`)}</span>
          {#if p.id === 'accounts' && app.accounts.length}
            <span class="count">{app.accounts.length}</span>
          {/if}
        </a>
      {/each}
    </nav>
    <div class="aside-foot">
      {#if app.mode === 'gui'}<EngineBadge />{/if}
      <div class="row">
        <button class="ghost icon" title="Theme" onclick={toggleTheme}>
          <Icon name={theme.name === 'dark' ? 'sun' : 'moon'} />
        </button>
        <button class="ghost small" onclick={() => setLang(locale.lang === 'zh' ? 'en' : 'zh')}>
          <Icon name="globe" size={16} />
          {locale.lang === 'zh' ? 'English' : '中文'}
        </button>
        {#if app.signedIn}
          <button class="ghost icon" title={t('signin.signOut')} onclick={signOut}><Icon name="logout" /></button>
        {/if}
      </div>
    </div>
  </aside>

  <main>
    {#if !app.hostConnected && app.status}
      <div class="callout warn offline">{t('common.hostOffline')}</div>
    {/if}
    <header>
      <h1>{t(`nav.${route}`)}</h1>
    </header>
    {#key route}
      <current.component />
    {/key}
  </main>
</div>
{/if}

<div class="toasts">
  {#each app.toasts as toast (toast.id)}
    <div class="toast {toast.kind}">{toast.message}</div>
  {/each}
</div>

<style>
  .loading {
    min-height: 100vh;
    display: grid;
    place-items: center;
  }

  .shell {
    display: grid;
    grid-template-columns: 228px 1fr;
    min-height: 100vh;
  }

  aside {
    position: sticky;
    top: 0;
    height: 100vh;
    display: flex;
    flex-direction: column;
    gap: 18px;
    padding: 18px 14px;
    border-right: 1px solid var(--border);
    background: var(--surface);
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 6px;
  }

  .brand .name {
    font-weight: 700;
    font-size: 16px;
    letter-spacing: -0.01em;
  }

  .brand .ver {
    font-size: 11.5px;
    color: var(--muted);
  }
  .brand .upd {
    display: block;
    font-size: 11.5px;
    color: var(--accent);
  }

  nav {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  nav a {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 36px;
    padding: 0 10px;
    border-radius: 9px;
    color: var(--muted);
    font-weight: 500;
  }

  nav a:hover {
    background: var(--surface-2);
    color: var(--text);
  }

  nav a.active {
    background: var(--accent-soft);
    color: var(--accent);
  }

  .count {
    margin-left: auto;
    font-size: 11.5px;
    padding: 0 7px;
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--muted);
  }

  .aside-foot {
    margin-top: auto;
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 0 4px;
  }

  main {
    padding: 24px 28px 48px;
    max-width: 1180px;
    width: 100%;
    min-width: 0;
  }

  header {
    margin-bottom: 18px;
  }

  header h1 {
    font-size: 22px;
    letter-spacing: -0.015em;
  }

  .offline {
    margin-bottom: 14px;
  }

  .toasts {
    position: fixed;
    right: 20px;
    bottom: 20px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    z-index: 100;
  }

  .toast {
    max-width: 380px;
    padding: 10px 14px;
    border-radius: 10px;
    background: var(--text);
    color: var(--bg);
    box-shadow: 0 6px 24px rgba(0, 0, 0, 0.18);
    font-size: 13px;
  }

  .toast.error {
    background: var(--err);
    color: #fff;
  }

  .toast.ok {
    background: var(--ok);
    color: #fff;
  }

  @media (max-width: 760px) {
    .shell {
      grid-template-columns: minmax(0, 1fr);
      grid-template-rows: auto 1fr;
    }

    aside {
      position: static;
      height: auto;
      flex-direction: row;
      flex-wrap: wrap;
      align-items: center;
      gap: 10px;
      padding: 10px 12px;
      border-right: none;
      border-bottom: 1px solid var(--border);
    }

    nav {
      order: 3;
      width: 100%;
      flex-direction: row;
      overflow-x: auto;
    }

    nav a span:not(.count) {
      white-space: nowrap;
    }

    .aside-foot {
      margin: 0 0 0 auto;
      flex-direction: row;
      align-items: center;
      gap: 4px;
    }

    .aside-foot :global(.badge) {
      display: none;
    }

    main {
      padding: 16px 14px 40px;
    }
  }
</style>
