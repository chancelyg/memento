(() => {
  'use strict';

  const $ = (id) => document.getElementById(id);
  let csrfToken = null;
  let busy = false;

  function redirectToLogin() {
    csrfToken = null;
    window.location.replace('/login?next=%2Fadmin');
  }

  async function request(path, { method = 'GET', body } = {}) {
    const headers = { Accept: 'application/json' };
    if (method !== 'GET') {
      headers['Content-Type'] = 'application/json';
      headers['X-CSRF-Token'] = csrfToken;
    }
    let response;
    try {
      response = await fetch(path, {
        method,
        headers,
        credentials: 'same-origin',
        cache: 'no-store',
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      });
    } catch (error) {
      if (method !== 'GET') throw new Error('连接中断，保存结果未知。请先重新读取设置，不要直接重复提交。');
      throw error;
    }
    const envelope = await response.json().catch(() => null);
    if (!response.ok || !envelope?.success) {
      const error = new Error(typeof envelope?.error === 'string' ? envelope.error : `请求失败（${response.status}），请重试。`);
      error.status = response.status;
      throw error;
    }
    return envelope.data;
  }

  function readSettings(data) {
    if (typeof data?.name !== 'string' || typeof data?.slogan !== 'string' || typeof data?.icon !== 'string') {
      throw new Error('站点设置响应无效，请刷新后重试。');
    }
    $('siteName').value = data.name;
    $('siteSlogan').value = data.slogan;
    $('siteIcon').value = data.icon;
  }

  async function loadSettings() {
    $('adminStatus').textContent = '正在加载站点设置…';
    $('settingsError').textContent = '';
    try {
      readSettings(await request('/private/settings/site'));
      $('settingsForm').hidden = false;
      $('adminStatus').textContent = '站点设置已加载。';
    } catch (error) {
      if (error.status === 401) return redirectToLogin();
      $('adminStatus').textContent = '站点设置加载失败。';
      $('settingsError').textContent = error.message || '无法加载站点设置，请刷新后重试。';
    }
  }

  $('settingsForm').addEventListener('submit', async (event) => {
    event.preventDefault();
    if (busy || !csrfToken) return;
    const body = {
      name: $('siteName').value.trim(),
      slogan: $('siteSlogan').value.trim(),
      icon: $('siteIcon').value.trim(),
    };
    if (!body.name || !body.icon) {
      $('settingsError').textContent = '站点名称和图标不能为空；标语可以留空。';
      (!body.name ? $('siteName') : $('siteIcon')).focus();
      return;
    }
    busy = true;
    $('saveSettings').disabled = true;
    $('settingsError').textContent = '';
    $('adminStatus').textContent = '正在保存…';
    try {
      const data = await request('/private/settings/site', { method: 'PUT', body });
      if (data !== null && data !== undefined) readSettings(data);
      $('adminStatus').textContent = '站点信息已保存；之后打开或刷新的页面会使用新设置。';
    } catch (error) {
      if (error.status === 401) return redirectToLogin();
      $('adminStatus').textContent = '保存失败。';
      $('settingsError').textContent = error.message || '无法保存站点设置，请重试。';
    } finally {
      busy = false;
      $('saveSettings').disabled = false;
    }
  });

  $('logout').addEventListener('click', async () => {
    if (busy || !csrfToken || !window.confirm('退出登录？')) return;
    busy = true;
    $('logout').disabled = true;
    $('saveSettings').disabled = true;
    $('settingsError').textContent = '';
    $('adminStatus').textContent = '正在退出…';
    try {
      await request('/session', { method: 'DELETE' });
      csrfToken = null;
      window.location.replace('/login?next=%2Fadmin');
    } catch (error) {
      $('adminStatus').textContent = '未确认退出。';
      $('settingsError').textContent = error.message || '退出失败，请重试。';
      busy = false;
      $('logout').disabled = false;
      $('saveSettings').disabled = false;
    }
  });

  request('/session').then((data) => {
    if (typeof data?.username !== 'string' || typeof data?.csrf_token !== 'string' || !data.csrf_token) {
      throw new Error('会话响应无效，请刷新后重试。');
    }
    csrfToken = data.csrf_token;
    return loadSettings();
  }).catch((error) => {
    if (error.status === 401) return redirectToLogin();
    $('adminStatus').textContent = error.message || '无法检查登录状态，请刷新后重试。';
  });
})();
