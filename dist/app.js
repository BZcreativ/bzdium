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
  var mockState = {
    services: [
      { id: "demo-1", name: "WhatsApp", url: "https://web.whatsapp.com", icon: "W", enabled: true, order: 0, hibernated: false, badgeCount: 3 },
      { id: "demo-2", name: "Telegram", url: "https://web.telegram.org", icon: "T", enabled: true, order: 1, hibernated: true, badgeCount: 0 },
      { id: "demo-3", name: "Discord", url: "https://discord.com/app", icon: "D", enabled: true, order: 2, hibernated: false, badgeCount: 12 }
    ],
    activeServiceId: "demo-1",
    settings: { hibernationMinutes: 30, minimizeToTray: true, startWithWindows: false, showUrlBar: true, darkUi: true, devMode: false },
    totalBadgeCount: 15,
    dataDir: "demo"
  };
  var mockRecipes = [
    { name: "WhatsApp", url: "https://web.whatsapp.com", icon: "W" },
    { name: "Telegram", url: "https://web.telegram.org", icon: "T" },
    { name: "Discord", url: "https://discord.com/app", icon: "D" },
    { name: "Slack", url: "https://app.slack.com", icon: "S" },
    { name: "Messenger", url: "https://www.messenger.com", icon: "M" },
    { name: "Gmail", url: "https://mail.google.com", icon: "G" }
  ];

  function mockInvoke(name, args) {
    if (name === "get_recipes") return Promise.resolve(mockRecipes);
    if (name === "get_state") return Promise.resolve(mockState);
    if (name === "export_config") return Promise.resolve(null);
    if (name === "import_config") return Promise.resolve(null);
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

    refreshUrlBar();
  }

  function buildServiceItem(service, activeServiceId) {
    var btn = document.createElement("button");
    btn.className = "svc-item";
    btn.dataset.id = service.id;
    btn.title = service.name;
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

  function addCtxItem(label, danger, onClick) {
    var item = document.createElement("button");
    item.className = "ctx-item" + (danger ? " danger" : "");
    item.textContent = label;
    item.setAttribute("role", "menuitem");
    item.addEventListener("click", function () {
      closeContextMenu();
      onClick();
    });
    contextMenu.appendChild(item);
  }

  function addCtxSep() {
    var sep = document.createElement("div");
    sep.className = "ctx-sep";
    contextMenu.appendChild(sep);
  }

  function openContextMenu(service, x, y) {
    contextMenu.textContent = "";

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
  function openConfirm(message, onConfirm) {
    $("confirm-text").textContent = message;
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
        $("add-name").value = recipe.name;
        $("add-url").value = recipe.url;
        $("add-icon").value = (recipe.icon || defaultIconFor(recipe.name)).toUpperCase();
        $("add-icon-id").value = recipe.iconId || "";
        addIconTouched = false;
      });
      grid.appendChild(cell);
    });
  }

  function openAddModal() {
    $("add-form").reset();
    $("add-error").classList.add("hidden");
    addIconTouched = false;
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
    var err = validateServiceForm(name, url);
    if (err) {
      $("add-error").textContent = err;
      $("add-error").classList.remove("hidden");
      return;
    }
    cmd("add_service", { name: name, url: url, icon: icon, iconId: iconId })
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
    $("edit-name").value = service.name;
    $("edit-url").value = service.url;
    $("edit-icon").value = service.icon || defaultIconFor(service.name);
    $("edit-enabled").checked = service.enabled !== false;
    $("edit-error").classList.add("hidden");
    refreshEditIconPreview();
    openModal("edit-modal");
    $("edit-name").focus();
  }

  $("edit-form").addEventListener("submit", function (e) {
    e.preventDefault();
    if (!editingId) return;
    var name = $("edit-name").value.trim();
    var url = $("edit-url").value.trim();
    var icon = $("edit-icon").value.trim() || defaultIconFor(name);
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
      enabled: $("edit-enabled").checked
    }).then(function () { closeModal("edit-modal"); editingId = null; })
      .catch(function () {});
  });

  /* ----- Settings ----- */

  function openSettingsModal() {
    var s = (currentState && currentState.settings) || {};
    $("set-hibernation").value = (typeof s.hibernationMinutes === "number") ? s.hibernationMinutes : 30;
    $("set-tray").checked = s.minimizeToTray !== false;
    $("set-autostart").checked = !!s.startWithWindows;
    $("set-urlbar").checked = s.showUrlBar !== false;
    $("set-dark").checked = s.darkUi !== false;
    $("set-devmode").checked = !!s.devMode;
    $("settings-error").classList.add("hidden");
    refreshSettingsFooter();
    openModal("settings-modal");
  }

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
    cmd("update_settings", {
      settings: {
        hibernationMinutes: minutes,
        minimizeToTray: $("set-tray").checked,
        startWithWindows: $("set-autostart").checked,
        showUrlBar: $("set-urlbar").checked,
        darkUi: $("set-dark").checked,
        devMode: $("set-devmode").checked
      }
    }).then(function () { closeModal("settings-modal"); })
      .catch(function () {});
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
      }
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
    if (e.key === "Escape") {
      if (!contextMenu.classList.contains("hidden")) closeContextMenu();
      else if (anyModalOpen()) closeAllModals();
    }
  });

  /* ---------------- URL bar ---------------- */

  var urlBar = $("url-bar");
  var urlInput = $("url-input");
  var urlGo = $("url-go");
  var currentUrls = {};       // service id -> last reported url
  var urlEditing = false;

  function refreshUrlBar() {
    if (!currentState) return;
    var show = !!(currentState.settings && currentState.settings.showUrlBar);
    document.body.classList.toggle("no-url-bar", !show);
    var activeId = currentState.activeServiceId;
    var enabled = !!activeId;
    urlInput.disabled = !enabled;
    urlGo.disabled = !enabled;
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

  /* ---------------- Startup ---------------- */

  if (hasTauri) {
    tauri.event.listen("state-changed", function (e) { render(e.payload); })
      .catch(function () {});
    tauri.event.listen("url-changed", function (e) {
      if (e.payload && e.payload.id) {
        currentUrls[e.payload.id] = e.payload.url;
        if (!urlEditing && currentState && currentState.activeServiceId === e.payload.id) {
          urlInput.value = e.payload.url;
        }
      }
    }).catch(function () {});
    // Brand manifest first so the initial render can resolve brand icons.
    loadBrandManifest()
      .then(function () { return cmd("get_state"); })
      .then(function (state) { render(state); })
      .catch(function () {});
  } else {
    // Plain-browser preview: render demo state, no backend calls.
    brandManifest = {};
    render(mockState);
  }
})();
