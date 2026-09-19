# SSH Workspace 1.1

A desktop SSH workspace built with Tauri 2 + Rust + vanilla JS/xterm.js:
organize hosts, run multiple SSH terminals, transfer files over SFTP,
forward ports, run serial consoles, and drive remote GUI apps via X11.

## What's new in 1.1

### Serial console
- New `⌁` button opens a serial modal: port picker with refresh, baud
  rate (9600–921600), data bits, parity, stop bits, flow control
  (RTS/CTS, XON/XOFF), Enter-key line ending (CR/LF/CR+LF), local echo
- Each port opens a dedicated tab with a real terminal: scrollback
  search, clipboard shortcuts, context menu with **Send BREAK**

### File manager
- **OS drag-and-drop** — drop files from Explorer/Finder/Nautilus onto
  the remote pane to upload (works via `tauri://drag-drop`, so it's
  reliable on Windows too)
- **Double-click actions** (opt-in via Settings): local file opens with
  the system default app, remote file downloads into the local pane

### Terminal
- Press **R** on a dead terminal to reconnect (same as the button)

### Fixes
- **Secrets on Linux actually persist** — the keyring was silently
  compiled with an in-memory mock backend; now uses Secret Service
  (GNOME Keyring/KDE Wallet), with a self-healing prompt when a stored
  marker can't be resolved
- Windows release builds no longer spawn a stray console window
- macOS `.app` now ships as a proper zip in release assets (loose
  `Info.plist`/`icon.icns` no longer leak into the release)
- `Release-*` tags (capital R) trigger the release job

### Testing & CI
- 23 Rust unit tests (ssh-config parsing, host store, secrets, SFTP
  local ops, SOCKS5 parser, `reset_host_key`) + 6 frontend tests for
  the path/format helpers
- New `lint-workflows` job runs actionlint on every push — workflow
  mistakes now fail fast instead of at release time

## Full feature set

### Host manager
- Add / edit / remove / duplicate hosts; search filter and collapsible
  groups (state persisted)
- Import from `~/.ssh/config`
- Auth: password, private key, SSH agent, keyboard-interactive / 2FA
- Secrets in the OS keyring; `hosts.json` keeps only markers
- Per-host reset key, TOFU host keys, ProxyJump, `-L`/`-R`/`-D` forwards

### Terminals
- Multiple simultaneous tabs, scrollback search, reconnect, broadcast
  input, raw `.log` and asciinema `.cast` recording, tab drag-reorder,
  long-command notifications, keepalive

### SFTP
- Dual-pane local/remote browser, drag-and-drop, parallel transfers
  with resume, rename/chmod/properties, remote edit-in-place,
  per-host bookmarks, shell-cwd following

### X11 forwarding
- Linux/macOS (XQuartz) via unix socket, Windows via `127.0.0.1:6000`

### Settings
- Dark/light theme, system font dropdown, font size, file double-click
  toggle — persisted locally

## Packages

- **Linux:** `.deb`, `.rpm` — deps `libssl3`, `libwebkit2gtk-4.1-0`,
  `libgtk-3-0`, `libdbus-1-3`
- **Windows:** NSIS `.exe` installer + MSI
- **macOS:** `.dmg` + `.app` zipped (unsigned — right-click → Open)
- Wayland-ready desktop entry

## Data locations

- Hosts: `~/.config/ssh-workspace/hosts.json` (secrets are `keyring:` markers)
- Secrets: OS keyring, service name `dev.sshworkspace`
- Logs/casts: `<data_dir>/ssh-workspace/logs|casts/`
- Host keys: shared `~/.ssh/known_hosts`
