# SSH Workspace

A desktop app (Tauri 2 + Rust + vanilla JS/xterm.js) that organizes SSH hosts,
opens multiple SSH terminal sessions side by side in tabs, and transfers files
over SFTP with a dual-pane drag & drop browser.

## Features

- **Host manager** (left sidebar)
  - Add / edit / remove hosts (name, host, port, username)
  - Auth methods: password, private key file (with optional passphrase), ssh-agent
  - Per-host action buttons: `⇄` open file transfer, `✎` edit, `⟲` reset host
    key, `✕` remove
  - Reset host key — removes the host's entries from `~/.ssh/known_hosts`
    (uses `ssh-keygen -R`, which also handles hashed entries)
  - Trust-on-first-use: unknown host keys are stored in `~/.ssh/known_hosts`
    automatically; a changed key is rejected with a mismatch warning
- **Console**
  - Unlimited simultaneous terminal tabs (xterm.js), one SSH channel each
  - Full PTY (`xterm-256color`), live resize propagation, per-tab close
  - Deleting a host disconnects its open sessions
- **File transfer** (⇄ button per host)
  - Dual-pane SFTP browser: remote filesystem left, local filesystem right
  - Double-click to enter directories, `..` / `↑` to go up, editable path bar
  - Drag & drop files or whole directories between panes (recursive);
    Ctrl-click for multi-select
  - Live transfer progress, auto-refresh of the destination pane

## Requirements

System packages (Ubuntu/Debian):

```sh
sudo apt install libwebkit2gtk-4.1-dev libsoup-3.0-dev \
    libjavascriptcoregtk-4.1-dev libgtk-3-dev libssl-dev pkg-config
```

Toolchain: Node.js + npm, and Rust ≥ 1.77 (`rustup` stable works).

## Run

```sh
npm install     # once
npm run dev     # tauri dev (debug build with hot frontend reload)
npm run build   # tauri build (release binary + .deb/.AppImage bundles)
```

Note: `npm run dev`/`build` prepend `~/.cargo/bin` to `PATH` so the rustup
toolchain is used instead of an older system cargo.

## Where things are stored

| Data            | Location                               |
| --------------- | -------------------------------------- |
| Hosts           | `~/.config/ssh-workspace/hosts.json`   |
| Host keys       | `~/.ssh/known_hosts` (shared with ssh) |

Passwords/passphrases you save are stored in **plaintext** in `hosts.json`.
Leave the field empty in the host form to be prompted at connect time instead.

## Layout

```
ui/                 vanilla JS frontend (index.html, styles.css, main.js)
ui/vendor/          xterm.js + fit addon (UMD, copied from node_modules)
src-tauri/          Rust backend
  src/hosts.rs      host store (JSON persistence)
  src/ssh.rs        SSH session manager, host-key checking, I/O pump threads
  src/sftp.rs       SFTP sessions: listing, recursive up/download, progress events
  src/main.rs       Tauri commands + app setup
```

## How it works

**Terminal tabs** — each maps to an SSH channel with a PTY. `connect` performs
TCP + SSH handshake, verifies/stores the host key, authenticates, requests the
shell, then spawns a pump thread that owns the channel in non-blocking mode: it
drains an mpsc command queue (write / resize / disconnect) and forwards channel
output to the frontend as `ssh-data-<id>` events (`Vec<u8>` →
`term.write(Uint8Array)`). Keystrokes go the other way via `ssh_write`.

**SFTP tabs** — `sftp_open` reuses the same `establish()` path (handshake,
host-key check, auth) then opens the SFTP subsystem on a dedicated worker
thread. The thread services an mpsc queue: `List` (replied via oneshot channel)
and `Transfer` (recursive upload/download, emitting `sftp-progress-<id>` and
`sftp-done-<id>` events). The local pane is served directly by `local_list`.

## IPC commands (developer reference)

These are the Tauri commands the frontend calls internally via
`window.__TAURI__.core.invoke("name", args)` — not something you type in the UI.
They can be invoked manually from the webview devtools console for debugging.

| Command           | Purpose                                        |
| ----------------- | ---------------------------------------------- |
| `list_hosts`      | List saved hosts                               |
| `save_host`       | Create or update a host (blank secret = keep)  |
| `delete_host`     | Remove host + disconnect its sessions          |
| `reset_host_key`  | Remove the host's `known_hosts` entries        |
| `connect_host`    | Open a shell+PTY session, returns session id   |
| `ssh_write`       | Write bytes to a session                       |
| `ssh_resize`      | Propagate terminal resize (PTY size)           |
| `ssh_close`       | Disconnect a session                           |
| `sftp_open`       | Open an SFTP session, returns id + remote home |
| `sftp_list`       | List a remote directory                        |
| `sftp_transfer`   | Queue an upload/download (recursive)           |
| `sftp_close`      | Close an SFTP session                          |
| `local_list`      | List a local directory                         |
