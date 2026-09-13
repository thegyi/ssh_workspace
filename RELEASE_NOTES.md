# SSH Workspace 1.0

A desktop SSH workspace built with Tauri 2 + Rust + vanilla JS/xterm.js:
organize hosts, run multiple SSH terminals, transfer files over SFTP,
forward ports, and run remote GUI apps via X11 forwarding.

## Highlights

### Host manager
- Add / edit / remove / **duplicate** hosts; search filter and collapsible groups
- **Import from `~/.ssh/config`** — checkbox picker, `IdentityFile` → key auth
- Auth methods: password, private key (~ expansion, optional passphrase), SSH agent, **keyboard-interactive / 2FA** prompts
- **Secrets in the OS keyring** — Secret Service (Linux), Keychain (macOS), Credential Manager (Windows); `hosts.json` keeps only `keyring:` markers, plaintext fallback with self-healing re-prompt
- Per-host **reset key** — removes entries from `~/.ssh/known_hosts` (handles hashed entries, keeps `.old` backup)
- Trust-on-first-use host keys; changed keys are rejected with a clear warning
- **ProxyJump** — route connections through a bastion host (one level)
- **Port forwarding** per host: local `-L`, remote `-R`, dynamic SOCKS5 `-D`

### Terminals
- Multiple simultaneous SSH terminals, one tab per session
- PTY with `xterm-256color`, live resize propagation
- **Scrollback search** (Ctrl+Shift+F) via xterm search addon
- **Reconnect** dead sessions in place; status dot (connecting / live / dead)
- **Broadcast input** — type into all open terminals at once
- **Session logging** (raw `.log`) and **asciinema recording** (`.cast`, playable via `asciinema play`)
- Tab drag-reorder, Ctrl+PageUp/PageDown cycling
- Long-command desktop notifications via OSC 133 shell hooks
- Right-click menu: copy selection, copy all scrollback, select all, paste, clear
- Ctrl+Shift+C / Ctrl+Shift+V clipboard shortcuts
- SSH keepalive (30s) on every session — idle tabs survive NAT/firewall timeouts

### SFTP file manager
- Dual-pane browser per host: **local** left, **remote** right
- Drag-and-drop upload/download, **recursive** for directories
- **Parallel transfers** — up to 4 concurrent transfers on dedicated SSH sessions
- **Resume** interrupted transfers (append at existing offset), overwrite confirmation
- Rename, chmod/permissions, file properties, open local files, **edit remote files in place** (uploads back on save)
- Multi-select: Ctrl/Cmd toggle, Shift range-select; hidden-file toggle, create directory
- **Per-host remote bookmarks** (★), copy-path / reveal-in-manager items
- SFTP pane **follows the shell cwd** via OSC 7 shell integration (bash)
- Live transfer progress in the status bar

### X11 forwarding
- Per-host toggle — remote GUI apps render on the local display
- Implemented via remote port forwarding; no `X11Forwarding` needed in sshd
- Cookie injected into the X11 setup packet — no remote `xauth` needed
- `DISPLAY` exported into the shell automatically
- Linux/macOS (XQuartz) via unix socket; Windows via `127.0.0.1:6000` (VcXsrv/Xming)

### Settings
- Dark / light theme, terminal font (system font dropdown) and size — persisted in `localStorage`
- Dark-styled dropdowns matching the app theme

## Packages

- **Linux:** `.deb`, `.rpm` — runtime deps `libssl3`, `libwebkit2gtk-4.1-0`, `libgtk-3-0`, `libdbus-1-3`
- **Windows:** NSIS `.exe` installer + MSI
- **macOS:** `.app` + `.dmg` (unsigned — right-click → Open on first launch)
- Wayland-ready: desktop entry matches the app id for proper taskbar/pin icons

## CI

GitHub Actions on Node 24-era actions: check job (`node --check`, `cargo check`),
Linux + Windows + macOS bundle builds, and tag-triggered GitHub Releases
(`release-*` / `Release-*` / `v*` tags publish this file as the release body).

## Data locations

- Hosts: `~/.config/ssh-workspace/hosts.json` (secrets are `keyring:` markers)
- Secrets: OS keyring, service name `dev.sshworkspace`
- Logs/casts: `<data_dir>/ssh-workspace/logs|casts/`
- Host keys: shared `~/.ssh/known_hosts`
