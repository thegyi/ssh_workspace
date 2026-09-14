const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const encoder = new TextEncoder();

const hostListEl = document.getElementById("host-list");
const tabsEl = document.getElementById("tabs");
const panesEl = document.getElementById("panes");
const emptyStateEl = document.getElementById("empty-state");

let hosts = [];
const tabs = []; // { host, kind:'term'|'ftp', term, fit, pane, tabEl, dot, sid, dead, unlisteners, ft }
let activeTab = null;
let broadcastMode = false; // send keystrokes from the focused terminal to all live ones
let dragTab = null; // session being drag-reordered in the tab bar

// Broadcast-input toggle pinned to the right end of the tab bar.
const bcBtn = document.createElement("button");
bcBtn.id = "broadcast-btn";
bcBtn.textContent = "⇶";
bcBtn.title = "Broadcast input to all terminals";
bcBtn.addEventListener("click", () => {
  broadcastMode = !broadcastMode;
  bcBtn.classList.toggle("on", broadcastMode);
  bcBtn.title = broadcastMode
    ? "Broadcasting input to all terminals — click to stop"
    : "Broadcast input to all terminals";
});
tabsEl.append(bcBtn);

/* ---------------- hosts ---------------- */

const hostSearch = document.getElementById("host-search");
// Groups start collapsed; expansion state persists across restarts.
const expandedGroups = new Set(
  JSON.parse(localStorage.getItem("sshws.groups") || "[]"),
);
hostSearch.addEventListener("input", renderHosts);

async function loadHosts() {
  hosts = await invoke("list_hosts");
  renderHosts();
}

function renderHosts() {
  hostListEl.innerHTML = "";
  const q = hostSearch.value.trim().toLowerCase();
  const visible = hosts.filter(
    (h) =>
      !q ||
      [h.name, h.host, h.username, h.group].some((s) =>
        (s || "").toLowerCase().includes(q),
      ),
  );
  const groups = new Map();
  for (const h of visible) {
    const g = h.group || "";
    if (!groups.has(g)) groups.set(g, []);
    groups.get(g).push(h);
  }
  for (const g of [...groups.keys()].sort((a, b) => a.localeCompare(b))) {
    if (g) {
      const open = expandedGroups.has(g);
      const head = document.createElement("li");
      head.className = "host-group";
      head.textContent = `${open ? "▾" : "▸"} ${g}`;
      head.addEventListener("click", () => {
        if (expandedGroups.has(g)) expandedGroups.delete(g);
        else expandedGroups.add(g);
        localStorage.setItem("sshws.groups", JSON.stringify([...expandedGroups]));
        renderHosts();
      });
      hostListEl.append(head);
      // Searching overrides the collapsed state so matches stay visible.
      if (!open && !q) continue;
    }
    for (const h of groups.get(g)) hostListEl.append(hostItem(h));
  }
}

function hostItem(h) {
  const li = document.createElement("li");
  li.className = "host-item";

  const info = document.createElement("div");
  info.className = "host-info";
  info.title = `${h.name} — ${h.username}@${h.host}:${h.port}`;
  const name = document.createElement("div");
  name.className = "host-name";
  name.textContent = h.name;
  const sub = document.createElement("div");
  sub.className = "host-sub";
  sub.textContent = `${h.username}@${h.host}:${h.port}`;
  info.append(name, sub);

  const actions = document.createElement("div");
  actions.className = "host-actions";
  actions.append(
    actionBtn("⇄", "File transfer (SFTP)", (e) => {
      e.stopPropagation();
      openFtp(h);
    }),
    actionBtn("✎", "Edit host", (e) => {
      e.stopPropagation();
      openHostModal(h);
    }),
    actionBtn("⧉", "Duplicate host", async (e) => {
      e.stopPropagation();
      try {
        await invoke("duplicate_host", { id: h.id });
        await loadHosts();
      } catch (err) {
        toast(String(err), true);
      }
    }),
    actionBtn("⟲", "Reset host key (known_hosts)", async (e) => {
      e.stopPropagation();
      try {
        const n = await invoke("reset_host_key", { id: h.id });
        toast(
          n > 0
            ? `Removed ${n} known_hosts entr${n === 1 ? "y" : "ies"} for ${h.host}`
            : `No stored key found for ${h.host}`,
        );
      } catch (err) {
        toast(String(err), true);
      }
    }),
    actionBtn(
      "✕",
      "Remove host",
      async (e) => {
        e.stopPropagation();
        if (!confirm(`Remove host "${h.name}"?`)) return;
        await invoke("delete_host", { id: h.id });
        tabs.filter((t) => t.host.id === h.id).forEach(closeTab);
        await loadHosts();
      },
      true,
    ),
  );

  li.append(info, actions);
  li.addEventListener("click", () => openSession(h));
  return li;
}

function actionBtn(label, title, onClick, danger = false) {
  const b = document.createElement("button");
  b.textContent = label;
  b.title = title;
  if (danger) b.className = "danger";
  b.addEventListener("click", onClick);
  return b;
}

/* ---------------- host modal ---------------- */

const hostModal = document.getElementById("host-modal");
const hostForm = document.getElementById("host-form");
const f = (id) => document.getElementById(id);
let editingId = null;
let authKind = "password";

document.getElementById("add-host-btn").addEventListener("click", () => openHostModal(null));
document.getElementById("host-cancel").addEventListener("click", () => (hostModal.hidden = true));

function setAuthKind(kind) {
  authKind = kind;
  document
    .querySelectorAll("#f-auth button")
    .forEach((b) => b.classList.toggle("active", b.dataset.value === kind));
  f("f-password-row").hidden = kind !== "password";
  f("f-key-row").hidden = kind !== "key";
}

document
  .querySelectorAll("#f-auth button")
  .forEach((b) => b.addEventListener("click", () => setAuthKind(b.dataset.value)));

// ---------- settings (theme + terminal font) ----------

const settings = {
  theme: localStorage.getItem("sshws.theme") || "dark",
  font:
    localStorage.getItem("sshws.font") ||
    '"Cascadia Mono", "JetBrains Mono", Menlo, Consolas, monospace',
  size: Number(localStorage.getItem("sshws.fontsize")) || 13,
};

function applySettings() {
  document.documentElement.dataset.theme = settings.theme;
  for (const t of tabs) {
    if (t.term) {
      t.term.options.fontFamily = settings.font;
      t.term.options.fontSize = settings.size;
      if (t === activeTab) t.fit?.fit();
    }
  }
}

const settingsModal = document.getElementById("settings-modal");
let fontList = null;
document.getElementById("settings-btn").addEventListener("click", async () => {
  f("s-theme").value = settings.theme;
  f("s-size").value = settings.size;
  settingsModal.hidden = false;
  if (fontList === null) {
    try {
      fontList = await invoke("list_fonts");
    } catch {
      fontList = [];
    }
  }
  const sel = f("s-font");
  sel.innerHTML = "";
  const names = fontList.includes(settings.font)
    ? fontList
    : [settings.font, ...fontList];
  for (const name of names) {
    const o = document.createElement("option");
    o.value = name;
    o.textContent = name;
    sel.append(o);
  }
  sel.value = settings.font;
});
document.getElementById("settings-cancel").addEventListener("click", () => {
  settingsModal.hidden = true;
});
document.getElementById("settings-form").addEventListener("submit", (e) => {
  e.preventDefault();
  settings.theme = f("s-theme").value;
  settings.font = f("s-font").value.trim() || "monospace";
  settings.size = Math.min(32, Math.max(8, Number(f("s-size").value) || 13));
  localStorage.setItem("sshws.theme", settings.theme);
  localStorage.setItem("sshws.font", settings.font);
  localStorage.setItem("sshws.fontsize", String(settings.size));
  applySettings();
  settingsModal.hidden = true;
});

