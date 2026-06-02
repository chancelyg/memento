/* ============================================================
   memento — poster wall front-end logic (vanilla JS, no deps)
   Talks to GET /api/favorites and per-item image_url.
   ============================================================ */
(function () {
  "use strict";

  /* ---------- Constants ---------- */
  var PER_PAGE = 24;
  var THEME_KEY = "memento-theme";

  var TYPE_META = {
    game:  { label: "游戏", emoji: "🎮", cls: "badge--game" },
    movie: { label: "电影", emoji: "🎬", cls: "badge--movie" },
    book:  { label: "图书", emoji: "📚", cls: "badge--book" }
  };

  /* ---------- State (treated immutably-ish: replace, don't mutate) ---------- */
  var state = {
    type: "all",
    q: "",
    page: 1,
    total: 0,
    loaded: 0,
    loading: false
  };

  /* ---------- DOM refs ---------- */
  var $grid       = document.getElementById("grid");
  var $count      = document.getElementById("countLine");
  var $empty      = document.getElementById("emptyState");
  var $loadMore   = document.getElementById("loadMore");
  var $search     = document.getElementById("searchInput");
  var $pills      = Array.prototype.slice.call(document.querySelectorAll(".pill"));
  var $toast      = document.getElementById("toast");
  var $themeBtn   = document.getElementById("themeToggle");

  /* Modal refs */
  var $modal      = document.getElementById("modal");
  var $modalImg   = document.getElementById("modalImg");
  var $modalBadge = document.getElementById("modalBadge");
  var $modalTitle = document.getElementById("modalTitle");
  var $modalAka   = document.getElementById("modalAka");
  var $modalSum   = document.getElementById("modalSummary");
  var $modalFields= document.getElementById("modalFields");
  var $modalLink  = document.getElementById("modalLink");

  /* ============================================================
     Utilities
     ============================================================ */

  /* Escape text for safe text-node insertion (we never use innerHTML w/ API data). */
  function setText(el, value) {
    el.textContent = (value === null || value === undefined) ? "" : String(value);
  }

  /* Coerce a value (string | array | null) into a clean display string. */
  function asList(value) {
    if (value === null || value === undefined) return [];
    if (Array.isArray(value)) {
      return value.filter(function (v) { return v !== null && v !== undefined && v !== ""; })
                  .map(function (v) { return String(v); });
    }
    var s = String(value).trim();
    return s ? [s] : [];
  }

  function joinList(value, sep) {
    return asList(value).join(sep || " / ");
  }

  /* aka may be a JSON-encoded string, an array, or a plain string. */
  function normalizeAka(aka) {
    if (Array.isArray(aka)) return asList(aka);
    if (typeof aka === "string") {
      var t = aka.trim();
      if (t.charAt(0) === "[") {
        try { return asList(JSON.parse(t)); } catch (e) { /* fall through */ }
      }
      return t ? [t] : [];
    }
    return [];
  }

  /* Friendly date: take leading YYYY-MM-DD / YYYY portion if present. */
  function fmtDate(value) {
    if (!value) return "";
    var s = String(value);
    var m = s.match(/\d{4}(-\d{2}(-\d{2})?)?/);
    return m ? m[0] : s;
  }

  function extraOf(item) {
    var ex = item && item.extra;
    return (ex && typeof ex === "object") ? ex : {};
  }

  function debounce(fn, wait) {
    var t;
    return function () {
      var ctx = this, args = arguments;
      clearTimeout(t);
      t = setTimeout(function () { fn.apply(ctx, args); }, wait);
    };
  }

  function showToast(msg) {
    setText($toast, msg);
    $toast.hidden = false;
    /* force reflow so transition runs */
    void $toast.offsetWidth;
    $toast.classList.add("is-visible");
    clearTimeout(showToast._t);
    showToast._t = setTimeout(function () {
      $toast.classList.remove("is-visible");
      setTimeout(function () { $toast.hidden = true; }, 280);
    }, 3600);
  }

  /* ============================================================
     Theme toggle
     ============================================================ */
  function applyTheme(theme) {
    document.documentElement.setAttribute("data-theme", theme);
    var icon = $themeBtn.querySelector(".theme-toggle__icon");
    if (icon) icon.textContent = theme === "light" ? "☀️" : "🌙";
  }

  function initTheme() {
    var saved;
    try { saved = localStorage.getItem(THEME_KEY); } catch (e) { saved = null; }
    applyTheme(saved === "light" ? "light" : "dark");
  }

  $themeBtn.addEventListener("click", function () {
    var next = document.documentElement.getAttribute("data-theme") === "light" ? "dark" : "light";
    applyTheme(next);
    try { localStorage.setItem(THEME_KEY, next); } catch (e) { /* ignore */ }
  });

  /* ============================================================
     Lazy image loading via IntersectionObserver
     ============================================================ */
  var imgObserver = null;
  if ("IntersectionObserver" in window) {
    imgObserver = new IntersectionObserver(function (entries, obs) {
      entries.forEach(function (entry) {
        if (entry.isIntersecting) {
          loadImage(entry.target);
          obs.unobserve(entry.target);
        }
      });
    }, { rootMargin: "200px 0px" });
  }

  function loadImage(imgEl) {
    var src = imgEl.getAttribute("data-src");
    if (!src) return;
    var skeleton = imgEl.parentNode.querySelector(".skeleton");
    imgEl.addEventListener("load", function () {
      imgEl.classList.add("is-loaded");
      if (skeleton) skeleton.remove();
    });
    imgEl.addEventListener("error", function () {
      /* graceful fallback: swap skeleton for a fallback box */
      if (skeleton) skeleton.remove();
      var poster = imgEl.parentNode;
      imgEl.remove();
      poster.appendChild(buildFallback());
    });
    imgEl.src = src;
  }

  function buildFallback() {
    var box = document.createElement("div");
    box.className = "card__fallback";
    var icon = document.createElement("div");
    icon.textContent = "🖼️";
    var label = document.createElement("span");
    label.className = "label";
    label.textContent = "暂无海报";
    box.appendChild(icon);
    box.appendChild(label);
    return box;
  }

  /* ============================================================
     Meta lines per type (for cards)
     ============================================================ */
  function metaLines(item) {
    var ex = extraOf(item);
    var lines = [];

    if (item.type === "game") {
      pushMeta(lines, "类型", joinList(item.genres));
      pushMeta(lines, "平台", joinList(ex.platforms));
      pushMeta(lines, "开发商", ex.developer);
      pushMeta(lines, "发布", fmtDate(item.release_date));
    } else if (item.type === "movie") {
      pushMeta(lines, "导演", ex.director);
      pushMeta(lines, "类型", joinList(item.genres));
      pushMeta(lines, "国家", ex.country);
      pushMeta(lines, "上映", fmtDate(item.release_date));
    } else if (item.type === "book") {
      pushMeta(lines, "作者", ex.author);
      pushMeta(lines, "出版社", ex.publisher);
      pushMeta(lines, "出版年份", fmtDate(item.release_date));
    } else {
      pushMeta(lines, "类型", joinList(item.genres));
    }
    return lines.slice(0, 3);
  }

  function pushMeta(lines, label, value) {
    if (value === null || value === undefined) return;
    var s = String(value).trim();
    if (!s) return;
    lines.push(label + "：" + s);
  }

  /* ============================================================
     Card rendering
     ============================================================ */
  function buildCard(item) {
    var meta = TYPE_META[item.type] || { label: item.type, emoji: "•", cls: "" };

    var card = document.createElement("button");
    card.type = "button";
    card.className = "card";
    card.setAttribute("aria-label", item.name + " 详情");

    /* Poster */
    var poster = document.createElement("div");
    poster.className = "card__poster";

    var badge = document.createElement("span");
    badge.className = "card__badge badge " + meta.cls;
    badge.textContent = meta.emoji + " " + meta.label;
    poster.appendChild(badge);

    if (item.image_url) {
      var skeleton = document.createElement("div");
      skeleton.className = "skeleton";
      poster.appendChild(skeleton);

      var img = document.createElement("img");
      img.className = "card__img";
      img.alt = item.name;
      img.loading = "lazy";
      img.setAttribute("data-src", item.image_url);
      poster.appendChild(img);

      if (imgObserver) imgObserver.observe(img);
      else loadImage(img);
    } else {
      poster.appendChild(buildFallback());
    }
    card.appendChild(poster);

    /* Body */
    var body = document.createElement("div");
    body.className = "card__body";

    var title = document.createElement("h3");
    title.className = "card__title";
    setText(title, item.name);
    body.appendChild(title);

    metaLines(item).forEach(function (line) {
      var p = document.createElement("p");
      p.className = "card__meta";
      setText(p, line);
      body.appendChild(p);
    });

    var date = fmtDate(item.sort_date);
    if (date) {
      var d = document.createElement("p");
      d.className = "card__date";
      setText(d, date);
      body.appendChild(d);
    }
    card.appendChild(body);

    card.addEventListener("click", function () { openModal(item); });
    return card;
  }

  /* ============================================================
     Detail modal
     ============================================================ */
  var lastFocused = null;
  var focusable = [];

  function fieldRows(item) {
    var ex = extraOf(item);
    var rows = [];
    var add = function (label, value) {
      var s;
      if (Array.isArray(value)) s = joinList(value);
      else s = (value === null || value === undefined) ? "" : String(value).trim();
      if (s) rows.push([label, s]);
    };

    if (item.type === "game") {
      add("类型", item.genres);
      add("平台", ex.platforms);
      add("开发商", ex.developer);
      add("发行商", ex.publisher);
      add("发布日期", fmtDate(item.release_date));
    } else if (item.type === "movie") {
      add("导演", ex.director);
      add("编剧", ex.writers);
      add("主演", ex.cast);
      add("类型", item.genres);
      add("国家/地区", ex.country);
      add("语言", ex.language);
      add("片长", ex.duration);
      add("上映日期", fmtDate(item.release_date));
      add("IMDb", ex.imdb);
    } else if (item.type === "book") {
      add("作者", ex.author);
      add("出版社", ex.publisher);
      add("出版年份", fmtDate(item.release_date));
      add("丛书", ex.series);
      add("页数", ex.pages);
      add("定价", ex.price);
      add("装帧", ex.binding);
      add("ISBN", ex.isbn);
    }
    if (item.rating !== null && item.rating !== undefined && item.rating !== "") {
      add("评分", item.rating);
    }
    add("收藏日期", fmtDate(item.sort_date));
    return rows;
  }

  function openModal(item) {
    var meta = TYPE_META[item.type] || { label: item.type, emoji: "•", cls: "" };
    lastFocused = document.activeElement;

    /* Poster */
    if (item.image_url) {
      $modalImg.alt = item.name;
      $modalImg.src = item.image_url;
      $modalImg.style.display = "";
    } else {
      $modalImg.removeAttribute("src");
      $modalImg.alt = "";
      $modalImg.style.display = "none";
    }

    /* Badge + title */
    $modalBadge.className = "badge " + meta.cls;
    $modalBadge.textContent = meta.emoji + " " + meta.label;
    setText($modalTitle, item.name);

    /* aka */
    var aka = normalizeAka(item.aka);
    setText($modalAka, aka.length ? "又名：" + aka.join(" / ") : "");

    /* summary */
    setText($modalSum, item.summary || "");

    /* fields */
    $modalFields.textContent = "";
    fieldRows(item).forEach(function (row) {
      var dt = document.createElement("dt");
      setText(dt, row[0]);
      var dd = document.createElement("dd");
      setText(dd, row[1]);
      $modalFields.appendChild(dt);
      $modalFields.appendChild(dd);
    });

    /* source link — only allow http(s) hrefs (defence-in-depth vs javascript: URIs) */
    if (item.url && /^https?:\/\//i.test(item.url)) {
      $modalLink.href = item.url;
      $modalLink.hidden = false;
    } else {
      $modalLink.removeAttribute("href");
      $modalLink.hidden = true;
    }

    $modal.hidden = false;
    document.body.style.overflow = "hidden";

    focusable = Array.prototype.slice.call(
      $modal.querySelectorAll('button, [href], input, [tabindex]:not([tabindex="-1"])')
    ).filter(function (el) { return !el.hidden && el.offsetParent !== null; });

    var first = $modal.querySelector(".modal__close");
    if (first) first.focus();
  }

  function closeModal() {
    $modal.hidden = true;
    document.body.style.overflow = "";
    $modalImg.removeAttribute("src");
    if (lastFocused && typeof lastFocused.focus === "function") lastFocused.focus();
  }

  /* Backdrop / close-button clicks */
  $modal.addEventListener("click", function (e) {
    if (e.target.hasAttribute("data-close")) closeModal();
  });

  /* Keyboard: Esc + focus trap */
  document.addEventListener("keydown", function (e) {
    if ($modal.hidden) return;
    if (e.key === "Escape") { e.preventDefault(); closeModal(); return; }
    if (e.key === "Tab" && focusable.length) {
      var first = focusable[0];
      var last = focusable[focusable.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault(); last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault(); first.focus();
      }
    }
  });

  /* ============================================================
     Data fetching + rendering
     ============================================================ */
  function buildUrl() {
    var params = new URLSearchParams();
    params.set("type", state.type);
    params.set("page", String(state.page));
    params.set("per_page", String(PER_PAGE));
    if (state.q) params.set("q", state.q);
    return "/api/favorites?" + params.toString();
  }

  function renderSkeletonGrid(n) {
    $grid.textContent = "";
    for (var i = 0; i < n; i++) {
      var card = document.createElement("div");
      card.className = "card";
      var poster = document.createElement("div");
      poster.className = "card__poster";
      var sk = document.createElement("div");
      sk.className = "skeleton";
      poster.appendChild(sk);
      card.appendChild(poster);
      var body = document.createElement("div");
      body.className = "card__body";
      body.style.minHeight = "70px";
      card.appendChild(body);
      $grid.appendChild(card);
    }
  }

  function updateCountLine() {
    if (state.total === 0) {
      setText($count, "");
      return;
    }
    setText($count, "已展示 " + state.loaded + " 项收藏，共 " + state.total + " 项");
  }

  function fetchPage(reset) {
    if (state.loading) return;
    state.loading = true;
    $grid.setAttribute("aria-busy", "true");
    $loadMore.disabled = true;

    if (reset) {
      state.page = 1;
      state.loaded = 0;
      $empty.hidden = true;
      renderSkeletonGrid(8);
    } else {
      $loadMore.textContent = "加载中…";
    }

    fetch(buildUrl(), { headers: { "Accept": "application/json" } })
      .then(function (res) {
        if (!res.ok) throw new Error("HTTP " + res.status);
        return res.json();
      })
      .then(function (json) {
        if (!json || json.success !== true || !json.data) {
          throw new Error((json && json.error) || "数据格式异常");
        }
        var data = json.data;
        var items = Array.isArray(data.items) ? data.items : [];
        state.total = typeof data.total === "number" ? data.total : items.length;

        if (reset) $grid.textContent = "";

        var frag = document.createDocumentFragment();
        items.forEach(function (item) {
          if (item && item.type && item.name) frag.appendChild(buildCard(item));
        });
        $grid.appendChild(frag);

        state.loaded += items.length;

        /* Empty state */
        if (state.total === 0) {
          $grid.textContent = "";
          $empty.hidden = false;
        } else {
          $empty.hidden = true;
        }

        /* Load more visibility */
        $loadMore.hidden = state.loaded >= state.total;
        $loadMore.textContent = "加载更多收藏…";

        updateCountLine();
      })
      .catch(function (err) {
        if (reset) $grid.textContent = "";
        showToast("加载失败，请稍后重试");
        if (window.console && console.error) console.error("[memento] fetch error:", err);
      })
      .then(function () {
        state.loading = false;
        $grid.setAttribute("aria-busy", "false");
        $loadMore.disabled = false;
      });
  }

  /* ============================================================
     Controls wiring
     ============================================================ */
  $pills.forEach(function (pill) {
    pill.addEventListener("click", function () {
      var type = pill.getAttribute("data-type");
      if (type === state.type) return;
      state.type = type;
      $pills.forEach(function (p) {
        var active = p === pill;
        p.classList.toggle("is-active", active);
        p.setAttribute("aria-selected", active ? "true" : "false");
      });
      fetchPage(true);
    });
  });

  var onSearch = debounce(function () {
    var q = $search.value.trim();
    if (q === state.q) return;
    state.q = q;
    fetchPage(true);
  }, 320);
  $search.addEventListener("input", onSearch);
  $search.addEventListener("search", function () {
    state.q = $search.value.trim();
    fetchPage(true);
  });

  $loadMore.addEventListener("click", function () {
    if (state.loading || state.loaded >= state.total) return;
    state.page += 1;
    fetchPage(false);
  });

  /* ============================================================
     Init
     ============================================================ */
  initTheme();
  fetchPage(true);
})();
