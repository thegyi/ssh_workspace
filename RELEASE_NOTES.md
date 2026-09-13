# SSH Workspace 0.1
A desktop SSH workspace built with Tauri 2 + Rust + vanilla JS/xterm.js:
organize hosts, open multiple SSH terminals, transfer files over SFTP,
and run remote GUI apps via X11 forwarding.

## Highlights

### Host manager
- Add / edit / remove hosts (name, host, port, username)
- Auth methods: password, private key (~ expansion, optional passphrase), SSH agent
- Per-host **reset key** — removes entries from `~/.ssh/known_hosts` (handles hashed entries, keeps `.old` backup)
- Trust-on-first-use host keys; changed keys are rejected with a clear warning

### Terminals
- Multiple simultaneous SSH terminals, one tab per session
- PTY with `xterm-256color`, live resize propagation
- Per-tab close, connection status dot (connecting / live / dead)
- Right-click menu: copy selection, copy all scrollback, select all, paste, clear
- Ctrl+Shift+C / Ctrl+Shift+V clipboard shortcuts
- SSH keepalive (30s) on every session — idle tabs survive NAT/firewall timeouts

### SFTP file transfer
- Dual-pane browser per host: **local** left, **remote** right
- Drag-and-drop upload/download, **recursive** for directories
- Multi-select: Ctrl/Cmd toggle, Shift range-select
- Hidden-file toggle, create directory, refresh, parent navigation
- Right-click menus: download/upload, **recursive delete**, new directory
- Live transfer progress in the status bar

### X11 forwarding (Linux)
- Per-host toggle — remote GUI apps render on the local display
- Implemented via remote port forwarding; no `X11Forwarding` needed in sshd
- Cookie is injected into the X11 setup packet — no remote `xauth` needed
- `DISPLAY` is exported into the shell automatically

## Packages

- **Linux:** `.deb`, `.rpm`, `.AppImage`
- **Windows:** NSIS `.exe` installer + MSI
- Runtime deps declared: `libssl3`, `libwebkit2gtk-4.1-0`, `libgtk-3-0` (deb)
- Wayland-ready: desktop entry matches the app id for proper taskbar/pin icons

## CI

GitHub Actions: check job (`node --check`, `cargo check`, `cargo test`),
Linux bundle build, Windows bundle build — artifacts uploaded per run.

## Data locations

- Hosts: `~/.config/ssh-workspace/hosts.json`
- Host keys: shared `~/.ssh/known_hosts`
