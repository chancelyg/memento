(() => {
  'use strict';

  const themeKey = 'memento-theme';
  const themeButton = document.getElementById('themeToggle');
  const account = document.getElementById('shellAccount');

  function applyTheme(theme) {
    document.documentElement.dataset.theme = theme;
    if (!themeButton) return;
    themeButton.textContent = theme === 'light' ? '☀' : '☾';
    themeButton.setAttribute('aria-pressed', String(theme === 'light'));
  }

  let savedTheme = null;
  try { savedTheme = localStorage.getItem(themeKey); } catch (_) { /* Storage can be unavailable. */ }
  applyTheme(savedTheme === 'light' ? 'light' : 'dark');

  themeButton?.addEventListener('click', () => {
    const theme = document.documentElement.dataset.theme === 'light' ? 'dark' : 'light';
    applyTheme(theme);
    try { localStorage.setItem(themeKey, theme); } catch (_) { /* Theme persistence is optional. */ }
  });

  document.querySelectorAll('.site-brand__icon').forEach((icon) => {
    const failed = () => icon.classList.add('is-failed');
    icon.addEventListener('error', failed, { once: true });
    if (icon.complete && icon.naturalWidth === 0) failed();
  });

  const current = document.body.dataset.page;
  document.querySelector(`[data-nav-page="${current}"]`)?.setAttribute('aria-current', 'page');

  if (!account) return;
  fetch('/session', {
    headers: { Accept: 'application/json' },
    credentials: 'same-origin',
    cache: 'no-store',
  }).then(async (response) => {
    if (response.status === 401) {
      account.href = '/login';
      account.textContent = '登录';
      return;
    }
    if (!response.ok) return;
    const envelope = await response.json().catch(() => null);
    if (!envelope?.success || typeof envelope.data?.username !== 'string') return;
    account.href = '/admin';
    account.textContent = '管理';
    account.title = `已登录：${envelope.data.username}`;
  }).catch(() => {
    // Account discovery must not affect the page's primary behavior.
  });
})();
