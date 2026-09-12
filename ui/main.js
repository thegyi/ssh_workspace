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

/* ---------------- hosts ---------------- */

async function loadHosts() {
  hosts = await invoke("list_hosts");
  renderHosts();
}

function renderHosts() {
  hostListEl.innerHTML = "";
  for (const h of hosts) {
    const li = document.createElement("li");
    li.className = "host-item";

    const info = document.createElement("div");
    info.className = "host-info";
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
    hostListEl.append(li);
  }
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
  hostModal.hidden = false;
  f("f-name").focus();
}

hostForm.addEventListener("submit", async (e) => {
  e.preventDefault();
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
  };
  try {
    await invoke("save_host", { host });
    hostModal.hidden = true;
    await loadHosts();
  } catch (err) {
    toast(String(err), true);
  }
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

/* ---------------- generic text prompt ---------------- */

const textModal = document.getElementById("text-modal");
const textInput = document.getElementById("text-input");
let textResolve = null;

function askText(title) {
  document.getElementById("text-title").textContent = title;
  textInput.value = "";
  textModal.hidden = false;
  textInput.focus();
  return new Promise((resolve) => (textResolve = resolve));
}

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
  tabsEl.append(tabEl);

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

function closeTab(sess) {
  if (sess.sid && !sess.dead) {
    invoke(sess.kind === "ftp" ? "sftp_close" : "ssh_close", { sessionId: sess.sid }).catch(
      () => {},
    );
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
  }
});

/* ---------------- terminals ---------------- */

async function openSession(host) {
  const secret = await resolveSecret(host);
  if (secret === undefined) return;
  const sess = createTab(host, "term");
  activateTab(sess);
  buildTerminal(sess);
  try {
    const res = await invoke("connect_host", {
      hostId: host.id,
      secret,
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
    sess.term.focus();
  } catch (err) {
    sess.term.write(`\x1b[31m${String(err)}\x1b[0m\r\n`);
    markDead(sess, null);
  }
}

function buildTerminal(sess) {
  const term = new Terminal({
    fontFamily: '"Cascadia Mono", "JetBrains Mono", Menlo, Consolas, monospace',
    fontSize: 13,
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
  term.open(sess.pane);
  fit.fit();
  sess.term = term;
  sess.fit = fit;
  sess.ro = new ResizeObserver(() => {
    if (sess.pane.offsetParent !== null) fit.fit();
  });
  sess.ro.observe(sess.pane);

  term.onData((d) => {
    if (sess.sid && !sess.dead) {
      invoke("ssh_write", { sessionId: sess.sid, data: Array.from(encoder.encode(d)) }).catch(
        () => {},
      );
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
      { label: "Clear", action: () => term.clear() },
    ]);
  });

  term.attachCustomKeyEventHandler((e) => {
    if (e.type !== "keydown" || !e.ctrlKey || !e.shiftKey) return true;
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
    return true;
  });
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
      invoke("ssh_write", {
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

/* ---------------- sftp file transfer ---------------- */

async function openFtp(host) {
  const secret = await resolveSecret(host);
  if (secret === undefined) return;
  const sess = createTab(host, "ftp");
  activateTab(sess);
  let res;
  try {
    res = await invoke("sftp_open", { hostId: host.id, secret });
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
  };

  half.refresh = async (p) => {
    try {
      const res = isRemote
        ? await invoke("sftp_list", { sessionId: sess.sid, path: p ?? half.cwd })
        : await invoke("local_list", { path: p ?? half.cwd });
      half.cwd = res.path;
      path.value = res.path;
      half.entries = res.entries;
      renderEntries(half);
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
    for (const item of data.items) startTransfer(sess, data.isRemote, item, half.cwd);
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
      for (const t of targets) startTransfer(sess, half.isRemote, t, dest);
    },
  });
  // Future items: { label: "Rename", action: ... }, { label: "Properties", disabled: true }, ...
  items.push("-");
  items.push({
    label: n > 1 ? `Delete ${n} items` : "Delete",
    danger: true,
    action: () => deleteEntries(sess, half, targets),
  });
  return items;
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

function startTransfer(sess, fromRemote, item, dstDir) {
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

/* ---------------- helpers ---------------- */

function parentPath(p) {
  const t = p.replace(/\/+$/, "");
  const i = t.lastIndexOf("/");
  return i <= 0 ? "/" : t.slice(0, i);
}

function joinPath(dir, name) {
  return dir.endsWith("/") ? dir + name : `${dir}/${name}`;
}

function baseName(p) {
  return p.replace(/\/+$/, "").split("/").pop() || p;
}

function fmtBytes(n) {
  if (!n) return "0 B";
  const u = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(u.length - 1, Math.floor(Math.log2(n) / 10));
  return `${(n / 2 ** (10 * i)).toFixed(i ? 1 : 0)} ${u[i]}`;
}

function fmtDate(ts) {
  const d = new Date(ts * 1000);
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });
}

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