applySettings();

// ---------- tunnels editor ----------

let formTunnels = []; // rows being edited in the host modal

const TUNNEL_KINDS = [
  ["local", "local -L"],
  ["remote", "remote -R"],
  ["dynamic", "socks5 -D"],
];

function renderTunnelRows() {
  const box = f("f-tunnels");
  box.innerHTML = "";
  for (const [i, t] of formTunnels.entries()) {
    const row = document.createElement("div");
    row.className = "tunnel-row";
    const kind = document.createElement("select");
    for (const [v, label] of TUNNEL_KINDS) {
      const o = document.createElement("option");
      o.value = v;
      o.textContent = label;
      kind.append(o);
    }
    kind.value = t.kind;
    const inp = (cls, val, ph, num) => {
      const el = document.createElement("input");
      el.className = cls;
      el.value = val;
      el.placeholder = ph;
      if (num) el.type = "number";
      return el;
    };
    const bind = inp("t-bind", t.bind, "bind", false);
    const lport = inp("t-lport", t.listen_port || "", "port", true);
    const arrow = document.createElement("span");
    arrow.textContent = "→";
    const thost = inp("t-thost", t.target_host, "target host", false);
    const tport = inp("t-tport", t.target_port || "", "port", true);
    const del = document.createElement("button");
    del.type = "button";
    del.className = "t-del";
    del.textContent = "✕";
    del.addEventListener("click", () => {
      readTunnelRows();
      formTunnels.splice(i, 1);
      renderTunnelRows();
    });
    const syncTarget = () => {
      const dyn = kind.value === "dynamic";
      thost.disabled = tport.disabled = dyn;
    };
    kind.addEventListener("change", syncTarget);
    syncTarget();
    row.append(kind, bind, lport, arrow, thost, tport, del);
    box.append(row);
  }
}

function readTunnelRows() {
  formTunnels = [...f("f-tunnels").querySelectorAll(".tunnel-row")].map((row) => ({
    kind: row.querySelector(".t-kind").value,
    bind: row.querySelector(".t-bind").value.trim() || "127.0.0.1",
    listen_port: Number(row.querySelector(".t-lport").value) || 0,
    target_host: row.querySelector(".t-thost").value.trim(),
    target_port: Number(row.querySelector(".t-tport").value) || 0,
  }));
}

f("f-tunnel-add").addEventListener("click", () => {
  readTunnelRows();
  formTunnels.push({
    kind: "local",
    bind: "127.0.0.1",
    listen_port: 0,
    target_host: "",
    target_port: 0,
  });
  renderTunnelRows();
});

function openHostModal(host) {
  editingId = host ? host.id : null;
  document.getElementById("host-modal-title").textContent = host ? "Edit host" : "Add host";
  f("f-name").value = host?.name ?? "";
  f("f-host").value = host?.host ?? "";
  f("f-port").value = host?.port ?? 22;
  f("f-user").value = host?.username ?? "";
  const kind = host?.auth?.kind ?? "password";
  setAuthKind(kind);
  f("f-password").value = kind === "password" ? host?.auth?.password ?? "" : "";
  f("f-key-path").value =
    kind === "key" ? host?.auth?.path ?? "~/.ssh/id_ed25519" : "~/.ssh/id_ed25519";
  f("f-passphrase").value = kind === "key" ? host?.auth?.passphrase ?? "" : "";
  f("f-x11").checked = !!host?.x11;
  f("f-group").value = host?.group ?? "";
  f("group-list").innerHTML = "";
  for (const g of [...new Set(hosts.map((h) => h.group).filter(Boolean))].sort()) {
    const o = document.createElement("option");
    o.value = g;
    f("group-list").append(o);
  }
  const jumpSel = f("f-jump");
  jumpSel.innerHTML = '<option value="">none</option>';
  for (const h of hosts) {
    if (h.id === host?.id) continue;
    const o = document.createElement("option");
    o.value = h.id;
    o.textContent = `${h.name} (${h.username}@${h.host})`;
    jumpSel.append(o);
  }
  jumpSel.value = host?.jump ?? "";
  formTunnels = (host?.tunnels ?? []).map((t) => ({ ...t }));
  renderTunnelRows();
  hostModal.hidden = false;
  f("f-name").focus();
}

hostForm.addEventListener("submit", async (e) => {
  e.preventDefault();
  readTunnelRows();
  const kind = authKind;
  let auth;
  if (kind === "password") {
    auth = { kind: "password", password: f("f-password").value || null };
  } else if (kind === "key") {
    auth = {
      kind: "key",
      path: f("f-key-path").value.trim(),
      passphrase: f("f-passphrase").value || null,
    };
  } else {
    auth = { kind: "agent" };
  }
  const host = {
    id: editingId ?? "",
    name: f("f-name").value.trim(),
    host: f("f-host").value.trim(),
    port: Number(f("f-port").value) || 22,
    username: f("f-user").value.trim(),
    auth,
    x11: f("f-x11").checked,
    group: f("f-group").value.trim(),
    jump: f("f-jump").value || null,
    tunnels: formTunnels
      .filter(
        (t) =>
          t.listen_port > 0 &&
          (t.kind === "dynamic" || (t.target_host && t.target_port > 0)),
      )
      .map((t) => ({
        ...t,
        target_host: t.kind === "dynamic" ? "" : t.target_host,
        target_port: t.kind === "dynamic" ? 0 : t.target_port,
      })),
    // Fields the form doesn't edit — keep what was loaded.
    bookmarks: hosts.find((h) => h.id === editingId)?.bookmarks ?? [],
  };
  try {
    await invoke("save_host", { host });
    hostModal.hidden = true;
    await loadHosts();
  } catch (err) {
    toast(String(err), true);
  }
});

/* ---------------- ssh config import ---------------- */

const importModal = document.getElementById("import-modal");
const importList = document.getElementById("import-list");

document.getElementById("import-btn").addEventListener("click", async () => {
  let entries;
  try {
    entries = await invoke("ssh_config_hosts");
  } catch (err) {
    toast(String(err), true);
    return;
  }
  importList.innerHTML = "";
  if (!entries.length) {
    const p = document.createElement("p");
    p.className = "hint";
    p.textContent = "No named Host blocks found in ~/.ssh/config.";
    importList.append(p);
  }
  const taken = new Set(hosts.map((h) => h.name));
  for (const e of entries) {
    const label = document.createElement("label");
    label.className = "import-row";
    const cb = document.createElement("input");
    cb.type = "checkbox";
    cb.checked = !taken.has(e.name);
    cb.dataset.entry = JSON.stringify(e);
    const text = document.createElement("span");
    text.textContent = `${e.name} — ${e.username}@${e.host}:${e.port}` +
      (e.identity_file ? ` (key ${e.identity_file})` : "");
    if (taken.has(e.name)) {
      cb.disabled = true;
      text.textContent += "  · already imported";
      label.classList.add("disabled");
    }
    label.append(cb, text);
    importList.append(label);
  }
  importModal.hidden = false;
});

document.getElementById("import-cancel").addEventListener("click", () => {
  importModal.hidden = true;
});

