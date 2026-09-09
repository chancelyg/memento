(() => {
  'use strict';

  const $ = (id) => document.getElementById(id);
  const requestedNext = new URLSearchParams(window.location.search).get('next');
  const next = requestedNext === '/diary' || requestedNext === '/admin' ? requestedNext : '/admin';
  let challenge = null;
  let busy = false;

  function syncButtons() {
    $('loginButton').disabled = busy;
    $('totpButton').disabled = busy;
    $('backToPassword').disabled = busy;
  }

  async function request(method, body) {
    const controller = new AbortController();
    let timedOut = false;
    const timer = setTimeout(() => { timedOut = true; controller.abort(); }, 30000);
    try {
      const response = await fetch('/session', {
        method,
        headers: {
          Accept: 'application/json',
          ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
        },
        credentials: 'same-origin',
        cache: 'no-store',
        signal: controller.signal,
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      });
      const envelope = await response.json().catch(() => null);
      if (!response.ok || !envelope?.success) {
        const error = new Error(typeof envelope?.error === 'string' ? envelope.error : `请求失败（${response.status}），请重试。`);
        error.status = response.status;
        throw error;
      }
      return envelope.data;
    } catch (error) {
      if (method !== 'GET' && !error.status) {
        throw new Error('请求超时或连接中断，登录结果未知。请先重新检查登录状态，不要直接重复提交。');
      }
      if (timedOut) throw new Error('检查登录状态超时（30 秒），请刷新后重试。');
      throw error;
    } finally {
      clearTimeout(timer);
    }
  }

  function showPasswordStep(message) {
    challenge = null;
    $('password').value = '';
    $('code').value = '';
    $('loginError').textContent = '';
    $('totpError').textContent = '';
    $('loginForm').hidden = false;
    $('totpForm').hidden = true;
    $('notice').textContent = message;
  }

  $('loginForm').addEventListener('submit', async (event) => {
    event.preventDefault();
    if (busy || challenge) return;
    busy = true;
    syncButtons();
    $('loginError').textContent = '';
    const password = $('password').value;
    try {
      const data = await request('POST', { username: $('username').value, password });
      if (data?.requires_totp !== true || typeof data.challenge !== 'string' || !/^[a-f0-9]{64}$/i.test(data.challenge)) {
        throw new Error('登录响应无效，请重新验证密码。');
      }
      challenge = data.challenge;
      $('loginForm').hidden = true;
      $('totpForm').hidden = false;
      $('notice').textContent = '密码验证完成，请输入验证码；尚未登录。';
      $('code').focus();
    } catch (error) {
      $('loginError').textContent = error.message || '登录请求失败，请重试。';
      $('password').focus();
    } finally {
      $('password').value = '';
      busy = false;
      syncButtons();
    }
  });

  $('totpForm').addEventListener('submit', async (event) => {
    event.preventDefault();
    if (busy || !challenge) return;
    const code = $('code').value;
    if (!/^[0-9]{6}$/.test(code)) {
      $('totpError').textContent = '请输入 6 位数字验证码。';
      $('code').focus();
      return;
    }
    busy = true;
    syncButtons();
    $('totpError').textContent = '';
    try {
      const data = await request('POST', { challenge, code });
      if (typeof data?.username !== 'string' || typeof data?.csrf_token !== 'string' || !data.csrf_token) {
        throw new Error('登录响应无效，请返回上一步重新登录。');
      }
      challenge = null;
      window.location.replace(next);
    } catch (error) {
      $('totpError').textContent = error.status === 401
        ? '验证码无效或验证请求已失效。可重试当前验证码；若已超过 5 分钟或累计错误 5 次，请返回上一步重新验证密码。'
        : (error.message || '验证请求失败，请重试。');
      $('code').focus();
    } finally {
      $('code').value = '';
      busy = false;
      syncButtons();
    }
  });

  $('backToPassword').addEventListener('click', () => {
    if (busy) return;
    showPasswordStep('请重新验证密码。');
    $('password').focus();
  });

  request('GET').then((data) => {
    if (typeof data?.username !== 'string' || typeof data?.csrf_token !== 'string') throw new Error('会话响应无效，请刷新后重试。');
    window.location.replace(next);
  }).catch((error) => {
    if (error.status === 401) {
      showPasswordStep('登录后可进入日记与管理页面。');
      $('username').focus();
      return;
    }
    $('notice').textContent = error.message || '无法检查登录状态，请刷新后重试。';
  });
})();
