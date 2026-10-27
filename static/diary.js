(() => {
  'use strict';

  const $ = (id) => document.getElementById(id);
  let session = null;
  let busy = false;
  let page = 1;
  let filters = { sort: 'desc' };
  let listSequence = 0;
  let listController;
  let versionController;
  let listLoading = false;
  let editing = null;
  let latest = null;
  let conflicted = false;
  const drafts = window.DiaryDrafts;
  const draftMessages = { new: '', edit: '' };
  const draftErrors = { new: false, edit: false };

  function draftStatus(kind, message, error = false) {
    draftMessages[kind] = message;
    draftErrors[kind] = error;
    $('draftStatus').textContent = Object.values(draftMessages).filter(Boolean).join(' ');
    $('draftStatus').classList.toggle('error', Object.values(draftErrors).some(Boolean));
  }

  function clearDraft(kind) {
    try {
      localStorage.removeItem(drafts[`${kind}Key`]);
      draftStatus(kind, `${kind === 'new' ? '新建' : '编辑'}本地草稿已清除。`);
      return true;
    } catch {
      draftStatus(kind, `${kind === 'new' ? '新建' : '编辑'}本地草稿清除失败，浏览器中可能仍有明文正文；请手动清理本站本地存储。`, true);
      return false;
    }
  }

  function saveDraft(kind) {
    if (!session || (kind === 'edit' && !editing)) return;
    const label = kind === 'new' ? '新建' : '编辑';
    const value = $(kind === 'new' ? 'newContent' : 'editContent').value;
    if (kind === 'new' && value === '') { clearDraft(kind); return; }
    try {
      localStorage.setItem(drafts[`${kind}Key`], kind === 'new' ? value : JSON.stringify({
        id: editing.id, create_date: editing.create_date, version: editing.version, content: value,
      }));
      draftStatus(kind, `${label}草稿已保存到此浏览器本地，不代表服务器已保存。`);
    } catch {
      draftStatus(kind, `${label}草稿本地保存失败：存储不可用或空间已满。正文仅保留在当前页面，请先复制备份，勿刷新或离开。`, true);
    }
  }

  function restoreDrafts() {
    try {
      $('newContent').value = localStorage.getItem(drafts.newKey) ?? '';
      draftStatus('new', $('newContent').value ? '已恢复新建本地草稿；若上次提交结果未知，请先核实记录，不要重复提交。' : '新建正文输入时将保存到此浏览器本地。');
    } catch {
      draftStatus('new', '无法读取新建本地草稿：本地存储不可用，请勿依赖自动保存。', true);
    }
    try {
      const raw = localStorage.getItem(drafts.editKey);
      if (raw === null) {
        closeEditor();
        draftStatus('edit', '');
      } else {
        const item = JSON.parse(raw);
        if (!item || !Number.isSafeInteger(item.id) || item.id <= 0
          || !Number.isSafeInteger(item.version) || item.version <= 0
          || typeof item.create_date !== 'string' || !/^\d{4}-\d{2}-\d{2}$/.test(item.create_date)
          || typeof item.content !== 'string') throw new Error('invalid draft');
        showEditor(item);
        draftStatus('edit', '已恢复编辑本地草稿并保留原版本号；若上次提交结果未知，请先核实记录。');
      }
    } catch {
      draftStatus('edit', '编辑草稿读取失败：存储不可用或草稿数据损坏，未清除本地数据；请先备份，勿依赖自动恢复。', true);
    }
    count('newContent', 'newCount');
  }

  function syncButtons() {
    document.querySelectorAll('[data-write]').forEach((button) => {
      button.disabled = busy || listLoading || !session;
    });
    $('saveEdit').disabled = busy || listLoading || !session || conflicted;
    $('refreshVersion').disabled = busy || listLoading || !session || Boolean(versionController);
    $('logout').disabled = busy || !session;
    $('logout').title = busy ? '正在提交请求，已提交的写入不能通过退出取消，请等待结果。' : '退出登录';
  }

  function invalidateList() {
    listSequence += 1;
    listController?.abort();
    listLoading = false;
    $('entries').setAttribute('aria-busy', 'false');
  }

  function redirectToLogin() {
    session = null;
    invalidateList();
    versionController?.abort();
    $('workspace').hidden = true;
    window.location.replace('/login?next=%2Fdiary');
  }

  async function request(path, { method = 'GET', body, version, signal } = {}) {
    const headers = { Accept: 'application/json' };
    if (method !== 'GET') {
      headers['Content-Type'] = 'application/json';
      if (session) headers['X-CSRF-Token'] = session.csrf_token;
    }
    if (version !== undefined) headers['If-Match'] = `"${version}"`;
    const controller = new AbortController();
    const abort = () => controller.abort();
    signal?.addEventListener('abort', abort, { once: true });
    if (signal?.aborted) abort();
    let timedOut = false;
    const timer = setTimeout(() => { timedOut = true; controller.abort(); }, 30000);
    try {
      const response = await fetch(path, {
        method, headers, credentials: 'same-origin', cache: 'no-store', signal: controller.signal,
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      });
      if (response.status === 204) return null;
      const envelope = await response.json().catch((error) => {
        if (controller.signal.aborted || method !== 'GET') throw error;
        return null;
      });
      if (!response.ok || !envelope?.success) {
        const error = new Error(typeof envelope?.error === 'string' ? envelope.error : `请求失败（${response.status}），请重试。`);
        error.status = response.status;
        throw error;
      }
      return envelope.data;
    } catch (error) {
      // 中断等待不等于撤回写入；不自动重试结果未知的请求。
      if (method !== 'GET' && !error.status) {
        throw new Error('请求超时或连接中断，服务端操作结果未知。草稿已保留；请先核实结果，不要直接重复提交。');
      }
      if (timedOut) throw new Error('读取超时（30 秒），请重试。');
      throw error;
    } finally {
      clearTimeout(timer);
      signal?.removeEventListener('abort', abort);
    }
  }

  function failure(error, target) {
    let message = error.message || '网络请求失败，请重试。';
    if (error.status === 412 || error.status === 428) {
      message = '版本已变化或缺少版本条件，请刷新并对照最新内容后再操作。编辑草稿已保留。';
    }
    $(target).textContent = message;
    if (target === 'listError') $('retryList').hidden = false;
    if (error.status === 401 && session) redirectToLogin();
  }

  function count(textarea, output) {
    const length = Array.from($(textarea).value.trim()).length;
    $(output).textContent = `${length} / 10000 字符（去除首尾空白后）`;
    $(textarea).setAttribute('aria-invalid', String(length > 10000));
  }

  function content(textarea) {
    const value = $(textarea).value.trim();
    if (!value || Array.from(value).length > 10000) {
      $(textarea).focus();
      throw new Error('正文去除首尾空白后须为 1 至 10000 个 Unicode 字符。');
    }
    return value;
  }

  async function mutate(target, action) {
    if (busy || listLoading || !session) return;
    busy = true;
    invalidateList();
    versionController?.abort();
    syncButtons();
    $(target).textContent = '';
    try { await action(); } catch (error) { failure(error, target); }
    finally { busy = false; syncButtons(); }
  }

  function closeEditor() {
    versionController?.abort();
    editing = null;
    latest = null;
    conflicted = false;
    $('editContent').value = '';
    $('editor').hidden = true;
    $('conflict').hidden = true;
    $('latestPanel').hidden = true;
    $('latestContent').textContent = '';
    $('editError').textContent = '';
  }

  function openEditor(item) {
    if (busy) return;
    if (editing && !window.confirm('放弃当前编辑草稿，改为编辑这篇日记？')) return;
    if (editing && !clearDraft('edit')) return;
    showEditor(item);
    saveDraft('edit');
    $('editContent').focus();
  }

  function showEditor(item) {
    closeEditor();
    editing = { id: item.id, create_date: item.create_date, version: item.version };
    $('editDate').textContent = `日记日期：${item.create_date}（不可编辑）`;
    $('editContent').value = item.content;
    count('editContent', 'editCount');
    $('editor').hidden = false;
    syncButtons();
  }

  function renderItems(items) {
    const fragment = document.createDocumentFragment();
    for (const item of items) {
      const article = document.createElement('article');
      article.className = 'entry';
      const heading = document.createElement('div');
      heading.className = 'entry-heading';
      const title = document.createElement('h3');
      title.textContent = item.create_date;
      const actions = document.createElement('div');
      actions.className = 'actions';
      const edit = document.createElement('button');
      edit.type = 'button';
      edit.textContent = '编辑';
      edit.setAttribute('aria-label', `编辑 ${item.create_date} 日记`);
      edit.dataset.write = '';
      edit.addEventListener('click', () => openEditor(item));
      const remove = document.createElement('button');
      remove.type = 'button';
      remove.className = 'danger';
      remove.textContent = '删除';
      remove.setAttribute('aria-label', `删除 ${item.create_date} 日记`);
      remove.dataset.write = '';
      remove.addEventListener('click', () => {
        if (busy || !window.confirm(`删除 ${item.create_date} 的日记？删除后不再显示，正文仍保留在数据库中，本期不提供恢复功能。${editing?.id === item.id ? '这篇日记的编辑草稿也会被清除。' : ''}`)) return;
        mutate('listError', async () => {
          await request(`/private/diaries/${encodeURIComponent(item.id)}`, { method: 'DELETE', version: item.version });
          if (editing?.id === item.id) { clearDraft('edit'); closeEditor(); }
          $('notice').textContent = '日记已删除，不再显示；正文仍保留在数据库中，本期不提供恢复功能。';
          loadList();
          $('archiveTitle').focus();
        });
      });
      actions.append(edit, remove);
      heading.append(title, actions);
      const body = document.createElement('p');
      body.className = 'entry-content';
      body.textContent = item.content.trim() ? item.content : '（历史日记：正文为空白）';
      article.append(heading, body);
      fragment.append(article);
    }
    $('entries').replaceChildren(fragment);
    syncButtons();
  }

  async function loadList() {
    if (!session) return;
    invalidateList();
    const sequence = listSequence;
    listController = new AbortController();
    listLoading = true;
    syncButtons();
    $('entries').hidden = true;
    $('entries').setAttribute('aria-busy', 'true');
    $('listError').textContent = '';
    $('retryList').hidden = true;
    $('listStatus').textContent = '正在加载日记…';
    $('previous').disabled = true;
    $('next').disabled = true;
    try {
      const query = new URLSearchParams({ ...filters, page, per_page: 24 });
      const data = await request(`/private/diaries?${query}`, { signal: listController.signal });
      if (sequence !== listSequence) return;
      const pages = Math.max(1, Math.ceil(data.total / 24));
      if (page > pages) { page = pages; return await loadList(); }
      renderItems(data.items);
      $('listStatus').textContent = data.total ? `共 ${data.total} 篇日记` : '没有符合条件的日记。';
      $('pageInfo').textContent = `第 ${page} / ${pages} 页`;
      $('previous').disabled = page <= 1;
      $('next').disabled = page >= pages;
    } catch (error) {
      if (sequence !== listSequence || error.name === 'AbortError') return;
      $('listStatus').textContent = '列表加载失败，未显示本次查询结果。';
      $('entries').replaceChildren();
      $('retryList').hidden = false;
      failure(error, 'listError');
    } finally {
      if (sequence === listSequence) {
        listLoading = false;
        $('entries').hidden = false;
        $('entries').setAttribute('aria-busy', 'false');
        syncButtons();
      }
    }
  }

  function signedIn(data) {
    if (typeof data?.username !== 'string' || typeof data?.csrf_token !== 'string' || !data.csrf_token) {
      throw new Error('会话响应无效，请刷新后重试。');
    }
    session = data;
    $('workspace').hidden = false;
    $('sessionTools').hidden = false;
    $('accountName').textContent = data.username;
    $('notice').textContent = '已登录。日记只有提交成功后才会保存到服务器。';
    restoreDrafts();
    syncButtons();
    loadList();
    if (session) $(editing ? 'editContent' : 'newContent').focus();
  }

  $('logout').addEventListener('click', async () => {
    if (busy || !session || !window.confirm('退出登录？服务器确认退出后，此浏览器保存的新建和编辑本地草稿都将清除。')) return;
    busy = true;
    invalidateList();
    versionController?.abort();
    $('entries').hidden = true;
    $('listStatus').textContent = '列表读取已取消，可重新加载。';
    $('retryList').hidden = false;
    $('notice').textContent = '正在退出，请等待服务器确认…';
    syncButtons();
    try {
      await request('/session', { method: 'DELETE' });
      session = null;
      $('newContent').value = '';
      closeEditor();
      $('workspace').hidden = true;
      if (!drafts.clearAll()) {
        window.alert('服务器已确认退出，但本地草稿清除失败，浏览器中可能仍有明文正文。请手动清理本站本地存储，尤其是在公共设备上。');
      }
      window.location.replace('/login?next=%2Fdiary');
    } catch (error) {
      if (error.status === 401) { redirectToLogin(); return; }
      $('notice').textContent = `未确认退出，登录态和草稿暂时保留。${error.message}`;
    } finally { busy = false; syncButtons(); }
  });

  $('createForm').addEventListener('submit', (event) => {
    event.preventDefault();
    mutate('createError', async () => {
      const value = content('newContent');
      $('newContent').readOnly = true;
      try {
        const item = await request('/private/diaries', { method: 'POST', body: { content: value } });
        clearDraft('new');
        $('newContent').value = '';
        count('newContent', 'newCount');
        $('notice').textContent = `已创建 ${item.create_date} 的日记。`;
        $('filterForm').reset();
        filters = { sort: 'desc' };
        page = 1;
        loadList();
      } finally { $('newContent').readOnly = false; }
    });
  });

  $('editForm').addEventListener('submit', (event) => {
    event.preventDefault();
    if (!editing || conflicted) return;
    mutate('editError', async () => {
      const value = content('editContent');
      $('editContent').readOnly = true;
      try {
        await request(`/private/diaries/${encodeURIComponent(editing.id)}`, { method: 'PATCH', body: { content: value }, version: editing.version });
        clearDraft('edit');
        closeEditor();
        $('notice').textContent = '修改已保存。';
        loadList();
        $('archiveTitle').focus();
      } catch (error) {
        if (error.status === 412 || error.status === 428) {
          conflicted = true;
          $('conflict').hidden = false;
        }
        throw error;
      } finally { $('editContent').readOnly = false; }
    });
  });

  $('refreshVersion').addEventListener('click', async () => {
    if (busy || listLoading || versionController || !editing || !session) return;
    const entry = editing;
    const activeSession = session;
    const controller = new AbortController();
    versionController = controller;
    syncButtons();
    $('editError').textContent = '';
    try {
      latest = null;
      $('latestPanel').hidden = true;
      const data = await request(`/private/diaries/${encodeURIComponent(entry.id)}`, { signal: controller.signal });
      if (controller.signal.aborted || editing !== entry || session !== activeSession) return;
      latest = data;
      $('latestContent').textContent = latest.content.trim() ? latest.content : '（历史日记：正文为空白）';
      $('latestPanel').hidden = false;
    } catch (error) {
      if (!controller.signal.aborted && editing === entry && session === activeSession) failure(error, 'editError');
    } finally { versionController = null; syncButtons(); }
  });
  $('acceptVersion').addEventListener('click', () => {
    if (busy || !latest || !editing) return;
    editing = latest;
    latest = null;
    conflicted = false;
    $('conflict').hidden = true;
    $('latestPanel').hidden = true;
    $('editError').textContent = '已采用读取到的版本号；请检查草稿后保存。';
    saveDraft('edit');
    syncButtons();
    $('editContent').focus();
  });
  $('cancelEdit').addEventListener('click', () => {
    if (!busy && window.confirm('放弃这次编辑草稿并清除对应本地草稿？') && clearDraft('edit')) { closeEditor(); $('archiveTitle').focus(); }
  });
  $('newContent').addEventListener('input', () => { count('newContent', 'newCount'); saveDraft('new'); });
  $('editContent').addEventListener('input', () => { count('editContent', 'editCount'); saveDraft('edit'); });
  $('startDate').addEventListener('change', () => {
    const start = $('startDate').value;
    if (!start) return;
    $('sort').value = 'asc';
    const [year, month, day] = start.split('-').map(Number);
    const end = new Date(0);
    end.setUTCHours(0, 0, 0, 0);
    end.setUTCFullYear(year, month - 1, day);
    end.setUTCDate(end.getUTCDate() + 31);
    if (end.getUTCFullYear() > 9999) {
      $('endDate').value = '';
      $('listError').textContent = '开始日期过晚，无法自动生成往后 31 天的结束日期。';
      return;
    }
    $('endDate').value = [
      String(end.getUTCFullYear()).padStart(4, '0'),
      String(end.getUTCMonth() + 1).padStart(2, '0'),
      String(end.getUTCDate()).padStart(2, '0'),
    ].join('-');
  });
  $('filterForm').addEventListener('submit', (event) => {
    event.preventDefault();
    if (busy) return;
    if ($('startDate').value && $('endDate').value && $('startDate').value > $('endDate').value) {
      $('listError').textContent = '开始日期不能晚于结束日期。';
      $('startDate').focus();
      return;
    }
    filters = { sort: $('sort').value };
    for (const [key, value] of new FormData($('filterForm'))) if (value) filters[key] = value;
    page = 1;
    loadList();
  });
  $('resetFilters').addEventListener('click', () => {
    if (busy) return;
    $('filterForm').reset(); filters = { sort: 'desc' }; page = 1; loadList();
  });
  $('previous').addEventListener('click', () => { if (!busy && page > 1) { page -= 1; loadList(); } });
  $('next').addEventListener('click', () => { if (!busy) { page += 1; loadList(); } });
  $('retryList').addEventListener('click', () => { if (!busy) loadList(); });
  window.addEventListener('beforeunload', (event) => {
    if (session && ($('newContent').value || editing)) { event.preventDefault(); event.returnValue = ''; }
  });

  function checkSession() {
    session = null;
    invalidateList();
    versionController?.abort();
    $('workspace').hidden = true;
    syncButtons();
    request('/session').then(signedIn).catch((error) => {
      if (error.status === 401) {
        redirectToLogin();
        return;
      }
      $('notice').textContent = error.message || '无法检查会话，请刷新后重试。';
    });
  }
  // 返回缓存页时重新鉴权并读取存储；导航生命周期不回写旧快照。
  window.addEventListener('pagehide', () => { $('workspace').hidden = true; });
  window.addEventListener('pageshow', (event) => {
    if (event.persisted) checkSession();
  });
  checkSession();
})();