document.getElementById("import-ok").addEventListener("click", async () => {
  const picked = [...importList.querySelectorAll("input:checked")].map((cb) =>
    JSON.parse(cb.dataset.entry),
  );
  importModal.hidden = true;
  let n = 0;
  for (const e of picked) {
    try {
      await invoke("save_host", {
        host: {
          id: "",
          name: e.name,
          host: e.host,
          port: e.port,
          username: e.username,
          auth: e.identity_file
            ? { kind: "key", path: e.identity_file, passphrase: null }
            : { kind: "agent" },
          x11: false,
          group: "",
          jump: null,
          tunnels: [],
          bookmarks: [],
        },
      });
      n++;
    } catch (err) {
      toast(String(err), true);
    }
  }
  if (n) toast(`Imported ${n} host${n === 1 ? "" : "s"}`);
  await loadHosts();
});

/* ---------------- secret prompt ---------------- */

const secretModal = document.getElementById("secret-modal");
const secretInput = document.getElementById("secret-input");
let secretResolve = null;

function askSecret(title) {
  document.getElementById("secret-title").textContent = title;
  secretInput.value = "";
  secretModal.hidden = false;
  secretInput.focus();
  return new Promise((resolve) => (secretResolve = resolve));
}

document.getElementById("secret-form").addEventListener("submit", (e) => {
  e.preventDefault();
  secretModal.hidden = true;
  secretResolve?.(secretInput.value);
  secretResolve = null;
});

document.getElementById("secret-cancel").addEventListener("click", () => {
  secretModal.hidden = true;
  secretResolve?.(null);
  secretResolve = null;
});

// Keyboard-interactive (2FA) auth: during connect the backend may emit
// prompt batches; answer each sequentially and reply with the answers.
listen("ssh-auth-prompt", async (ev) => {
  const { id, username, instructions, prompts } = ev.payload;
  const answers = [];
  for (const [text, echo] of prompts) {
    const label =
      text || instructions || `Credentials for ${username}`;
    const a = echo ? await askText(label) : await askSecret(label);
    if (a === null) {
      answers.length = 0;
      break;
    }
    answers.push(a ?? "");
  }
  invoke("auth_prompt_reply", { id, answers }).catch(() => {});
});

// Files dragged from the OS file manager: Tauri intercepts native drops
// (dragDropEnabled) and emits tauri://drag-* events instead of HTML5
// drop events — the position is in physical pixels, so scale it back to
// CSS pixels for elementFromPoint. Drops upload into the remote pane's
// current directory; the local pane ignores them (files are local already).
let dragHoverEl = null;

function dragHalf(position) {
  const dpr = window.devicePixelRatio || 1;
  const el = document.elementFromPoint(position.x / dpr, position.y / dpr);
  return el?.closest?.(".ft-list")?.ftHalf ?? null;
}

listen("tauri://drag-over", (ev) => {
  const half = dragHalf(ev.payload.position);
  const el = half?.isRemote ? half.listEl : null;
  if (el !== dragHoverEl) {
    dragHoverEl?.classList.remove("drop-target");
    el?.classList.add("drop-target");
    dragHoverEl = el;
  }
});
listen("tauri://drag-leave", () => {
  dragHoverEl?.classList.remove("drop-target");
  dragHoverEl = null;
});
listen("tauri://drag-drop", async (ev) => {
  dragHoverEl?.classList.remove("drop-target");
  dragHoverEl = null;
  const half = dragHalf(ev.payload.position);
  if (!half?.isRemote || !ev.payload.paths?.length) return;
  const items = (
    await Promise.all(
      ev.payload.paths.map((path) =>
        invoke("local_stat", { path }).catch(() => null),
      ),
    )
  ).filter(Boolean);
  if (items.length) transferItems(half.sess, false, items, half.cwd);
});

/* ---------------- generic text prompt ---------------- */

const textModal = document.getElementById("text-modal");
const textInput = document.getElementById("text-input");
let textResolve = null;

function askText(title, prefill = "") {
  document.getElementById("text-title").textContent = title;
  textInput.value = prefill;
  textModal.hidden = false;
  textInput.focus();
  textInput.select();
  return new Promise((resolve) => (textResolve = resolve));
}

const infoModal = document.getElementById("info-modal");

function showInfo(title, text) {
  document.getElementById("info-title").textContent = title;
  document.getElementById("info-body").textContent = text;
  infoModal.hidden = false;
}

document.getElementById("info-close").addEventListener("click", () => {
  infoModal.hidden = true;
});

document.getElementById("text-form").addEventListener("submit", (e) => {
  e.preventDefault();
  textModal.hidden = true;
  textResolve?.(textInput.value.trim() || null);
  textResolve = null;
});

document.getElementById("text-cancel").addEventListener("click", () => {
  textModal.hidden = true;
  textResolve?.(null);
  textResolve = null;
});

/* ---------------- tabs ---------------- */

function createTab(host, kind) {
  const tabEl = document.createElement("div");
  tabEl.className = "tab";
  const dot = document.createElement("span");
  dot.className = "dot connecting";
  const label = document.createElement("span");
  label.textContent = kind === "ftp" ? `${host.name} ⇄` : host.name;
  const close = document.createElement("button");
  close.className = "close";
  close.textContent = "×";
  tabEl.append(dot, label, close);

  const pane = document.createElement("div");
  pane.className = "pane";
  panesEl.append(pane);
  tabsEl.insertBefore(tabEl, bcBtn);

  const sess = {
    host,
    kind,
    term: null,
    fit: null,
    pane,
    tabEl,
    dot,
    sid: null,
    dead: false,
    unlisteners: [],
    ft: null,
  };
  tabs.push(sess);

  close.addEventListener("click", (e) => {
    e.stopPropagation();
    closeTab(sess);
  });
  tabEl.addEventListener("click", () => activateTab(sess));

  // Drag a tab onto another tab to reorder.
  tabEl.draggable = true;
  tabEl.addEventListener("dragstart", (e) => {
    dragTab = sess;
    e.dataTransfer.setData("application/x-sshws-tab", "");
    e.dataTransfer.effectAllowed = "move";
  });
  tabEl.addEventListener("dragover", (e) => {
    if (e.dataTransfer.types.includes("application/x-sshws-tab") && dragTab !== sess) {
      e.preventDefault();
      e.dataTransfer.dropEffect = "move";
      tabEl.classList.add("drop-target");
    }
  });
  tabEl.addEventListener("dragleave", () => tabEl.classList.remove("drop-target"));
  tabEl.addEventListener("drop", (e) => {
    e.preventDefault();
    tabEl.classList.remove("drop-target");
    if (!dragTab || dragTab === sess) return;
    tabs.splice(tabs.indexOf(dragTab), 1);
    tabs.splice(tabs.indexOf(sess), 0, dragTab);
    for (const t of tabs) tabsEl.append(t.tabEl);
    tabsEl.append(bcBtn);
  });
  tabEl.addEventListener("dragend", () => {
    dragTab = null;
    for (const t of tabs) t.tabEl.classList.remove("drop-target");
  });
  return sess;
}

function activateTab(sess) {
  activeTab = sess;
  for (const t of tabs) {
    t.tabEl.classList.toggle("active", t === sess);
    t.pane.classList.toggle("active", t === sess);
  }
  emptyStateEl.hidden = tabs.length > 0;
  requestAnimationFrame(() => {
    sess.fit?.fit();
    sess.term?.focus();
  });
}

const CLOSE_CMD = { term: "ssh_close", ftp: "sftp_close", serial: "serial_close" };

