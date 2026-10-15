// Light / dark theme, remembered per browser.

export const theme = $state({ name: 'light' });

export function applyTheme(next) {
  theme.name = next;
  document.documentElement.dataset.theme = next;
  try {
    localStorage.setItem('wrapper.theme', next);
  } catch {
    /* ignore */
  }
}

export function initTheme() {
  let saved = null;
  try {
    saved = localStorage.getItem('wrapper.theme');
  } catch {
    /* ignore */
  }
  applyTheme(saved || (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'));
}

export const toggleTheme = () => applyTheme(theme.name === 'dark' ? 'light' : 'dark');
