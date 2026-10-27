(() => {
  'use strict';

  const newKey = 'memento-diary-new-v1';
  const editKey = 'memento-diary-edit-v1';

  window.DiaryDrafts = {
    newKey,
    editKey,
    clearAll() {
      let cleared = true;
      for (const key of [newKey, editKey]) {
        try { localStorage.removeItem(key); }
        catch { cleared = false; }
      }
      return cleared;
    },
  };
})();