function closeTab(sess) {
  if (sess.sid && !sess.dead) {
    invoke(CLOSE_CMD[sess.kind] ?? "ssh_close", { sessionId: sess.sid }).catch(() => {});
  }
  sess.unlisteners.forEach((u) => u());
  sess.ro?.disconnect();
  sess.term?.dispose();
  sess.tabEl.remove();
  sess.pane.remove();
  const i = tabs.indexOf(sess);
  tabs.splice(i, 1);
  if (activeTab === sess) {
    const next = tabs[Math.min(i, tabs.length - 1)];
    activeTab = null;
    if (next) activateTab(next);
  }
  emptyStateEl.hidden = tabs.length > 0;
}

function markDead(sess, msg) {
  sess.dead = true;
  sess.dot.className = "dot dead";
  if (msg && sess.term) sess.term.write(`\r\n\x1b[90m[${msg}]\x1b[0m\r\n`);
  if (sess.kind === "term" && !sess.reconnectEl) {
    const b = document.createElement("button");
    b.className = "reconnect-btn";
    b.textContent = "Reconnect";
    b.addEventListener("click", () => reconnectSsh(sess));
    sess.pane.append(b);
    sess.reconnectEl = b;
  }
}

window.addEventListener("resize", () => {
  activeTab?.fit?.fit();
});

// Desktop app: block the webview context menu (its "Reload" item would wipe
// the UI state while backend sessions stay alive) and reload hotkeys.
document.addEventListener("contextmenu", (e) => e.preventDefault());
window.addEventListener("keydown", (e) => {
  if (e.key === "F5" || (e.ctrlKey && (e.key === "r" || e.key === "R"))) {
    e.preventDefault();
    return;
  }
  if (e.ctrlKey && (e.key === "PageDown" || e.key === "PageUp")) {
    e.preventDefault();
    cycleTab(e.key === "PageDown" ? 1 : -1);
  }
});

function cycleTab(dir) {
  if (tabs.length < 2) return;
  const i = tabs.indexOf(activeTab);
  activateTab(tabs[(i + dir + tabs.length) % tabs.length]);
}

/* ---------------- terminals ---------------- */

async function openSession(host) {
  const secret = await resolveSecret(host);
  if (secret === undefined) return;
  const jumpSecret = await resolveJumpSecret(host);
  if (jumpSecret === undefined) return;
  const sess = createTab(host, "term");
  sess.jumpSecret = jumpSecret;
  activateTab(sess);
  buildTerminal(sess);
  attachSsh(sess, secret);
}

/// (Re)connect a terminal session onto an existing tab.
async function attachSsh(sess, secret) {
  try {
    const res = await invoke("connect_host", {
      hostId: sess.host.id,
      secret,
      jumpSecret: sess.jumpSecret ?? null,
      cols: sess.term.cols,
      rows: sess.term.rows,
    });
    sess.sid = res.session_id;
    invoke("ssh_resize", {
      sessionId: sess.sid,
      cols: sess.term.cols,
      rows: sess.term.rows,
    }).catch(() => {});
    if (res.notice) sess.term.write(`\x1b[33m${res.notice}\x1b[0m\r\n`);
    sess.unlisteners = [
      await listen(`ssh-data-${sess.sid}`, (ev) => sess.term.write(new Uint8Array(ev.payload))),
      await listen(`ssh-exit-${sess.sid}`, () => markDead(sess, "connection closed")),
    ];
    sess.dot.className = "dot live";
    sess.reconnectEl?.remove();
    sess.reconnectEl = null;
    sess.term.focus();
  } catch (err) {
    sess.term.write(`\x1b[31m${String(err)}\x1b[0m\r\n`);
    markDead(sess, null);
  }
}

async function reconnectSsh(sess) {
  const secret = await resolveSecret(sess.host);
  if (secret === undefined) return;
  sess.reconnectEl?.remove();
  sess.reconnectEl = null;
  sess.dead = false;
  sess.dot.className = "dot conn";
  sess.term.write("\r\n\x1b[33m[reconnecting…]\x1b[0m\r\n");
  sess.unlisteners.forEach((u) => u());
  sess.unlisteners = [];
  attachSsh(sess, secret);
}

function buildTerminal(sess) {
  const term = new Terminal({
    fontFamily: settings.font,
    fontSize: settings.size,
    cursorBlink: true,
    scrollback: 5000,
    theme: {
      background: "#0d1117",
      foreground: "#d7dae0",
      cursor: "#d7dae0",
      selectionBackground: "#264f78",
    },
  });
  const fit = new FitAddon.FitAddon();
  term.loadAddon(fit);
  if (window.SearchAddon) term.loadAddon((sess.search = new SearchAddon.SearchAddon()));
  term.open(sess.pane);
  fit.fit();

  // OSC 7 cwd reports (injected by the backend PROMPT_COMMAND hook) let open
  // SFTP panes for this host follow the shell's current directory.
  term.parser?.registerOscHandler(7, (data) => {
    onCwdReport(sess, data);
    return true;
  });
  // OSC 133 command lifecycle (injected by the backend shell hook): C marks a
  // command start, D;<exit-code> marks completion — notify on long commands.
  term.parser?.registerOscHandler(133, (data) => {
    if (data === "C") sess.cmdStart = Date.now();
    else if (data.startsWith("D")) {
      const code = data.split(";")[1] ?? "?";
      if (sess.cmdStart && Date.now() - sess.cmdStart > 5000) notifyCmdDone(sess, code);
      sess.cmdStart = null;
    }
    return true;
  });
  sess.term = term;
  sess.fit = fit;
  sess.ro = new ResizeObserver(() => {
    if (sess.pane.offsetParent !== null) fit.fit();
  });
  sess.ro.observe(sess.pane);

  term.onData((d) => {
    const data = Array.from(encoder.encode(d));
    if (broadcastMode) {
      for (const t of tabs) {
        if (t.kind === "term" && t.sid && !t.dead) {
          invoke("ssh_write", { sessionId: t.sid, data }).catch(() => {});
        }
      }
    } else if (sess.sid && !sess.dead) {
      invoke("ssh_write", { sessionId: sess.sid, data }).catch(() => {});
    }
  });
  term.onResize(({ cols, rows }) => {
    if (sess.sid && !sess.dead) {
      invoke("ssh_resize", { sessionId: sess.sid, cols, rows }).catch(() => {});
    }
  });

  // Right-click menu + Ctrl+Shift+C/V clipboard shortcuts.
  sess.pane.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    e.stopPropagation();
    const sel = term.getSelection();
    showCtxMenu(e.clientX, e.clientY, [
      { label: "Copy", disabled: !sel, action: () => copyText(sel) },
      { label: "Copy all output", action: () => copyText(getAllText(term)) },
      "-",
      { label: "Select all", action: () => term.selectAll() },
      { label: "Paste", action: () => pasteInto(sess) },
      "-",
      { label: "Find…", action: () => openFindBar(sess) },
      sess.logging
        ? { label: "Stop recording", action: () => toggleLog(sess) }
        : { label: "Log output to file", action: () => toggleLog(sess, "raw") },
      sess.logging
        ? null
        : {
            label: "Record session (.cast)",
            action: () => toggleLog(sess, "cast"),
          },
      "-",
      { label: "Clear", action: () => term.clear() },
    ].filter(Boolean));
  });

  term.attachCustomKeyEventHandler((e) => {
    if (e.type !== "keydown" || !e.ctrlKey) return true;
    if (e.key === "PageDown" || e.key === "PageUp") return false;
    if (!e.shiftKey) return true;
    const k = e.key.toLowerCase();
    if (k === "c") {
      const sel = term.getSelection();
      if (sel) copyText(sel);
      return false;
    }
    if (k === "v") {
      pasteInto(sess);
      return false;
    }
    if (k === "f") {
      openFindBar(sess);
      return false;
    }
    return true;
  });
}

