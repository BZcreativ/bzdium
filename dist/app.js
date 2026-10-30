/* bzdium UI chrome — vanilla JS, no build step.
 * Contract: docs/superpowers/specs/2026-10-08-bzdium-design.md section 6.
 * Renders purely from the initial get_state snapshot and "state-changed" events.
 */
(function () {
  "use strict";

  var hasTauri = !!(window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.event);
  var tauri = hasTauri ? window.__TAURI__ : null;

  /* ------------------------------------------------------------------ *
   * Plain-browser fallback (layout sanity check only, no backend).      *
   * ------------------------------------------------------------------ */
  // Browser-preview-only state for the Agent Access panel, so the admin
  // controls behave in a plain browser the way they do in the app.
  var mockAdminScope = false;
  var mockState = {
    services: [
      { id: "demo-1", name: "WhatsApp", url: "https://web.whatsapp.com", icon: "W", enabled: true, order: 0, hibernated: false, badgeCount: 3 },
      { id: "demo-2", name: "Telegram", url: "https://web.telegram.org", icon: "T", enabled: true, order: 1, hibernated: true, badgeCount: 0 },
      { id: "demo-3", name: "Discord", url: "https://discord.com/app", icon: "D", enabled: true, order: 2, hibernated: false, badgeCount: 12 },
      { id: "demo-4", name: "Grok", url: "https://grok.com/", icon: "GR", iconId: "grok", kind: "web", enabled: true, order: 3, hibernated: false, badgeCount: 0 },
      { id: "demo-5", name: "Grok API", url: "https://grok.com/", icon: "GA", iconId: "grok", kind: "grokApi", enabled: true, order: 4, hibernated: false, badgeCount: 0 }
    ],
    activeServiceId: "demo-1",
    settings: { hibernationMinutes: 30, minimizeToTray: true, startWithWindows: false, showUrlBar: true, darkUi: true, devMode: false, autoRecover: true, linkTarget: "browser", agentAccess: false, webview2RuntimeFolder: null, grokModel: null },
    totalBadgeCount: 15,
    dataDir: "demo"
  };
  // Browser preview mirrors the backend's per-app font defaults.
  mockState.services.forEach(function (s) { s.font = defaultFontConfig(); });

  var mockRecipes = [
    { name: "WhatsApp", url: "https://web.whatsapp.com", icon: "W" },
    { name: "Telegram", url: "https://web.telegram.org", icon: "T" },
    { name: "Discord", url: "https://discord.com/app", icon: "D" },
    { name: "Slack", url: "https://app.slack.com", icon: "S" },
    { name: "Messenger", url: "https://www.messenger.com", icon: "M" },
    { name: "Gmail", url: "https://mail.google.com", icon: "G" },
    { name: "Grok", url: "https://grok.com/", icon: "GR", iconId: "grok" },
    { name: "Grok API", url: "https://grok.com/", icon: "GA", iconId: "grok", kind: "grokApi" }
  ];

  // Same list the Rust `list_font_families` command returns.
  var mockFontFamilies = [
    "Arial", "Roboto", "Inter", "Open Sans", "Lato", "Noto Sans", "Segoe UI",
    "Helvetica Neue", "Verdana", "Tahoma", "Trebuchet MS", "Georgia",
    "Times New Roman", "Courier New"
  ];

  function mockService(id) {
    for (var i = 0; i < mockState.services.length; i++) {
      if (mockState.services[i].id === id) return mockState.services[i];
    }
    return null;
  }

  /* Mirror the Rust canonicalizer just enough for the preview: a trailing "+"
   * is the plus key (stored as "Plus" because "+" is also the separator). */
  function mockCanonicalAccel(value) {
    var text = String(value || "");
    if (text.slice(-1) === "+") return text.slice(0, -1) + "Plus";
    return text;
  }

  /* Merge a FontConfig the panel sends into the preview defaults; the real
   * backend does the validation, the preview only has to stay clickable. */
  function mockMergeFont(cfg) {
    var base = defaultFontConfig();
    if (!cfg) return base;
    base.family = cfg.family || null;
    if (typeof cfg.sizePx === "number") {
      base.sizePx = Math.max(base.minSizePx, Math.min(base.maxSizePx, cfg.sizePx));
    }
    base.enabled = cfg.enabled !== false;
    base.scalePxText = cfg.scalePxText !== false;
    ["increase", "decrease", "reset", "openSettings"].forEach(function (key) {
      if (cfg.shortcuts && cfg.shortcuts[key]) {
        base.shortcuts[key] = mockCanonicalAccel(cfg.shortcuts[key]);
      }
    });
    return base;
  }

  // Preview-only conversation so the assistant surface renders in a browser.
  var mockGrokConversations = [];

  function mockInvoke(name, args) {
    if (name === "get_recipes") return Promise.resolve(mockRecipes);
    if (name === "get_state") return Promise.resolve(mockState);
    // Grok assistant, so the chat surface is inspectable in a plain browser.
    if (name === "grok_key_status") return Promise.resolve({ configured: true, masked: "xai-••••demo", platformSupported: true });
    if (name === "grok_test_connection") return Promise.resolve({ ok: true, error: null, models: ["grok-4.7", "grok-4.6", "grok-4.5"], defaultModel: "grok-4.7" });
    if (name === "grok_list_conversations") return Promise.resolve(mockGrokConversations);
    if (name === "grok_save_conversation") {
      var conv = args.conversation;
      var at = mockGrokConversations.findIndex(function (c) { return c.id === conv.id; });
      if (at >= 0) mockGrokConversations[at] = conv; else mockGrokConversations.unshift(conv);
      return Promise.resolve(null);
    }
    if (name === "grok_clear_history") { mockGrokConversations = []; return Promise.resolve(null); }
    if (name === "grok_save_key") return Promise.resolve({ configured: true, masked: "xai-••••demo", platformSupported: true });
    if (name === "grok_delete_key") return Promise.resolve({ configured: false, masked: null, platformSupported: true });
    if (name === "grok_send_message") {
      // Simulate a streamed answer through the same handlers the real
      // backend events drive, so the preview exercises the UI path.
      var rid = args.requestId;
      setTimeout(function () {
        window.__grokTest.chunk({ requestId: rid, delta: "Preview answer: " });
        setTimeout(function () {
          window.__grokTest.chunk({ requestId: rid, delta: args.messages[args.messages.length - 1].content });
          window.__grokTest.done({ requestId: rid, model: args.model, usage: { promptTokens: 12, completionTokens: 34 } });
        }, 300);
      }, 200);
      return Promise.resolve(null);
    }
    if (name === "open_url_external") return Promise.resolve(null);
    if (name === "export_config") return Promise.resolve(null);
    if (name === "import_config") return Promise.resolve(null);
    // Agent Access, so the panel is inspectable in a plain browser too.
    if (name === "get_agent_status") return Promise.resolve({
      enabled: !!(mockState.settings && mockState.settings.agentAccess),
      running: false,
      port: null,
      allowAdminScope: !!mockAdminScope,
      tokenCount: 0,
      baseUrl: null
    });
    if (name === "mint_agent_token") return Promise.resolve("bzd_agt_demo_preview_only");
    if (name === "revoke_agent_tokens") return Promise.resolve(0);
    if (name === "set_agent_admin_scope") {
      // Remember the toggle so a later get_agent_status agrees with it; the
      // panel re-reads status after every change.
      mockAdminScope = !!args.allow;
      return Promise.resolve({
        enabled: true, running: false, port: null,
        allowAdminScope: mockAdminScope, tokenCount: 0, baseUrl: null
      });
    }
    // Mutate the demo state a little so interactions are visible in a browser.
    if (name === "set_active_service") mockState.activeServiceId = args.id;
    if (name === "hibernate_service" || name === "wake_service") {
      mockState.services.forEach(function (s) {
        if (s.id === args.id) s.hibernated = (name === "hibernate_service");
      });
    }
    if (name === "remove_service") {
      mockState.services = mockState.services.filter(function (s) { return s.id !== args.id; });
      if (mockState.activeServiceId === args.id) mockState.activeServiceId = null;
    }
    if (name === "update_settings") mockState.settings = args.settings;
    // Per-app fonts, so the Fonts panel works in a plain browser too.
    if (name === "list_font_families") return Promise.resolve(mockFontFamilies.slice());
    if (name === "get_font_config") {
      var fontSvc = mockService(args.id);
      return Promise.resolve(fontSvc ? fontSvc.font : defaultFontConfig());
    }
    if (name === "set_font_config") {
      var setSvc = mockService(args.id);
      if (setSvc && args.config) setSvc.font = mockMergeFont(args.config);
      setTimeout(function () { render(mockState); }, 0);
      return Promise.resolve(mockState);
    }
    if (name === "adjust_font_size") {
      var adjSvc = mockService(args.id);
      var adjCfg = adjSvc ? adjSvc.font : defaultFontConfig();
      if (args.action === "reset") adjCfg.sizePx = adjCfg.defaultSizePx;
      else if (args.action === "increase") adjCfg.sizePx = Math.min(adjCfg.maxSizePx, adjCfg.sizePx + adjCfg.stepPx);
      else if (args.action === "decrease") adjCfg.sizePx = Math.max(adjCfg.minSizePx, adjCfg.sizePx - adjCfg.stepPx);
      setTimeout(function () { render(mockState); }, 0);
      return Promise.resolve(adjCfg);
    }
    if (name === "reset_font_config") {
      var resetFontSvc = mockService(args.id);
      if (resetFontSvc) {
        var keptShortcuts = resetFontSvc.font.shortcuts;
        resetFontSvc.font = defaultFontConfig();
        resetFontSvc.font.shortcuts = keptShortcuts;
      }
      setTimeout(function () { render(mockState); }, 0);
      return Promise.resolve(mockState);
    }
    if (name === "reset_font_shortcuts") {
      var shortcutSvc = mockService(args.id);
      if (shortcutSvc) shortcutSvc.font.shortcuts = defaultFontConfig().shortcuts;
      setTimeout(function () { render(mockState); }, 0);
      return Promise.resolve(mockState);
    }
    if (name === "apply_font_to_all") {
      var sourceSvc = mockService(args.id);
      if (sourceSvc) {
        mockState.services.forEach(function (s) {
          if (s.id !== sourceSvc.id) s.font = JSON.parse(JSON.stringify(sourceSvc.font));
        });
      }
      setTimeout(function () { render(mockState); }, 0);
      return Promise.resolve(mockState);
    }
    mockState.totalBadgeCount = mockState.services.reduce(function (n, s) { return n + s.badgeCount; }, 0);
    setTimeout(function () { render(mockState); }, 0);
    return Promise.resolve(mockState);
  }

  function cmd(name, args) {
    var p = hasTauri ? tauri.core.invoke(name, args) : mockInvoke(name, args || {});
    return p.catch(function (err) {
      toast(typeof err === "string" ? err : String((err && err.message) || err));
      throw err;
    });
  }

  /* ------------------------------------------------------------------ *
   * Latest rendered state (refreshed on every event; never edited here).*
   * ------------------------------------------------------------------ */
  var currentState = null;
  var recipesCache = null;

  /* ---------------- DOM refs ---------------- */
  var $ = function (id) { return document.getElementById(id); };
  var serviceList = $("service-list");
  var logoBadge = $("logo-badge");
  var contextMenu = $("context-menu");
  var welcome = $("welcome");
  var toastContainer = $("toast-container");

  /* ---------------- Helpers ---------------- */

  function hashString(s) {
    var h = 0;
    for (var i = 0; i < s.length; i++) {
      h = (h * 31 + s.charCodeAt(i)) >>> 0;
    }
    return h;
  }

  function formatBadge(n) {
    return n > 99 ? "99+" : String(n);
  }

  /* The platform-primary modifier is Cmd on macOS, Ctrl elsewhere. Shortcuts
   * are stored with the portable "Ctrl" token, so matching maps it here. */
  var isMacLike = /Mac|iPhone|iPad/.test(navigator.platform);

  /* ------------------------------------------------------------------ *
   * Icon resolution — the single source of precedence:                 *
   *   1. user-uploaded custom icon (service.customIcon, file on disk)  *
   *   2. bundled brand icon (service.iconId -> assets/icons/<slug>.svg)*
   *   3. monogram fallback (1-3 letter lettermark)                     *
   * ------------------------------------------------------------------ */

  var brandManifest = null;          // slug -> brand hex (assets/icons/manifest.json)
  var customIconCache = {};          // service id -> { file, url }

  function loadBrandManifest() {
    if (brandManifest) return Promise.resolve(brandManifest);
    return fetch("assets/icons/manifest.json")
      .then(function (r) { return r.json(); })
      .then(function (m) { brandManifest = m || {}; return m; })
      .catch(function () { brandManifest = {}; return {}; });
  }

  function resolveServiceIcon(service) {
    if (service.customIcon) return { type: "custom" };
    if (service.iconId && brandManifest && brandManifest[service.iconId]) {
      return { type: "brand", slug: service.iconId, color: brandManifest[service.iconId] };
    }
    return { type: "monogram", letters: serviceLetter(service) };
  }

  /* Fetches the custom icon data URL (cached per icon file name, so
   * re-renders are free and a replaced icon refetches exactly once). */
  function ensureCustomIcon(service) {
    if (!service.customIcon) return Promise.resolve(null);
    var cached = customIconCache[service.id];
    if (cached && cached.file === service.customIcon) return Promise.resolve(cached.url);
    return cmd("get_service_icon", { id: service.id })
      .then(function (url) {
        if (!url) return null; // missing/corrupt file -> graceful fallback
        customIconCache[service.id] = { file: service.customIcon, url: url };
        return url;
      })
      .catch(function () { return null; });
  }

  /* Builds the icon element used everywhere (sidebar, edit preview). */
  function buildIconEl(service) {
    var resolved = resolveServiceIcon(service);
    if (resolved.type === "custom") {
      var wrap = document.createElement("span");
      wrap.className = "svc-icon svc-icon-custom";
      var img = document.createElement("img");
      img.alt = "";
      img.draggable = false;
      wrap.appendChild(img);
      ensureCustomIcon(service).then(function (url) {
        if (url) img.src = url;
        else fallbackToMonogram(wrap, service);
      });
      return wrap;
    }
    if (resolved.type === "brand") {
      var chip = document.createElement("span");
      chip.className = "svc-icon svc-icon-brand";
      chip.style.setProperty("--brand", resolved.color);
      var glyph = document.createElement("img");
      glyph.src = "assets/icons/" + resolved.slug + ".svg";
      glyph.alt = "";
      glyph.draggable = false;
      glyph.onerror = function () { fallbackToMonogram(chip, service); };
      chip.appendChild(glyph);
      return chip;
    }
    return buildMonogramEl(service, resolved.letters);
  }

  function buildMonogramEl(service, letters) {
    var el = document.createElement("span");
    el.className = "svc-icon svc-icon-mono";
    var mark = letters || serviceLetter(service);
    el.textContent = mark;
    el.setAttribute("data-len", String(Math.min(3, Math.max(1, mark.length))));
    return el;
  }

  function fallbackToMonogram(el, service) {
    var mark = serviceLetter(service);
    el.className = "svc-icon svc-icon-mono";
    el.setAttribute("data-len", String(Math.min(3, Math.max(1, mark.length))));
    el.style.removeProperty("--brand");
    el.textContent = mark;
  }

  function serviceLetter(service) {
    var icon = (service.icon || "").trim();
    if (icon) return icon.toUpperCase();
    return initials(service.name || "?");
  }

  /* Two-letter lettermark: first letters of the first two words, else the
   * first two letters of the single word. Mirrors services.rs `initials`. */
  function initials(name) {
    var words = String(name).trim().split(/\s+/).filter(Boolean);
    if (words.length >= 2) {
      return (words[0][0] + words[1][0]).toUpperCase();
    }
    var letters = String(name).replace(/[^\p{L}\p{N}]/gu, "").slice(0, 2);
    return letters.toUpperCase();
  }

  function applyTheme(darkUi) {
    document.body.classList.toggle("light", !darkUi);
  }

  function toast(message) {
    var el = document.createElement("div");
    el.className = "toast";
    el.textContent = message;
    toastContainer.appendChild(el);
    setTimeout(function () { el.classList.add("fading"); }, 3600);
    setTimeout(function () { el.remove(); }, 4000);
  }

  function validateServiceForm(name, url) {
    if (!name) return "Name is required.";
    var u;
    try { u = new URL(url); } catch (e) { return "Enter a valid URL."; }
    if (u.protocol !== "https:") return "URL must start with https://";
    return null;
  }

  /* ---------------- Render (single source of truth) ---------------- */

  function render(state) {
    if (!state) return;
    currentState = state;
    applyTheme(!!(state.settings && state.settings.darkUi));

    // Total unread on the logo.
    var total = state.totalBadgeCount || 0;
    logoBadge.textContent = formatBadge(total);
    logoBadge.classList.toggle("hidden", total <= 0);

    // Service list (state.services is already sorted by order; disabled
    // services are hidden — they have no webview and cannot be activated).
    serviceList.textContent = "";
    (state.services || []).forEach(function (service) {
      if (service.enabled === false) return;
      serviceList.appendChild(buildServiceItem(service, state.activeServiceId));
    });

    // Empty state.
    welcome.classList.toggle("hidden", (state.services || []).length !== 0);

    // The native Grok assistant replaces the whole content area with the
    // chat surface rendered by this webview (no service webview backs it).
    var activeService = state.activeServiceId ? serviceById(state.activeServiceId) : null;
    var grokActive = !!(activeService && activeService.kind === "grokApi");
    document.body.classList.toggle("grok-active", grokActive);
    // .hidden carries display:none !important, so visibility is driven by
    // the class (not just the body.grok-active CSS rule).
    $("grok-chat").classList.toggle("hidden", !grokActive);
    if (grokActive) {
      refreshGrokChat(activeService);
      // Focus the composer only on the transition into the assistant —
      // background state events must never steal focus.
      if (!grokWasActive && document.hasFocus()) $("grok-input").focus();
    }
    grokWasActive = grokActive;

    refreshUrlBar();
    // Keep the Fonts panel in step with the snapshot; refreshFontPanel skips
    // any control the user is currently typing in.
    if (isSettingsOpen()) refreshFontPanel(false);
  }
  var grokWasActive = false;

  function buildServiceItem(service, activeServiceId) {
    var btn = document.createElement("button");
    btn.className = "svc-item";
    btn.dataset.id = service.id;
    btn.title = service.name;
    // TLS verification is disabled for this service — flag it so the
    // weakened security posture is visible at a glance (hover explains).
    if (service.ignoreTlsErrors) {
      btn.classList.add("tls-bypass");
      btn.title += " — TLS verification is OFF for this service";
    }
    btn.draggable = true;
    btn.style.setProperty("--hue", String(hashString(service.name || "") % 360));
    if (service.id === activeServiceId) btn.classList.add("active");
    if (service.hibernated) btn.classList.add("hibernated");

    var icon = buildIconEl(service);
    btn.appendChild(icon);

    if (service.badgeCount > 0) {
      var badge = document.createElement("span");
      badge.className = "badge";
      badge.textContent = formatBadge(service.badgeCount);
      btn.appendChild(badge);
    }

    if (service.hibernated) {
      var zz = document.createElement("span");
      zz.className = "zz";
      zz.textContent = "zZ";
      btn.appendChild(zz);
    }

    if (service.ignoreTlsErrors) {
      var tls = document.createElement("span");
      tls.className = "tls-warn";
      tls.textContent = "⚠";
      btn.appendChild(tls);
    }

    btn.addEventListener("click", function () {
      cmd("set_active_service", { id: service.id }).catch(function () {});
    });
    btn.addEventListener("contextmenu", function (e) {
      e.preventDefault();
      openContextMenu(service, e.clientX, e.clientY);
    });
    attachDnD(btn, service);
    return btn;
  }

  /* ---------------- Context menu ---------------- */

  function closeContextMenu() {
    contextMenu.classList.add("hidden");
    contextMenu.textContent = "";
    refreshOverlay();
  }

  function addCtxItem(label, danger, onClick, disabled) {
    var item = document.createElement("button");
    item.className = "ctx-item" + (danger ? " danger" : "") + (disabled ? " disabled" : "");
    item.textContent = label;
    item.setAttribute("role", "menuitem");
    if (disabled) {
      item.disabled = true;
      item.setAttribute("aria-disabled", "true");
    } else {
      item.addEventListener("click", function () {
        closeContextMenu();
        onClick();
      });
    }
    contextMenu.appendChild(item);
  }

  function addCtxSep() {
    var sep = document.createElement("div");
    sep.className = "ctx-sep";
    contextMenu.appendChild(sep);
  }

  function openContextMenu(service, x, y) {
    contextMenu.textContent = "";

    if (service.kind === "grokApi") {
      // The native assistant has no page: navigation/hibernation entries
      // would be dead controls, so it gets its own menu instead.
      addCtxItem("New chat", false, function () {
        grokStartNewChat();
        cmd("set_active_service", { id: service.id }).catch(function () {});
      });
      addCtxItem("Clear conversation history…", false, function () {
        openConfirm(
          "Delete ALL Grok conversation history stored in bzdium? This cannot be undone.",
          function () { grokClearHistory(); }
        );
      });
      addCtxItem("Open grok.com in browser", false, function () {
        cmd("open_service_external", { id: service.id }).catch(function () {});
      });
      addCtxSep();
      addCtxItem("Edit", false, function () { openEditModal(service); });
      if (currentState && currentState.settings && currentState.settings.devMode) {
        addCtxItem("Inspect UI (DevTools)", false, function () {
          cmd("open_devtools", { id: null }).catch(function () {});
        });
      }
      addCtxSep();
      addCtxItem("Remove", true, function () {
        openConfirm(
          "Remove “" + service.name + "”? Its locally stored conversation history is deleted too.",
          function () { cmd("remove_service", { id: service.id }).catch(function () {}); }
        );
      });
    } else {
      // Per-app font controls first: they are the most-used entries for a
      // long-lived chat window, and the size is only clamped backend-side.
      var fontCfg = service.font || defaultFontConfig();
      var fontMin = numberOr(fontCfg.minSizePx, 8);
      var fontMax = numberOr(fontCfg.maxSizePx, 40);
      var fontSize = numberOr(fontCfg.sizePx, 16);
      addCtxItem("Increase font size", false, function () {
        adjustServiceFont(service, "increase");
      }, fontSize >= fontMax);
      addCtxItem("Decrease font size", false, function () {
        adjustServiceFont(service, "decrease");
      }, fontSize <= fontMin);
      addCtxItem("Reset font size", false, function () {
        adjustServiceFont(service, "reset");
      });
      addCtxItem("Font settings…", false, function () {
        openFontSettings(service.id);
      });
      addCtxSep();
      addCtxItem("Reload", false, function () {
        cmd("reload_service", { id: service.id }).catch(function () {});
      });
      addCtxItem("Back", false, function () {
        cmd("navigate", { id: service.id, action: "back" }).catch(function () {});
      });
      addCtxItem("Forward", false, function () {
        cmd("navigate", { id: service.id, action: "forward" }).catch(function () {});
      });
      addCtxItem("Home", false, function () {
        cmd("navigate", { id: service.id, action: "home" }).catch(function () {});
      });
      addCtxItem("Open in browser", false, function () {
        cmd("open_service_external", { id: service.id }).catch(function () {});
      });
      // Recovery action: a broken session can survive every reload because the
      // bad state is in the WebView2 profile, not in the page. Confirm first —
      // this signs the user out of the service.
      addCtxItem("Clear site data…", true, function () {
        openConfirm(
          "Clear “" + service.name + "”'s cookies, local storage and cache? You will be signed out of this service and have to sign in again.",
          function () { cmd("clear_service_data", { id: service.id }).catch(function () {}); },
          "Clear data"
        );
      });
      addCtxSep();
      if (service.hibernated) {
        addCtxItem("Wake", false, function () {
          cmd("wake_service", { id: service.id }).catch(function () {});
        });
      } else {
        addCtxItem("Hibernate", false, function () {
          cmd("hibernate_service", { id: service.id }).catch(function () {});
        });
      }
      addCtxItem("Edit", false, function () { openEditModal(service); });
      if (currentState && currentState.settings && currentState.settings.devMode) {
        addCtxItem("Inspect page (DevTools)", false, function () {
          cmd("open_devtools", { id: service.id }).catch(function () {});
        });
      }
      addCtxSep();
      addCtxItem("Remove", true, function () {
        openConfirm(
          "Remove “" + service.name + "”? Its saved session (login data) is deleted too.",
          function () { cmd("remove_service", { id: service.id }).catch(function () {}); }
        );
      });
    }

    contextMenu.classList.remove("hidden");
    // Clamp inside the window.
    var rect = contextMenu.getBoundingClientRect();
    contextMenu.style.left = Math.min(x, window.innerWidth - rect.width - 4) + "px";
    contextMenu.style.top = Math.min(y, window.innerHeight - rect.height - 4) + "px";
    refreshOverlay();
  }

  /* ---------------- Drag & drop reorder ---------------- */

  var draggedId = null;

  function attachDnD(btn, service) {
    btn.addEventListener("dragstart", function (e) {
      draggedId = service.id;
      e.dataTransfer.effectAllowed = "move";
      e.dataTransfer.setData("text/plain", service.id);
    });
    btn.addEventListener("dragover", function (e) {
      if (!draggedId || draggedId === service.id) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = "move";
      var rect = btn.getBoundingClientRect();
      var before = e.clientY < rect.top + rect.height / 2;
      btn.classList.toggle("drag-over-top", before);
      btn.classList.toggle("drag-over-bottom", !before);
    });
    btn.addEventListener("dragleave", function () {
      btn.classList.remove("drag-over-top", "drag-over-bottom");
    });
    btn.addEventListener("drop", function (e) {
      e.preventDefault();
      btn.classList.remove("drag-over-top", "drag-over-bottom");
      if (!draggedId || draggedId === service.id || !currentState) return;
      var rect = btn.getBoundingClientRect();
      var before = e.clientY < rect.top + rect.height / 2;
      var orderedIds = currentState.services.map(function (s) { return s.id; });
      var from = orderedIds.indexOf(draggedId);
      var to = orderedIds.indexOf(service.id);
      // A state-changed event may have rebuilt the list mid-drag; if either
      // id vanished, drop the gesture instead of mangling the order.
      if (from === -1 || to === -1) { draggedId = null; return; }
      orderedIds.splice(from, 1);
      orderedIds.splice(to + (before ? 0 : 1), 0, draggedId);
      draggedId = null;
      cmd("reorder_services", { orderedIds: orderedIds }).catch(function () {});
    });
    btn.addEventListener("dragend", function () {
      draggedId = null;
      serviceList.querySelectorAll(".drag-over-top, .drag-over-bottom").forEach(function (el) {
        el.classList.remove("drag-over-top", "drag-over-bottom");
      });
    });
  }

  /* ---------------- Modals ---------------- */

  var modals = ["add-modal", "edit-modal", "settings-modal", "confirm-modal"];

  function openModal(id) { $(id).classList.remove("hidden"); refreshOverlay(); }
  function closeModal(id) { $(id).classList.add("hidden"); refreshOverlay(); }
  function closeAllModals() { modals.forEach(closeModal); }
  function anyModalOpen() {
    return modals.some(function (id) { return !$(id).classList.contains("hidden"); });
  }

  /* While any overlay (modal or context menu) is open, service webviews must
   * be hidden: they sit above the UI webview in z-order and would occlude
   * the dialog. Backend command `set_overlay_mode` does the hiding. */
  var overlayActive = false;
  function refreshOverlay() {
    var any = anyModalOpen() || !contextMenu.classList.contains("hidden");
    if (any === overlayActive) return;
    overlayActive = any;
    if (hasTauri) {
      tauri.core.invoke("set_overlay_mode", { open: any }).catch(function () {});
    }
  }

  modals.forEach(function (id) {
    $(id).addEventListener("mousedown", function (e) {
      if (e.target === this) closeModal(id);
    });
  });
  document.querySelectorAll("[data-close]").forEach(function (btn) {
    btn.addEventListener("click", function () {
      var overlay = btn.closest(".modal-overlay");
      if (overlay) closeModal(overlay.id);
    });
  });

  /* ----- Confirm ----- */
  var confirmHandler = null;
  // `okLabel` lets each caller name its action; the default matches the
  // destructive origin of this dialog ("Remove").
  function openConfirm(message, onConfirm, okLabel) {
    $("confirm-text").textContent = message;
    $("confirm-ok").textContent = okLabel || "Remove";
    confirmHandler = onConfirm;
    openModal("confirm-modal");
  }
  $("confirm-ok").addEventListener("click", function () {
    closeModal("confirm-modal");
    if (confirmHandler) confirmHandler();
    confirmHandler = null;
  });
  $("confirm-cancel").addEventListener("click", function () {
    closeModal("confirm-modal");
    confirmHandler = null;
  });

  /* ----- Add service ----- */
  var addIconTouched = false;
  var addSelectedKind = "web";

  /* The TLS checkbox and its warning only apply to embedded pages; the
   * native assistant has neither, so both hide when its recipe is picked. */
  function setTlsRowVisible(formPrefix, visible) {
    var checkboxLabel = $(formPrefix + "-ignore-tls").closest("label");
    checkboxLabel.classList.toggle("edit-grok-hidden", !visible);
    var disclaimer = checkboxLabel.nextElementSibling;
    if (disclaimer && disclaimer.classList.contains("field-disclaimer")) {
      disclaimer.classList.toggle("edit-grok-hidden", !visible);
    }
  }

  function defaultIconFor(name) {
    var n = (name || "").trim();
    return n ? initials(n) : "";
  }

  function loadRecipes() {
    if (recipesCache) { renderRecipes(recipesCache); return; }
    cmd("get_recipes").then(function (recipes) {
      recipesCache = recipes || [];
      renderRecipes(recipesCache);
    }).catch(function () {});
  }

  function renderRecipes(recipes) {
    var grid = $("recipe-grid");
    grid.textContent = "";
    recipes.forEach(function (recipe) {
      var cell = document.createElement("button");
      cell.type = "button";
      cell.className = "recipe-cell";
      cell.title = recipe.url;
      cell.style.setProperty("--hue", String(hashString(recipe.name) % 360));

      var icon = document.createElement("span");
      if (recipe.iconId && brandManifest && brandManifest[recipe.iconId]) {
        icon.className = "svc-icon svc-icon-brand";
        icon.style.setProperty("--brand", brandManifest[recipe.iconId]);
        var glyph = document.createElement("img");
        glyph.src = "assets/icons/" + recipe.iconId + ".svg";
        glyph.alt = "";
        glyph.draggable = false;
        icon.appendChild(glyph);
      } else {
        icon.className = "svc-icon svc-icon-mono";
        var mark = (recipe.icon || defaultIconFor(recipe.name) || "?").toUpperCase();
        icon.textContent = mark;
        icon.setAttribute("data-len", String(Math.min(3, Math.max(1, mark.length))));
      }
      cell.appendChild(icon);

      var name = document.createElement("span");
      name.className = "recipe-name";
      name.textContent = recipe.name;
      cell.appendChild(name);

      cell.addEventListener("click", function () {
        grid.querySelectorAll(".recipe-cell").forEach(function (c) { c.classList.remove("selected"); });
        cell.classList.add("selected");
        addSelectedKind = recipe.kind || "web";
        $("add-name").value = recipe.name;
        $("add-url").value = recipe.url;
        $("add-icon").value = (recipe.icon || defaultIconFor(recipe.name)).toUpperCase();
        $("add-icon-id").value = recipe.iconId || "";
        // The native assistant connects through the xAI API: its URL is
        // pinned backend-side to the official site, so the field is inert.
        var isApi = addSelectedKind === "grokApi";
        $("add-url").disabled = isApi;
        setTlsRowVisible("add", !isApi);
        addIconTouched = false;
      });
      grid.appendChild(cell);
    });
  }

  function openAddModal() {
    $("add-form").reset();
    $("add-ignore-tls").checked = false;
    $("add-error").classList.add("hidden");
    $("add-url").disabled = false;
    setTlsRowVisible("add", true);
    addIconTouched = false;
    addSelectedKind = "web";
    renderRecipes(recipesCache || []);
    openModal("add-modal");
    loadRecipes();
    $("add-name").focus();
  }

  $("add-btn").addEventListener("click", openAddModal);
  $("welcome-add").addEventListener("click", openAddModal);

  $("add-name").addEventListener("input", function () {
    if (!addIconTouched) $("add-icon").value = defaultIconFor(this.value);
  });
  // A hand-edited URL re-identifies the service: drop the selected recipe's
  // brand icon so the backend re-resolves from the new host.
  $("add-url").addEventListener("input", function () {
    $("add-icon-id").value = "";
  });
  $("add-icon").addEventListener("input", function () {
    addIconTouched = this.value.trim().length > 0;
  });

  $("add-form").addEventListener("submit", function (e) {
    e.preventDefault();
    var name = $("add-name").value.trim();
    var url = $("add-url").value.trim();
    var icon = $("add-icon").value.trim() || defaultIconFor(name);
    var iconId = $("add-icon-id").value.trim() || null;
    var ignoreTlsErrors = $("add-ignore-tls").checked;
    var isApi = addSelectedKind === "grokApi";
    var err = validateServiceForm(name, url);
    if (err) {
      $("add-error").textContent = err;
      $("add-error").classList.remove("hidden");
      return;
    }
    cmd("add_service", {
      name: name,
      url: url,
      icon: icon,
      iconId: iconId,
      ignoreTlsErrors: ignoreTlsErrors,
      kind: isApi ? "grokApi" : null
    })
      .then(function () { closeModal("add-modal"); })
      .catch(function () {});
  });

  /* ----- Edit service ----- */
  var editingId = null;

  function serviceFromState(id) {
    if (!currentState) return null;
    for (var i = 0; i < (currentState.services || []).length; i++) {
      if (currentState.services[i].id === id) return currentState.services[i];
    }
    return null;
  }

  function refreshEditIconPreview() {
    var service = editingId && serviceFromState(editingId);
    var preview = $("edit-icon-preview");
    preview.textContent = "";
    if (!service) return;
    preview.appendChild(buildIconEl(service));
    $("edit-icon-reset").disabled = !service.customIcon;
  }

  $("edit-icon-upload").addEventListener("click", function () {
    if (!editingId) return;
    cmd("pick_service_icon", { id: editingId })
      .then(function () { refreshEditIconPreview(); })
      .catch(function () {});
  });

  $("edit-icon-reset").addEventListener("click", function () {
    if (!editingId) return;
    cmd("reset_service_icon", { id: editingId })
      .then(function () { refreshEditIconPreview(); })
      .catch(function () {});
  });

  function openEditModal(service) {
    editingId = service.id;
    var isApi = service.kind === "grokApi";
    $("edit-name").value = service.name;
    $("edit-url").value = service.url;
    $("edit-icon").value = service.icon || defaultIconFor(service.name);
    $("edit-enabled").checked = service.enabled !== false;
    $("edit-ignore-tls").checked = !!service.ignoreTlsErrors;
    // Web-only controls: the assistant has no page and no TLS choice.
    $("edit-url").closest("label").classList.toggle("edit-grok-hidden", isApi);
    setTlsRowVisible("edit", !isApi);
    $("edit-url").disabled = isApi;
    $("edit-error").classList.add("hidden");
    refreshEditIconPreview();
    openModal("edit-modal");
    $("edit-name").focus();
  }

  $("edit-form").addEventListener("submit", function (e) {
    e.preventDefault();
    if (!editingId) return;
    var service = serviceFromState(editingId);
    var name = $("edit-name").value.trim();
    var url = $("edit-url").value.trim() || (service ? service.url : "");
    var icon = $("edit-icon").value.trim() || defaultIconFor(name);
    var ignoreTlsErrors = $("edit-ignore-tls").checked;
    var err = validateServiceForm(name, url);
    if (err) {
      $("edit-error").textContent = err;
      $("edit-error").classList.remove("hidden");
      return;
    }
    cmd("update_service", {
      id: editingId,
      name: name,
      url: url,
      icon: icon,
      enabled: $("edit-enabled").checked,
      ignoreTlsErrors: ignoreTlsErrors
    }).then(function () { closeModal("edit-modal"); editingId = null; })
      .catch(function () {});
  });

  /* ----- Settings ----- */

  function openSettingsModal(preferredFontId) {
    var s = (currentState && currentState.settings) || {};
    $("set-hibernation").value = (typeof s.hibernationMinutes === "number") ? s.hibernationMinutes : 30;
    $("set-tray").checked = s.minimizeToTray !== false;
    $("set-autostart").checked = !!s.startWithWindows;
    $("set-urlbar").checked = s.showUrlBar !== false;
    $("set-dark").checked = s.darkUi !== false;
    $("set-devmode").checked = !!s.devMode;
    $("set-autorecover").checked = s.autoRecover !== false;
    $("set-link-bzdium").checked = s.linkTarget === "bzdium";
    $("set-agent-access").checked = !!s.agentAccess;
    loadWebview2RuntimeOptions(s.webview2RuntimeFolder || null);
    $("settings-error").classList.add("hidden");
    $("agent-error").classList.add("hidden");
    $("agent-token-note").classList.add("hidden");
    // The token value is never put into app state; clear whatever the last
    // session showed so a stale key cannot linger in the DOM.
    agentTokenShown = null;
    renderAgentToken();
    refreshAgentPanel();
    refreshGrokPanel();
    refreshSettingsFooter();
    // The Fonts panel preselects the active service unless the caller asked
    // for one explicitly (sidebar menu / in-page context menu).
    fontPanelId = (typeof preferredFontId === "string" && serviceById(preferredFontId))
      ? preferredFontId
      : ((currentState && currentState.activeServiceId) || firstServiceId());
    fontPanelStatusId = null;
    ensureFontFamilies();
    refreshFontPanel(true);
    openModal("settings-modal");
    if (typeof preferredFontId === "string") revealFontSection();
  }

  /* ----- WebView2 runtime pin (Windows only) -----
   * The list command returns [] on Linux, so the row is hidden whenever
   * nothing is available. The stored pin may point at a runtime the
   * Evergreen updater has since removed — keep it visible (marked) rather
   * than silently dropping the user's choice. */
  function escapeHtml(s) {
    return String(s).replace(/[&<>"']/g, function (c) {
      return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c];
    });
  }

  function loadWebview2RuntimeOptions(selected) {
    var row = $("wv2rt-row");
    var sel = $("set-wv2rt");
    cmd("list_webview2_runtimes", {}).then(function (runtimes) {
      if (!runtimes || !runtimes.length) {
        row.classList.add("hidden");
        sel.dataset.value = selected || "";
        return;
      }
      var options = ['<option value="">System default (Evergreen)</option>'];
      runtimes.forEach(function (r) {
        var isSel = selected && r.path === selected;
        options.push('<option value="' + escapeHtml(r.path) + '"' + (isSel ? " selected" : "") + ">" +
                     escapeHtml(r.version) + "</option>");
      });
      if (selected && !runtimes.some(function (r) { return r.path === selected; })) {
        options.push('<option value="' + escapeHtml(selected) + '" selected>' +
                     escapeHtml(selected) + " (missing)</option>");
      }
      sel.innerHTML = options.join("");
      row.classList.remove("hidden");
    }).catch(function () {
      row.classList.add("hidden");
    });
  }

  /* ----- Agent Access panel -----
   * The token is deliberately NOT part of currentState: it must never be
   * mirrored into the snapshot the whole UI sees. We hold it in this one
   * variable, show it once, and forget it on the next modal open. */

  var agentTokenShown = null;
  var agentStatus = null;

  function renderAgentToken() {
    var el = $("agent-token");
    if (agentTokenShown) {
      el.textContent = agentTokenShown;
      $("agent-copy-btn").classList.remove("hidden");
    } else if (agentStatus && agentStatus.tokenCount > 0) {
      el.textContent = agentStatus.tokenCount + " key(s) configured — hidden";
      $("agent-copy-btn").classList.add("hidden");
    } else {
      el.textContent = "Not created yet";
      $("agent-copy-btn").classList.add("hidden");
    }
  }

  function refreshAgentPanel() {
    var on = $("set-agent-access").checked;
    $("agent-panel").classList.toggle("hidden", !on);
    if (!on) return;
    if (!hasTauri) return;
    cmd("get_agent_status").then(function (st) {
      agentStatus = st;
      $("set-agent-admin").checked = !!st.allowAdminScope;
      $("agent-endpoint").textContent = st.baseUrl || "Starting…";
      // The admin-key button only makes sense while the ceiling is open: the
      // backend refuses to mint an admin token otherwise, so offering the
      // button would just produce an error the user cannot act on.
      $("agent-admin-token-btn").classList.toggle("hidden", !st.allowAdminScope);
      renderAgentToken();
    }).catch(function () {});
  }

  $("set-agent-access").addEventListener("change", refreshAgentPanel);

  $("agent-copy-btn").addEventListener("click", function () {
    if (!agentTokenShown) return;
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(agentTokenShown).then(function () {
        toast("API key copied");
      }).catch(function () { toast("Could not copy; select the key manually."); });
    }
  });

  $("agent-new-token-btn").addEventListener("click", function () {
    var scope = "write";
    cmd("mint_agent_token", { scope: scope, label: "Created from Settings" })
      .then(function (token) {
        agentTokenShown = token;
        $("agent-token-note").classList.remove("hidden");
        renderAgentToken();
        refreshAgentPanel();
      })
      .catch(function (e) {
        $("agent-error").textContent = String(e);
        $("agent-error").classList.remove("hidden");
      });
  });

  $("agent-admin-token-btn").addEventListener("click", function () {
    // Admin keys can delete services and replace the whole configuration, so
    // ask first and say plainly what is being handed over.
    openConfirm(
      "Create an ADMIN key? It can delete services (destroying saved logins) and replace your entire configuration.",
      function () {
        cmd("mint_agent_token", { scope: "admin", label: "Admin key from Settings" })
          .then(function (token) {
            agentTokenShown = token;
            $("agent-token-note").classList.remove("hidden");
            renderAgentToken();
            refreshAgentPanel();
          })
          .catch(function (e) {
            $("agent-error").textContent = String(e);
            $("agent-error").classList.remove("hidden");
          });
      },
      "Create admin key"
    );
  });

  $("agent-revoke-btn").addEventListener("click", function () {
    openConfirm("Revoke ALL API keys? Any agent using them stops working immediately.", function () {
      cmd("revoke_agent_tokens").then(function (n) {
        agentTokenShown = null;
        toast("Revoked " + n + " key(s).");
        refreshAgentPanel();
      }).catch(function () {});
    });
  });

  $("set-agent-admin").addEventListener("change", function () {
    var allow = $("set-agent-admin").checked;
    if (allow) {
      // Granting unlocks irreversible tools, so confirm first. openConfirm
      // exposes no cancel callback, so attach a one-shot listener that reverts
      // the checkbox if the user backs out.
      var cancel = $("confirm-cancel");
      var revert = function () {
        cancel.removeEventListener("click", revert);
        refreshAgentPanel();
      };
      cancel.addEventListener("click", revert);
      openConfirm(
        "Allow admin-scope tools? An admin key can DELETE services and saved logins, and REPLACE your whole configuration.",
        function () {
          cancel.removeEventListener("click", revert);
          cmd("set_agent_admin_scope", { allow: true })
            .then(refreshAgentPanel)
            .catch(function () { refreshAgentPanel(); });
        },
        "Allow admin tools"
      );
    } else {
      // Revoking admin never needs confirmation: it strictly reduces power.
      cmd("set_agent_admin_scope", { allow: false })
        .then(refreshAgentPanel)
        .catch(function () {});
    }
  });

  /* Troubleshooting footer: version + resolved data dir; DevTools button
   * only while developer mode is on. */
  function refreshSettingsFooter() {
    var s = (currentState && currentState.settings) || {};
    var footer = $("settings-footer");
    var parts = [];
    if (currentState && currentState.dataDir) parts.push(currentState.dataDir);
    if (hasTauri && tauri.app && tauri.app.getVersion) {
      tauri.app.getVersion().then(function (v) {
        footer.textContent = "bzdium v" + v + (currentState && currentState.dataDir ? "  ·  " + currentState.dataDir : "");
      }).catch(function () {});
    } else {
      footer.textContent = parts.join("  ·  ");
    }
    footer.classList.remove("hidden");
    $("dev-actions").classList.toggle("hidden", !s.devMode);
  }

  $("settings-btn").addEventListener("click", openSettingsModal);

  $("devtools-ui-btn").addEventListener("click", function () {
    cmd("open_devtools", { id: null }).catch(function () {});
  });

  $("settings-form").addEventListener("submit", function (e) {
    e.preventDefault();
    var minutes = parseInt($("set-hibernation").value, 10);
    if (isNaN(minutes) || minutes < 0) {
      $("settings-error").textContent = "Hibernation timeout must be a number ≥ 0.";
      $("settings-error").classList.remove("hidden");
      return;
    }
    var prevRuntime = (currentState && currentState.settings && currentState.settings.webview2RuntimeFolder) || null;
    // Options load asynchronously on modal open; if they have not arrived
    // yet, keep the stored pin instead of silently clearing it.
    var wv2rtSel = $("set-wv2rt");
    var runtimeFolder = wv2rtSel.options.length ? (wv2rtSel.value || null) : prevRuntime;
    cmd("update_settings", {
      settings: {
        hibernationMinutes: minutes,
        minimizeToTray: $("set-tray").checked,
        startWithWindows: $("set-autostart").checked,
        showUrlBar: $("set-urlbar").checked,
        darkUi: $("set-dark").checked,
        devMode: $("set-devmode").checked,
        autoRecover: $("set-autorecover").checked,
        linkTarget: $("set-link-bzdium").checked ? "bzdium" : "browser",
        agentAccess: $("set-agent-access").checked,
        webview2RuntimeFolder: runtimeFolder,
        grokModel: $("set-grok-model").value || null
      }
    }).then(function () {
      closeModal("settings-modal");
      // The runtime pin is read once at startup; a change needs a restart.
      if (runtimeFolder !== prevRuntime) {
        toast("WebView2 runtime pin saved — restart bzdium to apply it.");
      }
    }).catch(function () {});
  });

  /* ----- Export / import configuration ----- */

  $("export-config-btn").addEventListener("click", function () {
    cmd("export_config").then(function (path) {
      if (path) toast("Configuration exported to " + path);
    }).catch(function () {});
  });

  $("import-config-btn").addEventListener("click", function () {
    closeModal("settings-modal");
    openConfirm(
      "Import a configuration file? This REPLACES all current services and settings (saved sessions are kept).",
      function () {
        cmd("import_config").then(function (outcome) {
          if (!outcome) return; // cancelled file picker
          if (outcome.skipped && outcome.skipped.length) {
            toast("Imported with skipped entries: " + outcome.skipped.join(", "));
          } else {
            toast("Configuration imported.");
          }
        }).catch(function () {});
      },
      "Import"
    );
  });

  /* ---------------- Grok assistant (native "Grok API" service) ----------------
   * The backend (grok.rs) owns the API key and the https transport; this
   * surface renders conversations and consumes grok-chunk / grok-done /
   * grok-error events. The key never reaches this webview. */

  var grok = {
    conversations: [],     // newest first, as reported by the backend
    currentId: null,       // null = new, not-yet-saved conversation
    draft: [],             // messages of the unsaved conversation
    streaming: false,
    requestId: null,
    lastRequest: null,     // { model, messages } of the latest send (Retry)
    models: null,          // live model list from grok_test_connection
    backendDefault: null,  // backend's recommended default model
    keyConfigured: null,   // from grok_key_status
    loaded: false          // one-time per-session data fetch done
  };

  function grokConv() {
    for (var i = 0; i < grok.conversations.length; i++) {
      if (grok.conversations[i].id === grok.currentId) return grok.conversations[i];
    }
    return null;
  }

  function grokMessages() {
    var conv = grokConv();
    return conv ? conv.messages : grok.draft;
  }

  function grokResetError() {
    $("grok-error").classList.add("hidden");
    $("grok-retry").classList.add("hidden");
    $("grok-callout").classList.add("hidden");
  }

  function grokShowError(message, opts) {
    $("grok-error-text").textContent = message;
    $("grok-error").classList.remove("hidden");
    $("grok-retry").classList.toggle("hidden", !(opts && opts.retryable));
    $("grok-callout").classList.toggle("hidden", !(opts && opts.needKey));
    if (opts && opts.needKey) $("grok-error").classList.add("hidden");
  }

  /* One-time-per-session fetch of conversations, key state and the live
   * model list. A failed model fetch is not sticky: Test in Settings
   * refreshes it. */
  function ensureGrokData() {
    if (grok.loaded || !hasTauri) return;
    grok.loaded = true;
    cmd("grok_list_conversations").then(function (list) {
      grok.conversations = list || [];
      renderGrokHeader();
      renderGrokMessages();
    }).catch(function () {});
    cmd("grok_key_status").then(function (st) {
      grok.keyConfigured = !!st.configured;
      renderGrokMessages();
    }).catch(function () {});
    refreshGrokModels().catch(function () {});
  }

  function refreshGrokModels() {
    return cmd("grok_test_connection").then(function (res) {
      if (res && res.ok && res.models && res.models.length) {
        grok.models = res.models;
        grok.backendDefault = res.defaultModel || res.models[0];
      } else if (res && res.error && res.error.kind !== "auth") {
        toast(res.error.message || "Grok connection test failed.");
      }
      grok.keyConfigured = !(res && res.error && res.error.kind === "auth");
      renderGrokHeader();
      renderGrokMessages();
      return res;
    });
  }

  /* Default pick comes from the backend's `grok_test_connection` (flagship
   * family first over whatever the API returned — never a hardcoded list);
   * the user's saved choice in Settings wins over it. */
  function grokDefaultModel() {
    var configured = currentState && currentState.settings && currentState.settings.grokModel;
    if (configured) return configured;
    return grok.backendDefault || "";
  }

  function grokSelectedModel() {
    var sel = $("grok-model");
    return sel.value || grokDefaultModel();
  }

  function setGrokModel(model) {
    if (!currentState) return;
    var settings = JSON.parse(JSON.stringify(currentState.settings));
    settings.grokModel = model || null;
    cmd("update_settings", { settings: settings }).then(function (state) {
      render(state);
    }).catch(function () {});
  }

  function refreshGrokChat(activeService) {
    ensureGrokData();
    grokResetError();
    $("grok-heading").textContent = activeService.name || "Grok";
    var icon = $("grok-title-icon");
    icon.textContent = "";
    var slug = activeService.iconId && brandManifest && brandManifest[activeService.iconId];
    if (slug) {
      icon.className = "svc-icon svc-icon-brand";
      icon.style.setProperty("--brand", brandManifest[activeService.iconId]);
      var glyph = document.createElement("img");
      glyph.src = "assets/icons/" + activeService.iconId + ".svg";
      glyph.alt = "";
      glyph.draggable = false;
      icon.appendChild(glyph);
    }
    renderGrokHeader();
    renderGrokMessages();
  }

  function renderGrokHeader() {
    var modelSel = $("grok-model");
    var selected = grokDefaultModel();
    modelSel.textContent = "";
    var models = grok.models;
    if (models && models.length) {
      models.forEach(function (id) {
        var opt = document.createElement("option");
        opt.value = id;
        opt.textContent = id;
        if (id === selected) opt.selected = true;
        modelSel.appendChild(opt);
      });
      if (selected && !models.some(function (m) { return m === selected; })) {
        var extra = document.createElement("option");
        extra.value = selected;
        extra.textContent = selected + " (not in current list)";
        extra.selected = true;
        modelSel.appendChild(extra);
      }
    } else {
      var only = document.createElement("option");
      only.value = selected || "";
      only.textContent = selected || "Connect to load models";
      modelSel.appendChild(only);
    }

    var hist = $("grok-history");
    hist.textContent = "";
    var newOpt = document.createElement("option");
    newOpt.value = "";
    newOpt.textContent = "New conversation";
    hist.appendChild(newOpt);
    grok.conversations.forEach(function (c) {
      var opt = document.createElement("option");
      opt.value = c.id;
      opt.textContent = c.title || "(untitled)";
      if (c.id === grok.currentId) opt.selected = true;
      hist.appendChild(opt);
    });
    if (!grok.currentId) newOpt.selected = true;
  }

  function grokMessageEl(msg, streaming) {
    var el = document.createElement("div");
    el.className = "grok-msg grok-msg-" + (msg.role === "user" ? "user" : "assistant");
    if (streaming) el.classList.add("streaming");
    el.textContent = msg.content;
    if (msg.meta) {
      var meta = document.createElement("span");
      meta.className = "grok-msg-meta";
      meta.textContent = msg.meta;
      el.appendChild(meta);
    }
    return el;
  }

  function renderGrokMessages() {
    var wrap = $("grok-messages");
    wrap.textContent = "";
    var msgs = grokMessages();

    if (!msgs.length) {
      var empty = document.createElement("div");
      empty.className = "grok-empty";
      var p = document.createElement("p");
      p.textContent = grok.keyConfigured === false
        ? "Connect the assistant with your xAI API key to start chatting. Grok API usage is billed by xAI separately from any Grok website subscription."
        : "Send a message to talk to " + (grokSelectedModel() || "Grok") + ". Answers stream in as they are generated. Grok API usage is billed by xAI — it is separate from any Grok website subscription.";
      empty.appendChild(p);
      wrap.appendChild(empty);
    }
    msgs.forEach(function (msg, i) {
      wrap.appendChild(grokMessageEl(msg, grok.streaming && i === msgs.length - 1 && msg.role === "assistant"));
    });
    wrap.scrollTop = wrap.scrollHeight;

    var busy = grok.streaming;
    $("grok-status").textContent = busy ? "Generating…" : "";
    $("grok-status").classList.toggle("busy", busy);
    $("grok-send").disabled = busy;
    $("grok-cancel").classList.toggle("hidden", !busy);
  }

  function grokPersist() {
    var msgs = grokMessages();
    if (!msgs.length) return;
    var id = grok.currentId || grokNewId();
    var now = Math.floor(Date.now() / 1000);
    var existing = grokConv();
    var conv = {
      id: id,
      title: existing ? existing.title : String(msgs[0].content).slice(0, 60),
      model: grokSelectedModel(),
      createdAt: existing ? existing.createdAt : now,
      updatedAt: now,
      messages: msgs
    };
    grok.currentId = id;
    cmd("grok_save_conversation", { conversation: conv })
      .then(function () {
        var at = grok.conversations.findIndex(function (c) { return c.id === conv.id; });
        if (at >= 0) {
          // A newer save of the same conversation may already be listed —
          // never let an older in-flight response regress it.
          if (grok.conversations[at].updatedAt <= conv.updatedAt) grok.conversations[at] = conv;
        } else {
          grok.conversations.unshift(conv);
        }
        renderGrokHeader();
      })
      .catch(function () {});
  }

  function grokNewId() {
    if (window.crypto && crypto.randomUUID) return crypto.randomUUID();
    return "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g, function (c) {
      var r = (Math.random() * 16) | 0;
      return (c === "x" ? r : (r & 0x3) | 0x8).toString(16);
    });
  }

  function grokSend() {
    if (grok.streaming) return;
    var input = $("grok-input");
    var text = input.value.trim();
    if (!text) return;
    if (grok.keyConfigured === false) {
      grokShowError("No API key configured. Add your xAI API key in Settings → Grok assistant, then retry.", { needKey: true });
      return;
    }
    var model = grokSelectedModel();
    if (!model) {
      grokShowError("No model selected. Press “Test” in Settings → Grok assistant to load the model list.", {});
      return;
    }
    input.value = "";
    grokMessages().push({ role: "user", content: text });
    grokPersist();
    grokRequest(model, grokMessages().slice());
    renderGrokMessages();
  }

  function grokRequest(model, messages) {
    grok.streaming = true;
    grok.requestId = grokNewId();
    grok.lastRequest = { model: model, messages: messages };
    renderGrokMessages();
    cmd("grok_send_message", { requestId: grok.requestId, model: model, messages: messages })
      .catch(function (err) {
        // The command itself only fails on malformed calls; real provider
        // failures arrive as grok-error events.
        grokHandleError({
          requestId: grok.requestId,
          kind: "unknown",
          message: typeof err === "string" ? err : "Could not start the request.",
          retryable: false
        });
      });
  }

  function grokHandleChunk(payload) {
    if (!payload || payload.requestId !== grok.requestId || !payload.delta) return;
    var msgs = grokMessages();
    var last = msgs[msgs.length - 1];
    if (!last || last.role !== "assistant") {
      msgs.push({ role: "assistant", content: payload.delta });
    } else {
      last.content += payload.delta;
    }
    // Cheap in-place update: rewrite the last bubble's text, keep scroll.
    var wrap = $("grok-messages");
    var bubble = wrap.querySelector(".grok-msg-assistant:last-child");
    if (bubble && !bubble.classList.contains("streaming")) bubble.classList.add("streaming");
    if (bubble) {
      bubble.textContent = msgs[msgs.length - 1].content;
      wrap.scrollTop = wrap.scrollHeight;
    } else {
      renderGrokMessages();
    }
  }

  function grokHandleDone(payload) {
    if (!payload || payload.requestId !== grok.requestId) return;
    grok.streaming = false;
    grok.requestId = null;
    var msgs = grokMessages();
    var last = msgs[msgs.length - 1];
    if (last && last.role === "assistant") {
      var meta = payload.model ? payload.model : "";
      if (payload.usage && payload.usage.completionTokens) {
        meta += (meta ? " · " : "") + payload.usage.completionTokens + " tokens";
      }
      if (meta) last.meta = meta;
    }
    grokPersist();
    renderGrokMessages();
  }

  function grokHandleError(payload) {
    if (!payload || payload.requestId !== grok.requestId) return;
    grok.streaming = false;
    grok.requestId = null;
    // Drop an empty assistant placeholder so no hollow bubble lingers.
    var msgs = grokMessages();
    var last = msgs[msgs.length - 1];
    if (last && last.role === "assistant" && !last.content) msgs.pop();
    // Permanent failures (bad key, no credits, retired model) must not
    // leave a Retry path that can only fail the same way again.
    var permanent = payload.kind === "auth" || payload.kind === "credits" || payload.kind === "model";
    if (permanent) grok.lastRequest = null;
    grokShowError(payload.message || "Something went wrong talking to the Grok API.", {
      retryable: !!payload.retryable && !!grok.lastRequest
    });
    renderGrokMessages();
  }

  function grokStartNewChat() {
    grok.currentId = null;
    grok.draft = [];
    grokResetError();
    renderGrokHeader();
    renderGrokMessages();
    $("grok-input").focus();
  }

  function grokClearHistory() {
    cmd("grok_clear_history").then(function () {
      grok.conversations = [];
      grok.currentId = null;
      grok.draft = [];
      renderGrokHeader();
      renderGrokMessages();
      toast("Grok conversation history deleted.");
    }).catch(function () {});
  }

  $("grok-composer").addEventListener("submit", function (e) {
    e.preventDefault();
    grokSend();
  });
  $("grok-input").addEventListener("keydown", function (e) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      grokSend();
    }
  });
  $("grok-cancel").addEventListener("click", function () {
    if (grok.requestId) cmd("grok_cancel_request", { requestId: grok.requestId }).catch(function () {});
  });
  $("grok-new-chat").addEventListener("click", grokStartNewChat);
  $("grok-history").addEventListener("change", function () {
    grok.currentId = this.value || null;
    grok.draft = [];
    grokResetError();
    renderGrokMessages();
    $("grok-input").focus();
  });
  $("grok-model").addEventListener("change", function () {
    setGrokModel(this.value || null);
  });
  $("grok-retry").addEventListener("click", function () {
    grokResetError();
    if (grok.lastRequest && !grok.streaming) {
      grokRequest(grok.lastRequest.model, grok.lastRequest.messages);
    }
  });
  $("grok-dismiss").addEventListener("click", grokResetError);
  $("grok-callout-configure").addEventListener("click", function () {
    openSettingsModal();
  });
  $("grok-callout-console").addEventListener("click", function () {
    cmd("open_url_external", { url: "https://console.x.ai/" }).catch(function () {});
  });

  /* ----- Settings → Grok assistant panel ----- */

  function renderGrokKeyState(status) {
    var el = $("grok-key-state");
    if (status && status.configured) {
      el.textContent = status.masked || "Configured";
    } else {
      el.textContent = status && status.platformSupported === false
        ? "Not supported on this platform"
        : "Not configured";
    }
    $("grok-key-remove").disabled = !(status && status.configured);
  }

  function fillGrokModelSelects() {
    var sel = $("set-grok-model");
    var current = (currentState && currentState.settings && currentState.settings.grokModel) || grokDefaultModel();
    sel.textContent = "";
    var models = (grok.models && grok.models.length) ? grok.models : (current ? [current] : []);
    if (!models.length) {
      var none = document.createElement("option");
      none.value = "";
      none.textContent = "Press Test to load models";
      sel.appendChild(none);
      return;
    }
    models.forEach(function (id) {
      var opt = document.createElement("option");
      opt.value = id;
      opt.textContent = id;
      if (id === current) opt.selected = true;
      sel.appendChild(opt);
    });
    if (current && !models.some(function (m) { return m === current; })) {
      var extra = document.createElement("option");
      extra.value = current;
      extra.textContent = current + " (not in current list)";
      extra.selected = true;
      sel.appendChild(extra);
    }
  }

  function refreshGrokPanel() {
    cmd("grok_key_status").then(renderGrokKeyState).catch(function () {});
    $("grok-key-input").value = "";
    $("grok-connection").textContent = grok.models ? grok.models.length + " model(s) loaded" : "—";
    fillGrokModelSelects();
  }

  $("grok-key-save").addEventListener("click", function () {
    var key = $("grok-key-input").value;
    if (!key.trim()) {
      var errEl = $("grok-error");
      errEl.textContent = "Paste an API key first (it starts with “xai-”).";
      errEl.classList.remove("hidden");
      return;
    }
    cmd("grok_save_key", { key: key }).then(function (status) {
      $("grok-key-input").value = "";
      renderGrokKeyState(status);
      var errEl = $("grok-error");
      errEl.classList.add("hidden");
      toast("API key saved — stored encrypted, never shown in full again.");
      grokTestConnection();
    }).catch(function (e) {
      var errEl = $("grok-error");
      errEl.textContent = typeof e === "string" ? e : "Could not save the key.";
      errEl.classList.remove("hidden");
    });
  });

  $("grok-key-remove").addEventListener("click", function () {
    openConfirm(
      "Remove the stored xAI API key from this device? Existing conversations stay, but new messages need a key again.",
      function () {
        cmd("grok_delete_key").then(function (status) {
          renderGrokKeyState(status);
          grok.models = null;
          grok.keyConfigured = false;
          $("grok-connection").textContent = "—";
          fillGrokModelSelects();
          toast("API key removed.");
        }).catch(function () {});
      },
      "Remove key"
    );
  });

  function grokTestConnection() {
    $("grok-connection").textContent = "Testing…";
    refreshGrokModels().then(function (res) {
      if (res && res.ok) {
        $("grok-connection").textContent = "OK — " + res.models.length + " model(s) available";
        var errEl = $("grok-error");
        errEl.classList.add("hidden");
      } else if (res && res.error) {
        $("grok-connection").textContent = "Failed";
        var errEl = $("grok-error");
        errEl.textContent = res.error.message || "Connection test failed.";
        errEl.classList.remove("hidden");
      }
      fillGrokModelSelects();
    }).catch(function () {
      $("grok-connection").textContent = "Failed";
    });
  }
  $("grok-test-btn").addEventListener("click", grokTestConnection);

  function openExternal(url) {
    cmd("open_url_external", { url: url }).catch(function () {});
  }
  $("grok-open-console").addEventListener("click", function () { openExternal("https://console.x.ai/"); });
  $("grok-open-docs").addEventListener("click", function () { openExternal("https://docs.x.ai/overview"); });
  $("grok-open-site").addEventListener("click", function () { openExternal("https://grok.com/"); });

  $("grok-clear-history").addEventListener("click", function () {
    openConfirm(
      "Delete ALL Grok conversation history stored in bzdium? This cannot be undone.",
      function () { grokClearHistory(); },
      "Delete"
    );
  });

  /* ---------------- Per-app font settings ----------------
   * The backend owns FontConfig (clamping, validation, persistence). This
   * panel is a view over currentState.services[*].font: every mutation goes
   * through set_font_config / adjust_font_size and the panel re-renders from
   * the snapshot those commands return or from the font-changed event. */

  var FONT_CUSTOM_VALUE = "__custom__";
  var FONT_SAVE_DEBOUNCE_MS = 200;
  var FONT_DEFAULT_SHORTCUTS = {
    increase: "Ctrl+=",
    decrease: "Ctrl+-",
    reset: "Ctrl+0",
    openSettings: "Ctrl+Shift+F"
  };
  var FONT_SHORTCUT_ACTIONS = [
    { key: "increase", label: "Increase font size" },
    { key: "decrease", label: "Decrease font size" },
    { key: "reset", label: "Reset font size" },
    { key: "openSettings", label: "Font settings" }
  ];
  var FONT_PANEL_CONTROLS = [
    "font-family", "font-custom", "font-size", "font-slider",
    "font-enabled", "font-scale-px", "font-reset", "font-apply-all",
    "font-shortcuts-reset"
  ];

  var fontPanelId = null;
  var fontPanelStatusId = null;
  var fontSavedConfig = null;
  var fontFamilies = null;
  var fontFamiliesRequested = false;
  var fontFamilySignature = null;
  var fontAppSignature = null;
  var fontSaveTimer = null;
  var fontShellActions = {};     // id -> when a shell-initiated change happened

  /* Mirrors the Rust FontConfig defaults: used by the browser preview and to
   * tolerate a snapshot from a backend that predates this feature. */
  function defaultFontConfig() {
    return {
      family: null,
      sizePx: 16,
      defaultSizePx: 16,
      minSizePx: 8,
      maxSizePx: 40,
      stepPx: 1,
      enabled: true,
      scalePxText: true,
      shortcuts: {
        increase: "Ctrl+=",
        decrease: "Ctrl+-",
        reset: "Ctrl+0",
        openSettings: "Ctrl+Shift+F"
      }
    };
  }

  function isSettingsOpen() {
    return !$("settings-modal").classList.contains("hidden");
  }

  function firstServiceId() {
    return (currentState && currentState.services && currentState.services.length)
      ? currentState.services[0].id : null;
  }

  function numberOr(value, fallback) {
    return (typeof value === "number" && isFinite(value)) ? value : fallback;
  }

  function errorText(err) {
    if (typeof err === "string") return err;
    if (err && err.message) return String(err.message);
    return "Could not save the font settings.";
  }

  function fontSummary(cfg) {
    var family = (cfg && cfg.family) ? cfg.family : "System default";
    var text = family + " · " + numberOr(cfg && cfg.sizePx, 16) + "px";
    if (cfg && cfg.enabled === false) text += " · overrides off";
    return text;
  }

  function setFontStatus(cfg, suffix) {
    var text = cfg ? fontSummary(cfg) : "";
    if (text && suffix) text += " — " + suffix;
    $("font-status").textContent = text;
  }

  function showFontError(message) {
    $("font-error").textContent = message;
    $("font-error").classList.remove("hidden");
  }

  function hideFontError() {
    $("font-error").textContent = "";
    $("font-error").classList.add("hidden");
  }

  function fontConfigFor(id) {
    var svc = id ? serviceById(id) : null;
    return (svc && svc.font) ? svc.font : null;
  }

  function panelService() {
    return fontPanelId ? serviceById(fontPanelId) : null;
  }

  function shortcutInput(action) {
    return $("font-shortcuts").querySelector('.shortcut-input[data-action="' + action + '"]');
  }

  function ensureFontFamilies() {
    if (fontFamilies || fontFamiliesRequested) return;
    fontFamiliesRequested = true;
    cmd("list_font_families", {}).then(function (list) {
      fontFamilies = Array.isArray(list) ? list.slice() : [];
      fontFamilySignature = null;
      if (isSettingsOpen() && fontPanelId) refreshFontPanel(false);
    }).catch(function () {
      fontFamilies = [];
      fontFamiliesRequested = false;
    });
  }

  function setControlValue(el, value, force) {
    if (!el) return;
    if (!force && el === document.activeElement) return;
    if (el.value !== value) el.value = value;
  }

  function setCheckbox(id, checked, force) {
    var el = $(id);
    if (!force && el === document.activeElement) return;
    el.checked = checked;
  }

  function panelMetrics() {
    var svc = panelService();
    var cfg = (svc && svc.font) || defaultFontConfig();
    var min = numberOr(cfg.minSizePx, 8);
    var max = numberOr(cfg.maxSizePx, 40);
    if (max <= min) max = min + 1;
    return {
      min: min,
      max: max,
      step: numberOr(cfg.stepPx, 1),
      defaultSize: numberOr(cfg.defaultSizePx, 16)
    };
  }

  function selectedFamilyFromInputs() {
    var sel = $("font-family");
    if (!sel) return null;
    if (sel.value === FONT_CUSTOM_VALUE) {
      var custom = $("font-custom");
      var value = custom ? custom.value.trim() : "";
      return value || null;
    }
    return sel.value || null;
  }

  function selectedSizeFromInputs() {
    var metrics = panelMetrics();
    // sizePx is an f64 backend-side; keep fractional hand-edited values.
    var raw = parseFloat($("font-size").value);
    if (isNaN(raw)) raw = metrics.defaultSize;
    return Math.max(metrics.min, Math.min(metrics.max, raw));
  }

  function paintFontPreview(family, size) {
    var el = $("font-preview");
    if (family) el.style.fontFamily = family;
    else el.style.removeProperty("font-family");
    el.style.fontSize = String(numberOr(size, 16)) + "px";
  }

  /* -/+ disable at the ends of the range; a locked (assistant) panel stays
   * disabled regardless of the current size. */
  function updateFontStepButtons(size) {
    var svc = panelService();
    var locked = !svc || svc.kind === "grokApi";
    var metrics = panelMetrics();
    $("font-dec").disabled = locked || size <= metrics.min;
    $("font-inc").disabled = locked || size >= metrics.max;
  }

  /* Builds the whole FontConfig the backend expects from the panel. `set_*`
   * always saves the complete object, shortcuts included. */
  function buildConfigFromPanel() {
    var svc = panelService();
    if (!svc) return null;
    var base = svc.font || defaultFontConfig();
    var metrics = panelMetrics();
    var shortcuts = {};
    FONT_SHORTCUT_ACTIONS.forEach(function (action) {
      var input = shortcutInput(action.key);
      var saved = (base.shortcuts && base.shortcuts[action.key]) || FONT_DEFAULT_SHORTCUTS[action.key];
      shortcuts[action.key] = input ? (input.value.trim() || saved) : saved;
    });
    return {
      family: selectedFamilyFromInputs(),
      sizePx: selectedSizeFromInputs(),
      defaultSizePx: numberOr(base.defaultSizePx, 16),
      minSizePx: metrics.min,
      maxSizePx: metrics.max,
      stepPx: metrics.step,
      enabled: $("font-enabled").checked,
      scalePxText: $("font-scale-px").checked,
      shortcuts: shortcuts
    };
  }

  function cancelFontPanelSave() {
    if (fontSaveTimer) {
      clearTimeout(fontSaveTimer);
      fontSaveTimer = null;
    }
  }

  function scheduleFontPanelSave() {
    cancelFontPanelSave();
    fontSaveTimer = setTimeout(function () {
      fontSaveTimer = null;
      saveFontPanelConfig();
    }, FONT_SAVE_DEBOUNCE_MS);
  }

  function flushFontPanelSave() {
    cancelFontPanelSave();
    saveFontPanelConfig();
  }

  function saveFontPanelConfig() {
    var svc = panelService();
    if (!svc) return;
    var config = buildConfigFromPanel();
    if (!config) return;
    cancelFontPanelSave();
    hideFontError();
    var id = fontPanelId;
    cmd("set_font_config", { id: id, config: config })
      .then(function (state) {
        if (state) render(state);
        fontSavedConfig = fontConfigFor(id) || config;
        setFontStatus(fontSavedConfig, "saved");
      })
      .catch(function (err) {
        var message = errorText(err);
        showFontError(message);
        $("font-status").textContent = message;
      });
  }

  function renderFontAppOptions(force) {
    var sel = $("font-app");
    var services = (currentState && currentState.services) || [];
    var sig = services.map(function (s) {
      return s.id + "\u0001" + (s.name || "") + "\u0001" + (s.kind || "");
    }).join("\u0002");
    if (sig !== fontAppSignature && (force || document.activeElement !== sel)) {
      fontAppSignature = sig;
      sel.textContent = "";
      services.forEach(function (s) {
        var opt = document.createElement("option");
        opt.value = s.id;
        opt.textContent = s.name || s.id;
        sel.appendChild(opt);
      });
    }
    sel.value = fontPanelId || "";
  }

  /* Family select: "System default", the backend's families, then "Custom…".
   * An unavailable/hand-edited family stays visible as a prefilled custom
   * value instead of silently resetting to the system font. */
  function renderFontFamilyOptions(family, force) {
    var sel = $("font-family");
    var custom = $("font-custom");
    // Never yank the field out from under someone typing a custom family; the
    // next refresh after blur reconciles the select with the saved value.
    if (!force && document.activeElement === custom) return;
    var families = fontFamilies || [];
    var isCustom = !!family && families.indexOf(family) === -1;
    var sig = families.join("\u0001") + "\u0002" + (family || "") + "\u0002" + (fontFamilies ? "y" : "n");
    if (sig !== fontFamilySignature && (force || document.activeElement !== custom)) {
      fontFamilySignature = sig;
      sel.textContent = "";
      var system = document.createElement("option");
      system.value = "";
      system.textContent = "System default";
      sel.appendChild(system);
      families.forEach(function (name) {
        var opt = document.createElement("option");
        opt.value = name;
        opt.textContent = name;
        sel.appendChild(opt);
      });
      var customOpt = document.createElement("option");
      customOpt.value = FONT_CUSTOM_VALUE;
      customOpt.textContent = "Custom…";
      sel.appendChild(customOpt);
    }
    var target = isCustom ? FONT_CUSTOM_VALUE : (family || "");
    var hasOption = target === "" || target === FONT_CUSTOM_VALUE || families.indexOf(target) !== -1;
    if (force || document.activeElement !== sel) {
      sel.value = hasOption ? target : FONT_CUSTOM_VALUE;
    }
    var customMode = sel.value === FONT_CUSTOM_VALUE;
    $("font-custom-row").classList.toggle("hidden", !customMode);
    if (customMode) {
      if (force || document.activeElement !== custom) custom.value = isCustom ? family : "";
    } else {
      custom.value = "";
    }
  }

  function ensureShortcutRows() {
    var wrap = $("font-shortcuts");
    if (wrap.firstChild) return;
    FONT_SHORTCUT_ACTIONS.forEach(function (action) {
      var row = document.createElement("div");
      row.className = "shortcut-row";
      var label = document.createElement("span");
      label.className = "settings-field-label";
      label.textContent = action.label;
      var input = document.createElement("input");
      input.type = "text";
      input.className = "shortcut-input";
      input.readOnly = true;
      input.autocomplete = "off";
      input.spellcheck = false;
      input.setAttribute("data-action", action.key);
      input.setAttribute("aria-label", action.label + " shortcut");
      input.placeholder = FONT_DEFAULT_SHORTCUTS[action.key] || "";
      input.addEventListener("keydown", onShortcutKeydown);
      row.appendChild(label);
      row.appendChild(input);
      wrap.appendChild(row);
    });
  }

  function refreshFontPanel(force) {
    if (!currentState) return;
    var services = currentState.services || [];
    if (!fontPanelId || !serviceById(fontPanelId)) {
      fontPanelId = currentState.activeServiceId || (services.length ? services[0].id : null);
    }
    ensureFontFamilies();
    renderFontAppOptions(force);

    var svc = panelService();
    var isApi = !!(svc && svc.kind === "grokApi");
    var noService = !svc;
    var cfg = (svc && svc.font) || defaultFontConfig();
    var metrics = panelMetrics();
    var size = Math.max(metrics.min, Math.min(metrics.max, numberOr(cfg.sizePx, metrics.defaultSize)));

    $("font-size").setAttribute("min", String(metrics.min));
    $("font-size").setAttribute("max", String(metrics.max));
    $("font-size").setAttribute("step", String(metrics.step));
    $("font-slider").setAttribute("min", String(metrics.min));
    $("font-slider").setAttribute("max", String(metrics.max));
    $("font-slider").setAttribute("step", String(metrics.step));

    FONT_PANEL_CONTROLS.forEach(function (id) { $(id).disabled = noService || isApi; });
    setControlValue($("font-size"), String(size), force);
    setControlValue($("font-slider"), String(size), force);
    updateFontStepButtons(size);

    renderFontFamilyOptions(cfg.family || null, force);
    setCheckbox("font-enabled", cfg.enabled !== false, force);
    setCheckbox("font-scale-px", cfg.scalePxText !== false, force);
    $("font-disabled-note").classList.toggle("hidden", cfg.enabled !== false);
    $("font-app-note").classList.toggle("hidden", !isApi);
    paintFontPreview(cfg.family || null, size);

    ensureShortcutRows();
    FONT_SHORTCUT_ACTIONS.forEach(function (action) {
      var input = shortcutInput(action.key);
      if (!input) return;
      var value = (cfg.shortcuts && cfg.shortcuts[action.key]) || "";
      input.placeholder = FONT_DEFAULT_SHORTCUTS[action.key] || "";
      input.disabled = noService || isApi;
      if (!force && document.activeElement === input) return;
      input.value = shortcutForDisplay(value);
    });

    fontSavedConfig = cfg;

    if (fontPanelStatusId !== fontPanelId) {
      fontPanelStatusId = fontPanelId;
      hideFontError();
      setFontStatus((svc && !isApi) ? cfg : null, null);
    }
  }

  function revealFontSection() {
    var section = $("font-section");
    if (!section || !section.scrollIntoView) return;
    setTimeout(function () {
      section.scrollIntoView({ block: "start" });
    }, 0);
  }

  function openFontSettings(id) {
    if (!currentState) return;
    if (id && !serviceById(id)) return;
    openSettingsModal(id || currentState.activeServiceId || firstServiceId());
  }

  function noteShellFontAction(id) {
    if (id) fontShellActions[id] = Date.now();
  }

  /* adjust_font_size keeps the clamping in Rust; used by the sidebar menu and
   * the +/- buttons, so both show the resulting size as soon as it lands. */
  function adjustServiceFont(service, action) {
    if (!service) return;
    noteShellFontAction(service.id);
    cmd("adjust_font_size", { id: service.id, action: action })
      .then(function (cfg) {
        if (cfg && typeof cfg.sizePx === "number") toast("Font size " + cfg.sizePx + "px");
      })
      .catch(function () {});
  }

  function adjustPanelFont(action) {
    var svc = panelService();
    if (!svc) return;
    cancelFontPanelSave();
    noteShellFontAction(svc.id);
    cmd("adjust_font_size", { id: svc.id, action: action }).then(function (cfg) {
      if (!cfg) return;
      fontSavedConfig = cfg;
      var min = numberOr(cfg.minSizePx, 8);
      var max = numberOr(cfg.maxSizePx, 40);
      var size = Math.max(min, Math.min(max, numberOr(cfg.sizePx, 16)));
      setControlValue($("font-size"), String(size), false);
      setControlValue($("font-slider"), String(size), false);
      updateFontStepButtons(size);
      paintFontPreview(cfg.family || null, size);
      setFontStatus(cfg, "saved");
      toast("Font size " + size + "px");
    }).catch(function (err) {
      var message = errorText(err);
      showFontError(message);
      $("font-status").textContent = message;
    });
  }

  /* ---- Shortcut capture ---- */

  function shortcutKeyName(e) {
    var key = e.key;
    if (!key || key === "Dead" || key === "Unidentified") return null;
    if (key === "Control" || key === "Alt" || key === "Shift" || key === "Meta" ||
        key === "OS" || key === "AltGraph" || key === "CapsLock" || key === "NumLock" ||
        key === "ScrollLock" || key === "Fn") {
      return null;
    }
    if (key === " ") return "Space";
    if (key.length === 1) return key.toUpperCase();
    return key;
  }

  /* Canonical form: Ctrl+Alt+Shift+Key (only the modifiers actually held). */
  function buildShortcutCombo(e) {
    var key = shortcutKeyName(e);
    if (!key) return null;
    var parts = [];
    var primary = isMacLike ? (e.metaKey || e.ctrlKey) : e.ctrlKey;
    if (primary) parts.push("Ctrl");
    if (e.altKey) parts.push("Alt");
    if (e.shiftKey) parts.push("Shift");
    parts.push(key);
    return parts.join("+");
  }

  function savedShortcutFor(action) {
    var saved = fontSavedConfig && fontSavedConfig.shortcuts
      ? fontSavedConfig.shortcuts[action] : "";
    return shortcutForDisplay(saved || "");
  }

  /* The backend stores the plus key as "Plus" (canonical_accel maps a trailing
   * "+" to it because "+" is also the accelerator separator). Show the more
   * readable "+"/"-" spelling in the editor; the raw value still matches. */
  function shortcutForDisplay(combo) {
    if (!combo) return "";
    if (combo.slice(-4) === "Plus") return combo.slice(0, -4) + "+";
    if (combo.slice(-5) === "Minus") return combo.slice(0, -5) + "-";
    return combo;
  }

  function onShortcutKeydown(e) {
    var input = e.currentTarget;
    // Tab must keep moving focus through the form.
    if (e.key === "Tab") return;
    if (e.key === "Escape") {
      // Abort capture without letting Escape close the whole Settings dialog.
      e.preventDefault();
      e.stopPropagation();
      input.value = savedShortcutFor(input.getAttribute("data-action"));
      input.blur();
      return;
    }
    if (e.isComposing) {
      e.preventDefault();
      return;
    }
    e.preventDefault();
    var combo = buildShortcutCombo(e);
    if (!combo) return; // modifier-only press: wait for a real key
    hideFontError();
    input.value = combo;
    saveCapturedShortcut(input);
  }

  function saveCapturedShortcut(input) {
    var svc = panelService();
    if (!svc) return;
    var action = input.getAttribute("data-action");
    var config = buildConfigFromPanel();
    if (!config) return;
    var id = fontPanelId;
    hideFontError();
    cmd("set_font_config", { id: id, config: config })
      .then(function (state) {
        if (state) render(state);
        fontSavedConfig = fontConfigFor(id) || config;
        setFontStatus(fontSavedConfig, "saved");
      })
      .catch(function (err) {
        var message = errorText(err);
        showFontError(message);
        $("font-status").textContent = message;
        // Conflict / reserved: put the previously saved combination back.
        input.value = savedShortcutFor(action);
      });
  }

  /* ---- Matching shell keys against the saved combinations ---- */

  function parseShortcutCombo(combo) {
    var rest = String(combo || "");
    var ctrl = false;
    var alt = false;
    var shift = false;
    var match = rest.match(/^ctrl\+/i);
    if (match) { ctrl = true; rest = rest.slice(match[0].length); }
    match = rest.match(/^alt\+/i);
    if (match) { alt = true; rest = rest.slice(match[0].length); }
    match = rest.match(/^shift\+/i);
    if (match) { shift = true; rest = rest.slice(match[0].length); }
    if (!rest) return null;
    return { ctrl: ctrl, alt: alt, shift: shift, key: rest };
  }

  function shortcutKeyMatches(configured, e) {
    var key = String(configured || "");
    if (!key) return false;
    if (key === "Space") return e.key === " " || e.key === "Space";
    if (key === "Plus" || key === "+" || key === "=") {
      return e.key === "+" || e.key === "=" || e.code === "NumpadAdd";
    }
    if (key === "Minus" || key === "-") {
      return e.key === "-" || e.key === "_" || e.code === "NumpadSubtract";
    }
    if (key === "0") return e.key === "0" || e.code === "Numpad0";
    if (key.length === 1) return String(e.key).toUpperCase() === key.toUpperCase();
    return e.key === key;
  }

  function shortcutMatchesEvent(combo, e) {
    var parsed = parseShortcutCombo(combo);
    if (!parsed) return false;
    var primary = isMacLike ? e.metaKey : e.ctrlKey;
    if (parsed.ctrl !== primary) return false;
    if (parsed.alt !== e.altKey) return false;
    // Shift is part of typing "=" (Ctrl+Shift+=) and "_" — the page-side
    // bundle ignores it for symbol keys, so the shell does too.
    var symbolKey = parsed.key === "=" || parsed.key === "+" || parsed.key === "Plus" ||
      parsed.key === "-" || parsed.key === "_" || parsed.key === "Minus";
    if (parsed.shift !== e.shiftKey && !(symbolKey && !parsed.shift)) return false;
    if (!isMacLike && e.metaKey) return false;
    return shortcutKeyMatches(parsed.key, e);
  }

  function isEditableTarget(el) {
    if (!el || !el.closest) return false;
    if (el.isContentEditable) return true;
    return !!el.closest("input, textarea, select, [contenteditable=''], [contenteditable='true']");
  }

  function handleShellFontShortcut(e) {
    if (!currentState || !currentState.activeServiceId) return;
    var svc = serviceById(currentState.activeServiceId);
    if (!svc || !svc.font || !svc.font.shortcuts) return;
    var shortcuts = svc.font.shortcuts;
    var matched = null;
    for (var i = 0; i < FONT_SHORTCUT_ACTIONS.length; i++) {
      var action = FONT_SHORTCUT_ACTIONS[i].key;
      if (shortcutMatchesEvent(shortcuts[action], e)) { matched = action; break; }
    }
    if (!matched) return;
    if (anyModalOpen()) return;
    var opensSettings = matched === "openSettings";
    // Typing and IME composition win over size shortcuts; opening the font
    // panel stays available from inside an editable field.
    if (!opensSettings) {
      if (e.isComposing || isEditableTarget(e.target)) return;
      if (svc.kind === "grokApi") return; // the assistant has no page to restyle
    }
    e.preventDefault();
    if (opensSettings) {
      openFontSettings(svc.id);
      return;
    }
    noteShellFontAction(svc.id);
    cmd("adjust_font_size", { id: svc.id, action: matched }).then(function (cfg) {
      if (cfg && typeof cfg.sizePx === "number") toast("Font size " + cfg.sizePx + "px");
    }).catch(function () {});
  }

  function handleFontChanged(payload) {
    if (!payload || !payload.id || !payload.config) return;
    var svc = serviceById(payload.id);
    if (svc) {
      // Authoritative delta from the backend: merge it so the panel and the
      // sidebar menu see the new min/max/size before the next full snapshot.
      svc.font = payload.config;
    }
    var shellInitiated = !!fontShellActions[payload.id];
    delete fontShellActions[payload.id];
    if (isSettingsOpen()) {
      if (fontPanelId === payload.id) refreshFontPanel(false);
      return;
    }
    // The panel is closed: tell the user what changed, unless the shell
    // already reported the size change its own toast.
    if (!shellInitiated) toast(fontSummary(payload.config));
  }

  /* ---- Fonts panel wiring ---- */

  $("font-app").addEventListener("change", function () {
    fontPanelId = this.value || null;
    fontPanelStatusId = null;
    hideFontError();
    refreshFontPanel(true);
  });

  $("font-family").addEventListener("change", function () {
    if (this.value === FONT_CUSTOM_VALUE) {
      var svc = panelService();
      var custom = $("font-custom");
      $("font-custom-row").classList.remove("hidden");
      if (!custom.value) custom.value = (svc && svc.font && svc.font.family) || "";
      paintFontPreview(selectedFamilyFromInputs(), selectedSizeFromInputs());
      custom.focus();
      return;
    }
    $("font-custom-row").classList.add("hidden");
    $("font-custom").value = "";
    paintFontPreview(this.value || null, selectedSizeFromInputs());
    saveFontPanelConfig();
  });

  $("font-custom").addEventListener("input", function () {
    paintFontPreview(selectedFamilyFromInputs(), selectedSizeFromInputs());
    scheduleFontPanelSave();
  });

  $("font-custom").addEventListener("change", function () {
    if (!this.value.trim() && $("font-family").value === FONT_CUSTOM_VALUE) {
      $("font-family").value = "";
      $("font-custom-row").classList.add("hidden");
      paintFontPreview(null, selectedSizeFromInputs());
    }
    flushFontPanelSave();
  });

  $("font-size").addEventListener("input", function () {
    var raw = parseFloat(this.value);
    if (isNaN(raw)) return;
    var metrics = panelMetrics();
    var clamped = Math.max(metrics.min, Math.min(metrics.max, raw));
    $("font-slider").value = String(clamped);
    updateFontStepButtons(clamped);
    paintFontPreview(selectedFamilyFromInputs(), clamped);
    scheduleFontPanelSave();
  });

  $("font-size").addEventListener("change", function () {
    var size = selectedSizeFromInputs();
    this.value = String(size);
    $("font-slider").value = String(size);
    updateFontStepButtons(size);
    paintFontPreview(selectedFamilyFromInputs(), size);
    flushFontPanelSave();
  });

  $("font-slider").addEventListener("input", function () {
    var size = parseFloat(this.value);
    if (isNaN(size)) return;
    $("font-size").value = String(size);
    updateFontStepButtons(size);
    paintFontPreview(selectedFamilyFromInputs(), size);
    scheduleFontPanelSave();
  });
  $("font-slider").addEventListener("change", function () {
    flushFontPanelSave();
  });

  $("font-dec").addEventListener("click", function () { adjustPanelFont("decrease"); });
  $("font-inc").addEventListener("click", function () { adjustPanelFont("increase"); });

  $("font-enabled").addEventListener("change", function () {
    $("font-disabled-note").classList.toggle("hidden", this.checked);
    saveFontPanelConfig();
  });

  $("font-scale-px").addEventListener("change", function () {
    saveFontPanelConfig();
  });

  $("font-reset").addEventListener("click", function () {
    var svc = panelService();
    if (!svc) return;
    openConfirm(
      "Reset the font settings for “" + svc.name + "”? Family, size and options go back to the defaults. Shortcuts are kept.",
      function () {
        cancelFontPanelSave();
        cmd("reset_font_config", { id: svc.id }).then(function (state) {
          if (state) render(state);
          fontSavedConfig = fontConfigFor(svc.id) || defaultFontConfig();
          setFontStatus(fontSavedConfig, "defaults restored");
          toast("Font settings reset.");
        }).catch(function (err) {
          var message = errorText(err);
          showFontError(message);
          $("font-status").textContent = message;
        });
      },
      "Reset"
    );
  });

  $("font-apply-all").addEventListener("click", function () {
    var svc = panelService();
    if (!svc) return;
    openConfirm(
      "Copy “" + svc.name + "”'s font settings and shortcuts to every other app?",
      function () {
        cmd("apply_font_to_all", { id: svc.id }).then(function (state) {
          if (state) render(state);
          var applied = fontConfigFor(svc.id) || defaultFontConfig();
          fontSavedConfig = applied;
          setFontStatus(applied, "applied to all apps");
          toast("Font settings applied to all apps.");
        }).catch(function (err) {
          var message = errorText(err);
          showFontError(message);
          $("font-status").textContent = message;
        });
      },
      "Apply to all"
    );
  });

  $("font-shortcuts-reset").addEventListener("click", function () {
    var svc = panelService();
    if (!svc) return;
    openConfirm(
      "Reset this app's font shortcuts to the defaults?",
      function () {
        cmd("reset_font_shortcuts", { id: svc.id }).then(function (state) {
          if (state) render(state);
          var reset = fontConfigFor(svc.id) || defaultFontConfig();
          fontSavedConfig = reset;
          setFontStatus(reset, "shortcuts reset");
          toast("Font shortcuts reset.");
        }).catch(function (err) {
          var message = errorText(err);
          showFontError(message);
          $("font-status").textContent = message;
        });
      },
      "Reset shortcuts"
    );
  });

  /* ---------------- Global listeners ---------------- */

  document.addEventListener("mousedown", function (e) {
    if (!contextMenu.classList.contains("hidden") && !contextMenu.contains(e.target)) {
      closeContextMenu();
    }
  });
  window.addEventListener("blur", closeContextMenu);
  window.addEventListener("resize", closeContextMenu);

  document.addEventListener("keydown", function (e) {
    // Escape closes an open overlay first; with nothing open it is still
    // available as a configured font shortcut.
    if (e.key === "Escape" && (anyModalOpen() || !contextMenu.classList.contains("hidden"))) {
      if (!contextMenu.classList.contains("hidden")) closeContextMenu();
      else closeAllModals();
      return;
    }
    handleShellFontShortcut(e);
  });

  /* ---------------- URL bar ---------------- */

  var urlBar = $("url-bar");
  var urlInput = $("url-input");
  var urlGo = $("url-go");
  var urlHome = $("url-home");
  var currentUrls = {};       // service id -> last reported url
  var urlEditing = false;

  function refreshUrlBar() {
    if (!currentState) return;
    var active = currentState.activeServiceId ? serviceById(currentState.activeServiceId) : null;
    // The assistant has no page to address — the bar is hidden while it is
    // the active service, exactly like the no-url-bar setting.
    var grokActive = !!(active && active.kind === "grokApi");
    var show = !!(currentState.settings && currentState.settings.showUrlBar) && !grokActive;
    document.body.classList.toggle("no-url-bar", !show);
    var activeId = currentState.activeServiceId;
    var enabled = !!activeId;
    urlInput.disabled = !enabled;
    urlGo.disabled = !enabled;
    urlHome.disabled = !enabled;
    if (!urlEditing) {
      urlInput.value = (activeId && currentUrls[activeId]) ||
        (activeId && (serviceById(activeId) || {}).url) || "";
    }
  }

  function serviceById(id) {
    if (!currentState) return null;
    for (var i = 0; i < (currentState.services || []).length; i++) {
      if (currentState.services[i].id === id) return currentState.services[i];
    }
    return null;
  }

  function submitUrl() {
    if (!currentState || !currentState.activeServiceId) return;
    var raw = urlInput.value.trim();
    if (raw && !/^https?:\/\//i.test(raw)) raw = "https://" + raw;
    var u;
    try { u = new URL(raw); } catch (e) { toast("Enter a valid URL."); return; }
    if (u.protocol !== "https:") { toast("URL must start with https://"); return; }
    cmd("navigate_url", { id: currentState.activeServiceId, url: u.href }).catch(function () {});
    urlInput.blur();
  }

  urlInput.addEventListener("focus", function () { urlEditing = true; });
  urlInput.addEventListener("blur", function () {
    urlEditing = false;
    refreshUrlBar();
  });
  urlInput.addEventListener("keydown", function (e) {
    if (e.key === "Enter") { e.preventDefault(); submitUrl(); }
    else if (e.key === "Escape") { urlInput.blur(); }
  });
  urlGo.addEventListener("click", submitUrl);

  /* Home: back to the active service's configured URL (same action as the
   * context menu's "Home"). */
  urlHome.addEventListener("click", function () {
    if (!currentState || !currentState.activeServiceId) return;
    cmd("navigate", { id: currentState.activeServiceId, action: "home" })
      .catch(function () {});
  });

  /* ---------------- Startup ---------------- */

  if (hasTauri) {
    tauri.event.listen("state-changed", function (e) { render(e.payload); })
      .catch(function () {});
    // Per-app fonts changed (e.g. from the in-page right-click menu or a
    // shortcut inside the service page): refresh the shell view.
    tauri.event.listen("font-changed", function (e) { handleFontChanged(e.payload); })
      .catch(function () {});
    // "Font Settings…" was chosen inside a service page.
    tauri.event.listen("open-font-settings", function (e) {
      if (e.payload && e.payload.id) openFontSettings(e.payload.id);
    }).catch(function () {});
    tauri.event.listen("url-changed", function (e) {
      if (e.payload && e.payload.id) {
        currentUrls[e.payload.id] = e.payload.url;
        if (!urlEditing && currentState && currentState.activeServiceId === e.payload.id) {
          urlInput.value = e.payload.url;
        }
      }
    }).catch(function () {});
    // Backend notices carry no command error channel (e.g. a failed
    // "open this link in your browser" hand-off from a service webview).
    tauri.event.listen("app-notice", function (e) {
      if (typeof e.payload === "string" && e.payload) toast(e.payload);
    }).catch(function () {});
    // The first agent API key is minted by the backend the moment Agent
    // Access is switched on — displayed here exactly once, never again.
    tauri.event.listen("agent-token-created", function (e) {
      if (typeof e.payload === "string" && e.payload) {
        agentTokenShown = e.payload;
        $("agent-token-note").classList.remove("hidden");
        renderAgentToken();
        toast("Agent API key created — copy it now, it is shown only once.");
      }
    }).catch(function () {});
    // Grok assistant streams arrive as events from the Rust worker thread.
    tauri.event.listen("grok-chunk", function (e) { grokHandleChunk(e.payload); })
      .catch(function () {});
    tauri.event.listen("grok-done", function (e) { grokHandleDone(e.payload); })
      .catch(function () {});
    tauri.event.listen("grok-error", function (e) { grokHandleError(e.payload); })
      .catch(function () {});
    // Brand manifest first so the initial render can resolve brand icons.
    loadBrandManifest()
      .then(function () { return cmd("get_state"); })
      .then(function (state) { render(state); })
      .catch(function () {});
  } else {
    // Plain-browser preview: render demo state, no backend calls.
    brandManifest = { grok: "#000000" };
    render(mockState);
    // Preview-only test hooks so the assistant surface can be exercised
    // (mockInvoke routes simulated streams through the same handlers).
    window.__grokTest = {
      chunk: grokHandleChunk,
      done: grokHandleDone,
      error: grokHandleError,
      state: grok
    };
  }
})();
