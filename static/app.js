/* ============================================================
   memento — Cinematic Dark Editorial poster wall (vanilla JS)
   Read-only public display. Talks to GET /api/favorites and
   per-item image_url. No external deps, no build step.
   All API text inserted via textContent (never innerHTML).
   ============================================================ */
(function () {
  "use strict";

  /* ---------- Constants ---------- */
  var PER_PAGE = 24;
  var THEME_KEY = "memento-theme";

  var TYPE_META = {
    game:  { label: "游戏", emoji: "🎮", cls: "badge--game",  accent: "rgba(74,110,220,0.30)" },
    movie: { label: "电影", emoji: "🎬", cls: "badge--movie", accent: "rgba(214,78,110,0.30)" },
    book:  { label: "图书", emoji: "📚", cls: "badge--book",  accent: "rgba(54,168,130,0.30)" }
  };

  /* ---------- State (replaced, not mutated) ---------- */
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
  var $modal       = document.getElementById("modal");
  var $modalAmbient= document.getElementById("modalAmbient");
  var $modalImg    = document.getElementById("modalImg");
  var $modalBadge  = document.getElementById("modalBadge");
  var $modalTitle  = document.getElementById("modalTitle");
  var $modalAka    = document.getElementById("modalAka");
  var $modalRating = document.getElementById("modalRating");
  var $modalSum    = document.getElementById("modalSummary");
  var $modalFields = document.getElementById("modalFields");
  var $modalLink   = document.getElementById("modalLink");

  /* ============================================================
     Utilities
     ============================================================ */

  /* Safe text-node insertion (never innerHTML with API data). */
  function setText(el, value) {
    el.textContent = (value === null || value === undefined) ? "" : String(value);
  }

  /* Coerce (string | array | null) into a clean list of strings. */
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

  /* Friendly date: leading YYYY-MM-DD / YYYY portion if present. */
  function fmtDate(value) {
    if (!value) return "";
    var s = String(value);
    var m = s.match(/\d{4}(-\d{2}(-\d{2})?)?/);
    return m ? m[0] : s;
  }

  /* Just the year, for compact captions. */
  function fmtYear(value) {
    if (!value) return "";
    var m = String(value).match(/\d{4}/);
    return m ? m[0] : "";
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
    void $toast.offsetWidth; /* reflow so transition runs */
    $toast.classList.add("is-visible");
    clearTimeout(showToast._t);
    showToast._t = setTimeout(function () {
      $toast.classList.remove("is-visible");
      setTimeout(function () { $toast.hidden = true; }, 300);
    }, 3600);
  }

  /* Fill ratio (0..1) for a 0..10 rating, drives a CSS star bar.
     Avoids any font-dependent half-star glyph (e.g. ⯨) entirely. */
  function ratingFill(rating) {
    var score = Number(rating);
    if (!isFinite(score)) return 0;
    return Math.max(0, Math.min(1, score / 10));
  }

  /* ============================================================
     Theme toggle (persisted in localStorage)
     ============================================================ */
  function applyTheme(theme) {
    document.documentElement.setAttribute("data-theme", theme);
    var icon = $themeBtn.querySelector(".theme-toggle__icon");
    if (icon) icon.textContent = theme === "light" ? "☀️" : "🌙";
    $themeBtn.setAttribute("aria-pressed", theme === "light" ? "true" : "false");
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
    }, { rootMargin: "300px 0px" });
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
      if (skeleton) skeleton.remove();
      var poster = imgEl.parentNode;
      imgEl.remove();
      poster.insertBefore(buildFallback(), poster.firstChild);
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
     Compact meta line for hover scrim (one line per card)
     ============================================================ */
  function scrimMeta(item) {
    var ex = extraOf(item);
    var bits = [];
    if (item.type === "game") {
      bits.push(ex.developer, joinList(item.genres));
    } else if (item.type === "movie") {
      bits.push(ex.director, joinList(item.genres));
    } else if (item.type === "book") {
      bits.push(ex.author, ex.publisher);
    } else {
      bits.push(joinList(item.genres));
    }
    var year = fmtYear(item.release_date);
    if (year) bits.push(year);
    return bits
      .map(function (b) { return (b === null || b === undefined) ? "" : String(b).trim(); })
      .filter(function (b) { return b; })
      .join(" · ");
  }

  /* ============================================================
     Card rendering — poster-first, metadata on hover/caption
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

    if (item.image_url) {
      var skeleton = document.createElement("div");
      skeleton.className = "skeleton";
      poster.appendChild(skeleton);

      var img = document.createElement("img");
      img.className = "card__img";
      img.alt = item.name;
      img.loading = "lazy";
      img.decoding = "async";
      img.setAttribute("data-src", item.image_url);
      poster.appendChild(img);

      if (imgObserver) imgObserver.observe(img);
      else loadImage(img);
    } else {
      poster.appendChild(buildFallback());
    }

    /* Type badge */
    var badge = document.createElement("span");
    badge.className = "card__badge badge " + meta.cls;
    badge.textContent = meta.emoji + " " + meta.label;
    poster.appendChild(badge);

    /* Hover scrim: title + one meta line revealed over the art */
    var scrim = document.createElement("div");
    scrim.className = "card__scrim";
    var sTitle = document.createElement("p");
    sTitle.className = "card__scrim-title";
    setText(sTitle, item.name);
    scrim.appendChild(sTitle);
    var metaLine = scrimMeta(item);
    if (metaLine) {
      var sMeta = document.createElement("p");
      sMeta.className = "card__scrim-meta";
      setText(sMeta, metaLine);
      scrim.appendChild(sMeta);
    }
    poster.appendChild(scrim);

    card.appendChild(poster);

    /* Persistent caption under poster (title + year) */
    var caption = document.createElement("div");
    caption.className = "card__caption";
    var title = document.createElement("h3");
    title.className = "card__title";
    setText(title, item.name);
    caption.appendChild(title);

    var year = fmtYear(item.release_date) || fmtYear(item.sort_date);
    if (year) {
      var y = document.createElement("p");
      y.className = "card__year";
      setText(y, year);
      caption.appendChild(y);
    }
    card.appendChild(caption);

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
    add("收藏日期", fmtDate(item.sort_date));
    return rows;
  }

  function openModal(item) {
    var meta = TYPE_META[item.type] || { label: item.type, emoji: "•", cls: "", accent: "" };
    lastFocused = document.activeElement;

    /* Ambient accent behind panel, tinted by media type */
    $modalAmbient.style.setProperty("--modal-accent", meta.accent || "");

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

    /* rating — CSS star bar (no font-dependent half-star glyph) */
    if (item.rating !== null && item.rating !== undefined && item.rating !== "" && isFinite(Number(item.rating))) {
      var num = Number(item.rating);
      var fillEl = $modalRating.querySelector(".modal__rating-fill");
      var numEl = $modalRating.querySelector(".modal__rating-num");
      if (fillEl) fillEl.style.width = (ratingFill(num) * 100).toFixed(2) + "%";
      if (numEl) setText(numEl, num.toFixed(1) + " / 10");
      $modalRating.setAttribute("role", "img");
      $modalRating.setAttribute("aria-label", "评分 " + num.toFixed(1) + " / 10");
      $modalRating.hidden = false;
    } else {
      $modalRating.hidden = true;
      $modalRating.removeAttribute("aria-label");
    }

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

    /* source link — only http(s) hrefs (defence vs javascript: URIs) */
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
      var caption = document.createElement("div");
      caption.className = "card__caption";
      card.appendChild(caption);
      $grid.appendChild(card);
    }
  }

  function updateCountLine() {
    if (state.total === 0) {
      setText($count, "");
      return;
    }
    setText($count, "已展示 " + state.loaded + " / " + state.total + " 项收藏");
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
      renderSkeletonGrid(12);
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

        if (state.total === 0) {
          $grid.textContent = "";
          $empty.hidden = false;
        } else {
          $empty.hidden = true;
        }

        $loadMore.hidden = state.loaded >= state.total;
        $loadMore.textContent = "加载更多收藏…";

        updateCountLine();
      })
      .catch(function (err) {
        if (reset) $grid.textContent = "";
        $loadMore.textContent = "加载更多收藏…";
        if (reset) $loadMore.hidden = true;
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