// ---------- terminal search / logging / notifications ----------

function openFindBar(sess) {
  if (!sess.search) return;
  sess.findEl?.remove();
  const bar = document.createElement("div");
  bar.className = "find-bar";
  const input = document.createElement("input");
  input.placeholder = "Find in scrollback…";
  const mk = (label, title, fn) => {
    const b = document.createElement("button");
    b.textContent = label;
    b.title = title;
    b.addEventListener("click", fn);
    return b;
  };
  const close = () => {
    bar.remove();
    sess.findEl = null;
    sess.term.focus();
  };
  input.addEventListener("input", () => {
    if (input.value) sess.search.findNext(input.value);
  });
  input.addEventListener("keydown", (e) => {
    e.stopPropagation();
    if (e.key === "Enter") {
      if (!input.value) return;
      (e.shiftKey ? sess.search.findPrevious : sess.search.findNext).call(
        sess.search,
        input.value,
      );
    } else if (e.key === "Escape") close();
  });
  bar.append(
    input,
    mk("↑", "Previous match (Shift+Enter)", () => input.value && sess.search.findPrevious(input.value)),
    mk("↓", "Next match (Enter)", () => input.value && sess.search.findNext(input.value)),
    mk("✕", "Close (Esc)", close),
  );
  sess.pane.append(bar);
  sess.findEl = bar;
  input.focus();
}

async function toggleLog(sess, format) {
  if (!sess.sid || sess.dead) return;
  if (sess.logging) {
    await invoke("ssh_set_log", {
      sessionId: sess.sid,
      enable: false,
      format: "raw",
      cols: 0,
      rows: 0,
    }).catch(() => {});
    sess.logging = false;
    sess.term.write("\r\n\x1b[90m[recording stopped]\x1b[0m\r\n");
    return;
  }
  const path = await invoke("ssh_set_log", {
    sessionId: sess.sid,
    enable: true,
    format,
    cols: sess.term.cols,
    rows: sess.term.rows,
  }).catch(() => null);
  if (path) {
    sess.logging = true;
    sess.term.write(`\r\n\x1b[90m[recording to ${path}]\x1b[0m\r\n`);
  }
}

function notifyCmdDone(sess, code) {
  const msg = `${sess.host.name || sess.host.host}: command finished (exit ${code})`;
  if (document.hidden && window.Notification) {
    if (Notification.permission === "granted") {
      new Notification("SSH Workspace", { body: msg });
    } else if (Notification.permission === "default") {
      Notification.requestPermission().then((p) => {
        if (p === "granted") new Notification("SSH Workspace", { body: msg });
      });
    }
  } else if (activeTab !== sess) {
    toast(msg);
  }
}

async function copyText(text) {
  if (!text) return;
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // Fallback for webviews without async clipboard permission.
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.cssText = "position:fixed;opacity:0";
    document.body.append(ta);
    ta.select();
    document.execCommand("copy");
    ta.remove();
  }
}

function getAllText(term) {
  const buf = term.buffer.active;
  const lines = [];
  for (let i = 0; i < buf.length; i++) {
    lines.push(buf.getLine(i)?.translateToString(true) ?? "");
  }
  while (lines.length && lines[lines.length - 1] === "") lines.pop();
  return lines.join("\n");
}

async function pasteInto(sess) {
  try {
    const text = await navigator.clipboard.readText();
    if (text && sess.sid && !sess.dead) {
      invoke(sess.kind === "serial" ? "serial_write" : "ssh_write", {
        sessionId: sess.sid,
        data: Array.from(encoder.encode(text)),
      }).catch(() => {});
    }
  } catch (e) {
    toast("Clipboard read failed", true);
  }
}

/** Returns the secret to use, or undefined if the user cancelled the prompt. */
async function resolveSecret(host) {
  if (host.auth.kind === "password" && !host.auth.password) {
    const s = await askSecret(`Password for ${host.username}@${host.host}`);
    if (s === null) return undefined;
    return s;
  }
  return null;
}

/** Secret for the host's jump/bastion, if one is configured. */
async function resolveJumpSecret(host) {
  const jh = host.jump ? hosts.find((h) => h.id === host.jump) : null;
  if (!jh) return null;
  return resolveSecret(jh);
}

/* ---------------- serial console ---------------- */

const serialModal = document.getElementById("serial-modal");
const serialPort = document.getElementById("p-port");

document.getElementById("serial-btn").addEventListener("click", () => {
  serialModal.hidden = false;
  refreshSerialPorts();
});
document.getElementById("serial-cancel").addEventListener("click", () => {
  serialModal.hidden = true;
});
document.getElementById("p-refresh").addEventListener("click", refreshSerialPorts);

async function refreshSerialPorts() {
  let ports = [];
  try {
    ports = await invoke("serial_ports");
  } catch (e) {
    toast(String(e), true);
  }
  const prev = serialPort.value;
  serialPort.innerHTML = "";
  for (const p of ports) {
    const o = document.createElement("option");
    o.value = p.name;
    o.textContent = p.description ? `${p.name} — ${p.description}` : p.name;
    serialPort.append(o);
  }
  if (!ports.length) {
    const o = document.createElement("option");
    o.textContent = "(no serial ports found)";
    o.disabled = true;
    serialPort.append(o);
  } else if ([...serialPort.options].some((o) => o.value === prev)) {
    serialPort.value = prev;
  }
}

document.getElementById("serial-form").addEventListener("submit", (e) => {
  e.preventDefault();
  const port = serialPort.value;
  if (!port) {
    toast("No serial port selected", true);
    return;
  }
  serialModal.hidden = true;
  openSerial(port, {
    baud: +document.getElementById("p-baud").value,
    dataBits: +document.getElementById("p-databits").value,
    parity: document.getElementById("p-parity").value,
    stopBits: +document.getElementById("p-stopbits").value,
    flow: document.getElementById("p-flow").value,
    eol: document.getElementById("p-eol").value,
    echo: document.getElementById("p-echo").checked,
  });
});

async function openSerial(port, cfg) {
  const sess = createTab({ id: `serial:${port}`, name: port }, "serial");
  sess.serialCfg = cfg;
  activateTab(sess);
  buildSerialTerm(sess);
  try {
    sess.sid = await invoke("serial_connect", {
      port,
      baud: cfg.baud,
      dataBits: cfg.dataBits,
      parity: cfg.parity,
      stopBits: cfg.stopBits,
      flow: cfg.flow,
    });
    sess.dot.className = "dot live";
  } catch (err) {
    sess.term.write(`\x1b[31m${String(err)}\x1b[0m\r\n`);
    markDead(sess, null);
    return;
  }
  sess.unlisteners = [
    await listen(`serial-data-${sess.sid}`, (ev) =>
      sess.term.write(new Uint8Array(ev.payload)),
    ),
    await listen(`serial-exit-${sess.sid}`, (ev) =>
      markDead(sess, ev.payload ? `port closed: ${ev.payload}` : "port closed"),
    ),
  ];
}

function buildSerialTerm(sess) {
  const term = new Terminal({
    fontFamily: settings.font,
    fontSize: settings.size,
    cursorBlink: true,
    scrollback: 5000,
    theme: {
      background: "#0d1117",
      foreground: "#d7dae0",
      cursor: "#d7dae0",
      selectionBackground: "#264f78",
    },
  });
  const fit = new FitAddon.FitAddon();
  term.loadAddon(fit);
  if (window.SearchAddon) term.loadAddon((sess.search = new SearchAddon.SearchAddon()));
  term.open(sess.pane);
  fit.fit();
  sess.term = term;
  sess.fit = fit;
  sess.ro = new ResizeObserver(() => {
    if (sess.pane.offsetParent !== null) fit.fit();
  });
  sess.ro.observe(sess.pane);

  term.onData((d) => {
    const cfg = sess.serialCfg ?? {};
    if (cfg.echo) term.write(d);
    if (!sess.sid || sess.dead) return;
    // xterm emits "\r" for Enter; map it to the configured line ending.
    let out = d;
    if (d === "\r") {
      if (cfg.eol === "lf") out = "\n";
      else if (cfg.eol === "crlf") out = "\r\n";
    }
    invoke("serial_write", {
      sessionId: sess.sid,
      data: Array.from(encoder.encode(out)),
    }).catch(() => {});
  });

  sess.pane.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    e.stopPropagation();
    const sel = term.getSelection();
    showCtxMenu(e.clientX, e.clientY, [
      { label: "Copy", disabled: !sel, action: () => copyText(sel) },
      { label: "Copy all output", action: () => copyText(getAllText(term)) },
      "-",
      { label: "Select all", action: () => term.selectAll() },
      { label: "Paste", action: () => pasteInto(sess) },
      "-",
      { label: "Find…", action: () => openFindBar(sess) },
      {
        label: "Send BREAK",
        action: () =>
          sess.sid && invoke("serial_break", { sessionId: sess.sid }).catch(() => {}),
      },
      "-",
      { label: "Clear", action: () => term.clear() },
    ]);
  });

  term.attachCustomKeyEventHandler((e) => {
    if (e.type !== "keydown" || !e.ctrlKey) return true;
    if (e.key === "PageDown" || e.key === "PageUp") return false;
    if (!e.shiftKey) return true;
    const k = e.key.toLowerCase();
    if (k === "c") {
      const sel = term.getSelection();
      if (sel) copyText(sel);
      return false;
    }
    if (k === "v") {
      pasteInto(sess);
      return false;
    }
    if (k === "f") {
      openFindBar(sess);
      return false;
    }
    return true;
  });
}

/* ---------------- sftp file transfer ---------------- */

async function openFtp(host) {
  const secret = await resolveSecret(host);
  if (secret === undefined) return;
  const jumpSecret = await resolveJumpSecret(host);
  if (jumpSecret === undefined) return;
  const sess = createTab(host, "ftp");
  activateTab(sess);
  let res;
  try {
    res = await invoke("sftp_open", { hostId: host.id, secret, jumpSecret });
  } catch (err) {
    const d = document.createElement("div");
    d.className = "ft-error";
    d.textContent = String(err);
    sess.pane.append(d);
    markDead(sess, null);
    return;
  }
  sess.sid = res.session_id;
  sess.dot.className = "dot live";
  buildFtpUI(sess, res.home);
  sess.unlisteners = [
    await listen(`sftp-progress-${sess.sid}`, (e) => onXferProgress(sess, e.payload)),
    await listen(`sftp-done-${sess.sid}`, (e) => onXferDone(sess, e.payload)),
    await listen(`sftp-edit-synced-${sess.sid}`, (e) =>
      toast(`Saved → uploaded ${baseName(e.payload)}`),
    ),
  ];
}

function buildFtpUI(sess, remoteHome) {
  const wrap = document.createElement("div");
  wrap.className = "ftp";
  const body = document.createElement("div");
  body.className = "ftp-body";
  const remote = ftHalf(sess, "REMOTE", true);
  const local = ftHalf(sess, "LOCAL", false);
  body.append(local.el, remote.el);
  const status = document.createElement("div");
  status.className = "ft-xfers";
  wrap.append(body, status);
  sess.pane.append(wrap);
  sess.ft = { remote, local, status, seq: 0, ops: new Map() };
  remote.refresh(remoteHome);
  local.refresh("");
}

function ftHalf(sess, label, isRemote) {
  const el = document.createElement("div");
  el.className = "ft-half";

  const bar = document.createElement("div");
  bar.className = "ft-bar";
  const tag = document.createElement("span");
  tag.className = "ft-tag";
  tag.textContent = label;
  const up = document.createElement("button");
  up.className = "ft-btn";
  up.textContent = "↑";
  up.title = "Parent directory";
  const hid = document.createElement("button");
  hid.className = "ft-btn";
  hid.textContent = ".*";
  hid.title = "Show hidden files";
  const path = document.createElement("input");
  path.className = "ft-path";
  path.spellcheck = false;
  const mk = document.createElement("button");
  mk.className = "ft-btn";
  mk.textContent = "+";
  mk.title = "New directory";
  const reload = document.createElement("button");
  reload.className = "ft-btn";
  reload.textContent = "⟳";
  reload.title = "Refresh";
  bar.append(tag, up, hid, path, mk, reload);

  // Remote pane: per-host path bookmarks (★ toggles cwd, ▾ lists them).
  let starRefresh = null;
  if (isRemote) {
    const star = document.createElement("button");
    star.className = "ft-btn";
    star.title = "Bookmark this directory";
    const bmBtn = document.createElement("button");
    bmBtn.className = "ft-btn";
    bmBtn.textContent = "▾";
    bmBtn.title = "Bookmarks";
    bar.append(star, bmBtn);

    const bms = () => (sess.host.bookmarks ??= []);
    const refreshStar = () => {
      star.textContent = bms().includes(half.cwd) ? "★" : "☆";
    };
    star.addEventListener("click", async () => {
      const i = bms().indexOf(half.cwd);
      if (i >= 0) bms().splice(i, 1);
      else bms().push(half.cwd);
      try {
        sess.host = await invoke("save_host", { host: sess.host });
      } catch (e) {
        toast(String(e), true);
      }
      refreshStar();
    });
    bmBtn.addEventListener("click", (e) => {
      const items = bms().map((p) => ({ label: p, action: () => half.refresh(p) }));
      if (!items.length) items.push({ label: "(no bookmarks)", disabled: true, action: () => {} });
      showCtxMenu(e.clientX, e.clientY, items);
    });
    starRefresh = refreshStar;
  }

  const list = document.createElement("div");
  list.className = "ft-list";
  el.append(bar, list);

  const half = {
    el,
    sess,
    listEl: list,
    isRemote,
    cwd: "/",
    selected: new Set(),
    byPath: new Map(),
    entries: [],
    showHidden: false,
    anchor: null,
    syncStar: starRefresh,
  };
  // Lets the global tauri://drag-* handlers map a DOM hit to this pane.
  list.ftHalf = half;

  half.refresh = async (p) => {
    try {
      const res = isRemote
        ? await invoke("sftp_list", { sessionId: sess.sid, path: p ?? half.cwd })
        : await invoke("local_list", { path: p ?? half.cwd });
      half.cwd = res.path;
      path.value = res.path;
      half.entries = res.entries;
      renderEntries(half);
      half.syncStar?.();
    } catch (e) {
      toast(String(e), true);
    }
  };

  hid.addEventListener("click", () => {
    half.showHidden = !half.showHidden;
    hid.classList.toggle("on", half.showHidden);
    renderEntries(half);
  });

  mk.addEventListener("click", async () => {
    const name = await askText(`New directory in ${half.cwd}`);
    if (!name) return;
    try {
      if (isRemote) {
        await invoke("sftp_mkdir", { sessionId: sess.sid, path: joinPath(half.cwd, name) });
      } else {
        await invoke("local_mkdir", { path: joinPath(half.cwd, name) });
      }
      half.refresh(half.cwd);
    } catch (e) {
      toast(String(e), true);
    }
  });

  up.addEventListener("click", () => half.refresh(parentPath(half.cwd)));
  reload.addEventListener("click", () => half.refresh(half.cwd));
  path.addEventListener("keydown", (e) => {
    if (e.key === "Enter") half.refresh(path.value.trim());
  });

  list.addEventListener("click", (e) => {
    if (e.target === list) {
      half.selected.clear();
      half.anchor = null;
      syncSel(half);
    }
  });
  // Pane-level menu on empty space (rows stopPropagation on their own menu).
  list.addEventListener("contextmenu", (e) => {
    if (e.target !== list) return;
    e.preventDefault();
    e.stopPropagation();
    showCtxMenu(e.clientX, e.clientY, [
      { label: "New directory", action: () => mk.click() },
      {
        label: half.showHidden ? "Hide hidden files" : "Show hidden files",
        action: () => hid.click(),
      },
      "-",
      { label: "Refresh", action: () => half.refresh(half.cwd) },
    ]);
  });
  list.addEventListener("dragover", (e) => {
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
    list.classList.add("drop-target");
  });
  list.addEventListener("dragleave", () => list.classList.remove("drop-target"));
  list.addEventListener("drop", (e) => {
    e.preventDefault();
    list.classList.remove("drop-target");
    const raw = e.dataTransfer.getData("application/x-sshws");
    if (!raw) return;
    const data = JSON.parse(raw);
    if (data.isRemote === isRemote) return; // same side: ignore
    transferItems(sess, data.isRemote, data.items, half.cwd);
  });

  return half;
}

function renderEntries(half) {
  half.selected.clear();
  half.anchor = null;
  const entries = half.entries.filter((e) => half.showHidden || !e.name.startsWith("."));
  half.byPath = new Map(entries.map((e) => [e.path, e]));
  half.listEl.innerHTML = "";
  if (half.cwd !== "/") {
    half.listEl.append(entryRow(half, { name: "..", is_dir: true, path: parentPath(half.cwd) }));
  }
  for (const ent of entries) half.listEl.append(entryRow(half, ent));
}

function entryRow(half, ent) {
  const isParent = ent.name === "..";
  const row = document.createElement("div");
  row.className = "ft-row" + (ent.is_dir ? " dir" : "");
  row.dataset.path = ent.path;

  const ico = document.createElement("span");
  ico.className = "ft-ico " + (ent.is_dir ? "dir" : "file");
  const name = document.createElement("span");
  name.className = "ft-name";
  name.textContent = ent.name;
  const size = document.createElement("span");
  size.className = "ft-size";
  size.textContent = ent.is_dir ? "" : fmtBytes(ent.size);
  const date = document.createElement("span");
  date.className = "ft-date";
  date.textContent = isParent || !ent.mtime ? "" : fmtDate(ent.mtime);
  row.append(ico, name, size, date);

  if (!isParent) {
    row.draggable = true;
    row.addEventListener("dragstart", (e) => {
      if (!half.selected.has(ent.path)) {
        half.selected.clear();
        half.selected.add(ent.path);
        syncSel(half);
      }
      const items = [...half.selected].map((p) => half.byPath.get(p)).filter(Boolean);
      e.dataTransfer.setData(
        "application/x-sshws",
        JSON.stringify({ isRemote: half.isRemote, items }),
      );
      e.dataTransfer.effectAllowed = "copy";
    });
    row.addEventListener("mousedown", (e) => {
      if (e.shiftKey) e.preventDefault();
    });
    row.addEventListener("click", (e) => {
      const paths = [...half.byPath.keys()];
      if (e.shiftKey && half.anchor != null) {
        const a = paths.indexOf(half.anchor);
        const b = paths.indexOf(ent.path);
        if (a !== -1 && b !== -1) {
          half.selected.clear();
          const [lo, hi] = a < b ? [a, b] : [b, a];
          for (let i = lo; i <= hi; i++) half.selected.add(paths[i]);
          syncSel(half);
          return;
        }
      }
      if (e.ctrlKey || e.metaKey) {
        if (half.selected.has(ent.path)) half.selected.delete(ent.path);
        else half.selected.add(ent.path);
      } else {
        half.selected.clear();
        half.selected.add(ent.path);
      }
      half.anchor = ent.path;
      syncSel(half);
    });
    row.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
      if (!half.selected.has(ent.path)) {
        half.selected.clear();
        half.selected.add(ent.path);
        half.anchor = ent.path;
        syncSel(half);
      }
      showCtxMenu(e.clientX, e.clientY, fileMenuItems(half.sess, half));
    });
  }
  row.addEventListener("dblclick", () => {
    if (ent.is_dir) half.refresh(ent.path);
  });
  return row;
}

function syncSel(half) {
  for (const row of half.listEl.children) {
    row.classList.toggle("sel", half.selected.has(row.dataset.path));
  }
}

/* ---------------- file context menu ---------------- */

let ctxMenu = null;

function closeCtxMenu() {
  ctxMenu?.remove();
  ctxMenu = null;
}

/** items: array of { label, action, danger?, disabled? } or "-" for separator. */
function showCtxMenu(x, y, items) {
  closeCtxMenu();
  const m = document.createElement("div");
  m.className = "ctx-menu";
  for (const it of items) {
    if (it === "-") {
      const s = document.createElement("div");
      s.className = "ctx-sep";
      m.append(s);
      continue;
    }
    const b = document.createElement("button");
    b.className = "ctx-item" + (it.danger ? " danger" : "");
    b.textContent = it.label;
    b.disabled = !!it.disabled;
    b.addEventListener("click", () => {
      closeCtxMenu();
      it.action();
    });
    m.append(b);
  }
  document.body.append(m);
  const r = m.getBoundingClientRect();
  m.style.left = Math.min(x, innerWidth - r.width - 4) + "px";
  m.style.top = Math.min(y, innerHeight - r.height - 4) + "px";
  ctxMenu = m;
}

document.addEventListener("click", closeCtxMenu);
document.addEventListener("scroll", closeCtxMenu, true);
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") closeCtxMenu();
});

/** Menu items for a file row — add future actions here. */
function fileMenuItems(sess, half) {
  const targets = [...half.selected].map((p) => half.byPath.get(p)).filter(Boolean);
  const n = targets.length;
  const items = [];

  items.push({
    label: n > 1 ? `${half.isRemote ? "Download" : "Upload"} ${n} items` : half.isRemote ? "Download" : "Upload",
    action: () => {
      const dest = half.isRemote ? sess.ft.local.cwd : sess.ft.remote.cwd;
      transferItems(sess, half.isRemote, targets, dest);
    },
  });
  if (n === 1 && !targets[0].is_dir) {
    items.push({
      label: half.isRemote ? "Edit" : "Open",
      action: () => editEntry(sess, half, targets[0]),
    });
  }
  items.push({
    label: "Rename…",
    disabled: n !== 1,
    action: () => renameEntry(sess, half, targets[0]),
  });
  items.push({
    label: "Permissions…",
    disabled: n !== 1,
    action: () => chmodEntry(sess, half, targets[0]),
  });
  items.push({
    label: "Copy path",
    disabled: n !== 1,
    action: () => copyText(targets[0].path),
  });
  if (!half.isRemote) {
    items.push({
      label: "Open containing folder",
      disabled: n !== 1,
      action: () =>
        invoke("local_open", { path: parentPath(targets[0].path) }).catch((e) =>
          toast(String(e), true),
        ),
    });
  }
  items.push("-");
  items.push({
    label: "Properties",
    disabled: n !== 1,
    action: () => showProps(targets[0]),
  });
  items.push("-");
  items.push({
    label: n > 1 ? `Delete ${n} items` : "Delete",
    danger: true,
    action: () => deleteEntries(sess, half, targets),
  });
  return items;
}

/// Queue transfers after checking the destination pane for name conflicts.
/// A smaller existing file is treated as a partial transfer: offer to resume.
function transferItems(sess, fromRemote, items, dstDir) {
  const destHalf = fromRemote ? sess.ft.local : sess.ft.remote;
  const conflicts = items.filter((it) => destHalf.entries.some((e) => e.name === it.name));
  let ok = items;
  if (conflicts.length) {
    const names = conflicts.map((c) => c.name).join(", ");
    if (!confirm(`Overwrite ${conflicts.length} existing item(s)?\n${names}`)) {
      ok = items.filter((i) => !conflicts.includes(i));
    }
  }
  for (const item of ok) {
    const dst = destHalf.entries.find((e) => e.name === item.name);
    const partial =
      dst && !item.is_dir && !dst.is_dir && dst.size > 0 && dst.size < item.size;
    const resume =
      partial &&
      confirm(
        `${item.name}: partial file exists (${fmtBytes(dst.size)} / ${fmtBytes(item.size)}). Resume?`,
      );
    startTransfer(sess, fromRemote, item, dstDir, resume);
  }
}

async function editEntry(sess, half, ent) {
  try {
    if (half.isRemote) {
      const local = await invoke("sftp_edit_open", { sessionId: sess.sid, path: ent.path });
      await invoke("local_open", { path: local });
      toast(`${ent.name} — saving the local copy uploads it back`);
    } else {
      await invoke("local_open", { path: ent.path });
    }
  } catch (e) {
    toast(String(e), true);
  }
}

async function renameEntry(sess, half, ent) {
  const name = await askText(`Rename ${ent.name}`);
  if (!name || name === ent.name) return;
  const newPath = joinPath(parentPath(ent.path), name);
  try {
    if (half.isRemote) {
      await invoke("sftp_rename", { sessionId: sess.sid, oldPath: ent.path, newPath });
    } else {
      await invoke("local_rename", { path: ent.path, newPath });
    }
    half.refresh(half.cwd);
  } catch (e) {
    toast(`Rename: ${e}`, true);
  }
}

async function chmodEntry(sess, half, ent) {
  const cur = ent.perm != null ? fmtOctal(ent.perm) : "";
  const input = await askText(`Octal permissions for ${ent.name}`, cur);
  if (!input) return;
  const mode = parseInt(input, 8);
  if (Number.isNaN(mode) || mode < 0 || mode > 0o7777) {
    toast(`Invalid octal mode: ${input}`, true);
    return;
  }
  try {
    if (half.isRemote) {
      await invoke("sftp_chmod", { sessionId: sess.sid, path: ent.path, mode });
    } else {
      await invoke("local_chmod", { path: ent.path, mode });
    }
    half.refresh(half.cwd);
  } catch (e) {
    toast(`chmod: ${e}`, true);
  }
}

function showProps(ent) {
  const lines = [
    `Name:        ${ent.name}`,
    `Path:        ${ent.path}`,
    `Type:        ${ent.is_dir ? "Directory" : "File"}`,
    `Size:        ${ent.is_dir ? "—" : `${fmtBytes(ent.size)} (${ent.size} B)`}`,
    `Modified:    ${ent.mtime ? new Date(ent.mtime * 1000).toLocaleString() : "—"}`,
    `Permissions: ${ent.perm != null ? `${fmtOctal(ent.perm)} (${fmtPerm(ent.perm)})` : "—"}`,
    `Owner:       ${ent.uid != null ? `uid ${ent.uid}, gid ${ent.gid}` : "—"}`,
  ];
  showInfo(`Properties — ${ent.name}`, lines.join("\n"));
}

async function deleteEntries(sess, half, targets) {
  const names = targets.map((t) => t.name).join(", ");
  if (!confirm(`Delete ${targets.length} item(s)?\n${names}`)) return;
  for (const t of targets) {
    try {
      if (half.isRemote) {
        await invoke("sftp_delete", { sessionId: sess.sid, path: t.path });
      } else {
        await invoke("local_delete", { path: t.path });
      }
    } catch (e) {
      toast(`Delete ${t.name}: ${e}`, true);
    }
  }
  half.refresh(half.cwd);
}

function startTransfer(sess, fromRemote, item, dstDir, resume = false) {
  const op = `t${++sess.ft.seq}`;
  const dst = joinPath(dstDir, item.name);
  const line = document.createElement("div");
  line.className = "ft-xfer";
  line.textContent = `${fromRemote ? "⬇" : "⬆"} ${item.name} — queued`;
  sess.ft.status.append(line);
  sess.ft.ops.set(op, { line, fromRemote });
  invoke("sftp_transfer", {
    sessionId: sess.sid,
    id: op,
    upload: !fromRemote,
    src: item.path,
    dst,
    resume,
  }).catch((e) => {
    line.textContent = `${item.name} — ${e}`;
    line.classList.add("err");
  });
}

function onXferProgress(sess, p) {
  const op = sess.ft?.ops.get(p.id);
  if (!op) return;
  const total = p.total ? ` / ${fmtBytes(p.total)}` : "";
  op.line.textContent = `${op.fromRemote ? "⬇" : "⬆"} ${baseName(p.file)} — ${fmtBytes(p.done)}${total}`;
}

function onXferDone(sess, d) {
  const op = sess.ft?.ops.get(d.id);
  if (!op) return;
  sess.ft.ops.delete(d.id);
  if (d.ok) {
    op.line.classList.add("ok");
    op.line.textContent += " — done";
    setTimeout(() => op.line.remove(), 4000);
    // Refresh the receiving pane.
    const dest = op.fromRemote ? sess.ft.local : sess.ft.remote;
    dest.refresh(dest.cwd);
  } else {
    op.line.classList.add("err");
    op.line.textContent += ` — ${d.error}`;
    toast(`Transfer failed: ${d.error}`, true);
  }
}

/// OSC 7 payload is "file://<host>/<path>" — refresh matching remote panes.
function onCwdReport(sess, data) {
  const i = data.indexOf("/", "file://".length);
  if (i < 0) return;
  const path = decodeURIComponent(data.slice(i));
  for (const t of tabs) {
    const remote = t.ft?.remote;
    if (t.kind === "ftp" && t.host.id === sess.host.id && remote && remote.cwd !== path) {
      remote.refresh(path);
    }
  }
}

/* ---------------- helpers ---------------- */
// parentPath/joinPath/baseName/fmtBytes/fmtOctal/fmtPerm/fmtDate live in
// util.js (loaded before this file) so they can be unit-tested in node.

function toast(msg, isError = false) {
  const el = document.createElement("div");
  el.className = "toast" + (isError ? " error" : "");
  el.textContent = msg;
  document.getElementById("toasts").append(el);
  setTimeout(() => el.remove(), 5000);
}

// Kill orphaned backend sessions if this is a webview reload.
invoke("reset_sessions").catch(() => {});
loadHosts();
