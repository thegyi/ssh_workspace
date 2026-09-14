use crate::hosts::{expand_tilde, AuthMethod, Host};
use serde::Serialize;
use ssh2::{CheckResult, KeyboardInteractivePrompt, KnownHostFileKind, Session};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

// ---------- keyboard-interactive (2FA) prompt bridge ----------
//
// The server may require keyboard-interactive auth (password+OTP, PAM
// challenges). The connect happens on a blocking thread, so the prompter
// emits an event to the frontend and waits for `auth_prompt_reply`.

static PROMPT_ID: AtomicU64 = AtomicU64::new(1);

fn auth_replies() -> &'static Mutex<HashMap<u64, Sender<Vec<String>>>> {
    static R: std::sync::OnceLock<Mutex<HashMap<u64, Sender<Vec<String>>>>> =
        std::sync::OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Frontend callback: answers for the last emitted `ssh-auth-prompt` event.
pub fn auth_prompt_reply(id: u64, answers: Vec<String>) {
    if let Some(tx) = auth_replies().lock().unwrap().remove(&id) {
        let _ = tx.send(answers);
    }
}

#[derive(Serialize, Clone)]
struct AuthPromptReq {
    id: u64,
    username: String,
    instructions: String,
    /// (prompt text, echo input visibly) pairs, one answer expected each.
    prompts: Vec<(String, bool)>,
}

struct UiPrompter<'a> {
    app: &'a AppHandle,
}

impl KeyboardInteractivePrompt for UiPrompter<'_> {
    fn prompt<'p>(
        &mut self,
        username: &str,
        instructions: &str,
        prompts: &[ssh2::Prompt<'p>],
    ) -> Vec<String> {
        let id = PROMPT_ID.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        auth_replies().lock().unwrap().insert(id, tx);
        let _ = self.app.emit(
            "ssh-auth-prompt",
            AuthPromptReq {
                id,
                username: username.to_string(),
                instructions: instructions.to_string(),
                prompts: prompts
                    .iter()
                    .map(|p| (p.text.to_string(), p.echo))
                    .collect(),
            },
        );
        rx.recv_timeout(Duration::from_secs(300)).unwrap_or_default()
    }
}

/// Ask the frontend for one masked input via the `ssh-auth-prompt` bridge.
/// Used when a stored keyring marker fails to resolve (locked/missing
/// keyring) so the connect can still proceed instead of dead-ending.
fn ui_prompt(app: &AppHandle, prompt: &str) -> Option<String> {
    let id = PROMPT_ID.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    auth_replies().lock().unwrap().insert(id, tx);
    let _ = app.emit(
        "ssh-auth-prompt",
        AuthPromptReq {
            id,
            username: String::new(),
            instructions: String::new(),
            prompts: vec![(prompt.to_string(), false)],
        },
    );
    rx.recv_timeout(Duration::from_secs(300))
        .ok()
        .and_then(|mut a| a.pop())
}

pub enum SshCommand {
    Data(Vec<u8>),
    Resize { cols: u32, rows: u32 },
    /// Some(..) records all session output to a file; None stops.
    SetLog(Option<LogSpec>),
    Disconnect,
}

/// Session recording target: raw byte stream or asciinema v2 (.cast).
pub struct LogSpec {
    pub path: PathBuf,
    pub cast: bool,
    pub cols: u32,
    pub rows: u32,
}

enum SessLog {
    Raw(std::fs::File),
    Cast {
        f: std::fs::File,
        start: std::time::Instant,
    },
}

impl SessLog {
    fn open(spec: LogSpec) -> Option<Self> {
        if let Some(dir) = spec.path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        if !spec.cast {
            return std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&spec.path)
                .ok()
                .map(SessLog::Raw);
        }
        let mut f = std::fs::File::create(&spec.path).ok()?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let header = serde_json::json!({
            "version": 2,
            "width": spec.cols,
            "height": spec.rows,
            "timestamp": ts,
            "env": { "TERM": "xterm-256color" },
        });
        writeln!(f, "{header}").ok()?;
        Some(SessLog::Cast {
            f,
            start: std::time::Instant::now(),
        })
    }

    fn push(&mut self, data: &[u8]) {
        match self {
            SessLog::Raw(f) => {
                let _ = f.write_all(data);
            }
            SessLog::Cast { f, start } => {
                let ev = serde_json::json!([
                    start.elapsed().as_secs_f64(),
                    "o",
                    String::from_utf8_lossy(data),
                ]);
                let _ = writeln!(f, "{ev}");
            }
        }
    }
}

struct SessionEntry {
    tx: Sender<SshCommand>,
    host_id: String,
}

struct ManagerInner {
    sessions: Mutex<HashMap<String, SessionEntry>>,
}

#[derive(Clone)]
pub struct SshManager {
    inner: Arc<ManagerInner>,
}

impl SshManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ManagerInner {
                sessions: Mutex::new(HashMap::new()),
            }),
        }
    }

    fn insert(&self, id: &str, entry: SessionEntry) {
        self.inner
            .sessions
            .lock()
            .unwrap()
            .insert(id.to_string(), entry);
    }

    fn remove(&self, id: &str) {
        self.inner.sessions.lock().unwrap().remove(id);
    }

    pub fn send(&self, id: &str, cmd: SshCommand) -> Result<(), String> {
        let sessions = self.inner.sessions.lock().unwrap();
        match sessions.get(id) {
            Some(entry) => entry.tx.send(cmd).map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }

    /// Disconnect every session attached to the given host (used on host delete).
    pub fn disconnect_host(&self, host_id: &str) {
        let ids: Vec<String> = {
            let sessions = self.inner.sessions.lock().unwrap();
            sessions
                .iter()
                .filter(|(_, e)| e.host_id == host_id)
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in ids {
            let _ = self.send(&id, SshCommand::Disconnect);
        }
    }

    /// Disconnect all sessions (used when the webview reloads — orphaned
    /// sessions are unreachable from a fresh frontend).
    pub fn disconnect_all(&self) {
        let ids: Vec<String> = self.inner.sessions.lock().unwrap().keys().cloned().collect();
        for id in ids {
            let _ = self.send(&id, SshCommand::Disconnect);
        }
    }
}

/// TCP connect + SSH handshake + host-key check + authentication.
/// Shared by shell sessions and SFTP sessions.
/// When `jump` is set, the TCP connection is routed through that host via
/// a direct-tcpip channel bridged to a local listener (ProxyJump).
/// Returns the session and an optional first-connection notice.
pub fn establish(
    host: &Host,
    secret: Option<String>,
    jump: Option<(&Host, Option<String>)>,
    app: Option<&AppHandle>,
    known_hosts: &Path,
) -> Result<(Session, Option<String>), String> {
    let tcp = match jump {
        Some((jh, js)) => {
            if jh.jump.is_some() {
                return Err("nested jump hosts are not supported".into());
            }
            let port = jump_bridge(jh, js, &host.host, host.port, app, known_hosts)?;
            TcpStream::connect(("127.0.0.1", port))
                .map_err(|e| format!("jump bridge connect failed: {e}"))?
        }
        None => {
            let addr_str = format!("{}:{}", host.host, host.port);
            let addr = addr_str
                .to_socket_addrs()
                .map_err(|e| format!("cannot resolve {addr_str}: {e}"))?
                .next()
                .ok_or_else(|| format!("cannot resolve {addr_str}"))?;
            TcpStream::connect_timeout(&addr, Duration::from_secs(10))
                .map_err(|e| format!("cannot connect to {addr_str}: {e}"))?
        }
    };
    tcp.set_read_timeout(Some(Duration::from_secs(30))).ok();
    tcp.set_write_timeout(Some(Duration::from_secs(30))).ok();
    tcp.set_nodelay(true).ok();

    let mut sess = Session::new().map_err(|e| e.to_string())?;
    sess.set_tcp_stream(tcp);
    sess.handshake()
        .map_err(|e| format!("SSH handshake failed: {e}"))?;

    let notice = check_host_key(&sess, &host.host, host.port, known_hosts)?;

    authenticate(&mut sess, host, secret, app)?;
    if !sess.authenticated() {
        return Err("authentication failed".into());
    }
    // SSH-level keepalive (ignore messages) every 30s; want_reply=true so a
    // dead connection is detected instead of silently hanging.
    sess.set_keepalive(true, 30);
    Ok((sess, notice))
}

/// ProxyJump transport: open a direct-tcpip channel on the jump host's
/// session and bridge it to a one-shot local TCP listener. Returns the
/// local port; the bridge serves exactly one connection (the inner SSH
/// session) and exits when it does.
fn jump_bridge(
    jump: &Host,
    secret: Option<String>,
    target_host: &str,
    target_port: u16,
    app: Option<&AppHandle>,
    known_hosts: &Path,
) -> Result<u16, String> {
    let (jsess, _) = establish(jump, secret, None, app, known_hosts)
        .map_err(|e| format!("jump host {}: {e}", jump.host))?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("jump bridge listen failed: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let target = (target_host.to_string(), target_port);
    let alive = AtomicBool::new(true);
    thread::spawn(move || {
        let Ok((mut sock, _)) = listener.accept() else {
            return;
        };
        let mut chan = match jsess.channel_direct_tcpip(&target.0, target.1, None) {
            Ok(c) => c,
            Err(_) => return,
        };
        jsess.set_blocking(false);
        sock.set_nonblocking(true).ok();
        splice_channel(&mut chan, &mut sock, &alive);
    });
    Ok(port)
}

/// Bidirectional copy between a nonblocking SSH channel and a nonblocking
/// TCP stream until either side closes or `alive` flips false.
/// Shared by the jump bridge and the tunnel module.
pub fn splice_channel(chan: &mut ssh2::Channel, sock: &mut TcpStream, alive: &AtomicBool) {
    let mut to_sock: VecDeque<u8> = VecDeque::new();
    let mut to_chan: VecDeque<u8> = VecDeque::new();
    let mut buf = [0u8; 65536];
    let mut s_eof = false;
    loop {
        if !alive.load(Ordering::Relaxed) {
            return;
        }
        let mut active = false;
        if !chan.eof() {
            match chan.read(&mut buf) {
                Ok(0) => {}
                Ok(n) => {
                    active = true;
                    to_sock.extend(&buf[..n]);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return,
            }
        }
        if !s_eof {
            match sock.read(&mut buf) {
                Ok(0) => s_eof = true,
                Ok(n) => {
                    active = true;
                    to_chan.extend(&buf[..n]);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return,
            }
        }
        while !to_sock.is_empty() {
            match sock.write(to_sock.make_contiguous()) {
                Ok(0) => break,
                Ok(n) => {
                    to_sock.drain(..n);
                    active = true;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => return,
            }
        }
        while !to_chan.is_empty() {
            match chan.write(to_chan.make_contiguous()) {
                Ok(0) => break,
                Ok(n) => {
                    to_chan.drain(..n);
                    active = true;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => return,
            }
        }
        if chan.eof() && s_eof && to_sock.is_empty() && to_chan.is_empty() {
            return;
        }
        if !active {
            thread::sleep(Duration::from_millis(4));
        }
    }
}

/// Connect to a host, open a shell with a PTY and spawn the I/O pump thread.
/// Returns the session id and an optional notice to display in the terminal.
pub fn connect(
    app: AppHandle,
    mgr: SshManager,
    host: Host,
    secret: Option<String>,
    jump: Option<(Host, Option<String>)>,
    cols: u32,
    rows: u32,
    known_hosts: &Path,
) -> Result<(String, Option<String>), String> {
    let jump_ref = jump.as_ref().map(|(h, s)| (h, s.clone()));
    let (sess, notice) = establish(&host, secret.clone(), jump_ref, Some(&app), known_hosts)?;

    let mut channel = sess
        .channel_session()
        .map_err(|e| format!("cannot open channel: {e}"))?;
    channel
        .request_pty("xterm-256color", None, Some((cols, rows, 0, 0)))
        .map_err(|e| format!("pty request failed: {e}"))?;
    channel
        .shell()
        .map_err(|e| format!("shell request failed: {e}"))?;
    sess.set_blocking(false);

    let session_id = format!("s{}", SESSION_COUNTER.fetch_add(1, Ordering::Relaxed));
    let (tx, rx) = mpsc::channel::<SshCommand>();
    mgr.insert(
        &session_id,
        SessionEntry {
            tx: tx.clone(),
            host_id: host.id.clone(),
        },
    );

    let data_event = format!("ssh-data-{session_id}");
    let exit_event = format!("ssh-exit-{session_id}");
    let alive = Arc::new(AtomicBool::new(true));

    // Shell integration (bash): emit OSC 7 (cwd) after every prompt so SFTP
    // panes can follow `cd`, and OSC 133 C/D (command start/finish+exit) so
    // the frontend can notify on long-running commands. Harmless on shells
    // that ignore PROMPT_COMMAND/DEBUG traps. Leading space keeps it out of
    // history (HISTCONTROL=ignorespace); trailing `clear` hides the echo.
    let _ = tx.send(SshCommand::Data(
        concat!(
            " SSHWS_OLDPC=\"${PROMPT_COMMAND:-}\"; ",
            "__sshws_pc() { local r=$?; __sshws_in=1; ",
            "printf '\\033]7;file://%s%s\\007' \"$(hostname)\" \"$PWD\"; ",
            "printf '\\033]133;D;%s\\007' \"$r\"; ",
            "[ -n \"$SSHWS_OLDPC\" ] && eval \"$SSHWS_OLDPC\"; __sshws_in=0; }; ",
            "PROMPT_COMMAND=__sshws_pc; ",
            "trap '[ \"${__sshws_in:-0}\" = 0 ] && printf \"\\033]133;C\\007\"' DEBUG; ",
            "clear\n"
        )
        .as_bytes()
        .to_vec(),
    ));

    // X11 forwarding: second SSH session holding a remote port forward,
    // each inbound connection proxied to the local X server. Works
    // everywhere a local X server is reachable (X11/XWayland on Linux,
    // XQuartz on macOS, VcXsrv/Xming on Windows).
    if host.x11 {
        let (x_app, x_event, x_host, x_secret, x_jump, x_kh, x_tx, x_alive) = (
            app.clone(),
            data_event.clone(),
            host.clone(),
            secret.clone(),
            jump.clone(),
            known_hosts.to_path_buf(),
            tx.clone(),
            alive.clone(),
        );
        thread::spawn(move || {
            crate::x11::start(x_app, x_event, x_host, x_secret, x_jump, x_kh, x_tx, x_alive);
        });
    }

    // Port forwards configured on the host (local / remote / socks5).
    if !host.tunnels.is_empty() {
        crate::tunnels::start_all(
            app.clone(),
            data_event.clone(),
            host.clone(),
            secret.clone(),
            jump,
            known_hosts.to_path_buf(),
            alive.clone(),
        );
    }

    let thread_mgr = mgr.clone();
    let thread_id = session_id.clone();
    thread::spawn(move || {
        pump_loop(sess, channel, rx, &app, &data_event);
        alive.store(false, Ordering::Relaxed);
        thread_mgr.remove(&thread_id);
        let _ = app.emit(&exit_event, 0);
    });

    Ok((session_id, notice))
}

/// I/O pump: owns the session + channel, multiplexes reads and writes.
fn pump_loop(
    sess: Session,
    mut channel: ssh2::Channel,
    rx: mpsc::Receiver<SshCommand>,
    app: &AppHandle,
    data_event: &str,
) {
    let mut buf = [0u8; 32768];
    let mut pending: VecDeque<u8> = VecDeque::new();
    let mut log: Option<SessLog> = None;
    let mut last_ka = std::time::Instant::now();
    loop {
        if last_ka.elapsed() >= Duration::from_secs(15) {
            last_ka = std::time::Instant::now();
            if let Err(e) = sess.keepalive_send().map_err(std::io::Error::from) {
                if e.kind() != std::io::ErrorKind::WouldBlock {
                    return; // connection is dead
                }
            }
        }

        let mut disconnect = false;
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                SshCommand::Data(d) => pending.extend(d),
                SshCommand::Resize { cols, rows } => {
                    let _ = channel.request_pty_size(cols, rows, None, None);
                }
                SshCommand::SetLog(spec) => {
                    log = spec.and_then(SessLog::open);
                }
                SshCommand::Disconnect => disconnect = true,
            }
        }
        if disconnect {
            let _ = channel.close();
            let _ = channel.wait_close();
            return;
        }

        while !pending.is_empty() {
            match channel.write(pending.make_contiguous()) {
                Ok(0) => break,
                Ok(n) => {
                    pending.drain(..n);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => return,
            }
        }

        let mut got_data = false;
        match channel.read(&mut buf) {
            Ok(n) if n > 0 => {
                got_data = true;
                if let Some(f) = log.as_mut() {
                    f.push(&buf[..n]);
                }
                let _ = app.emit(data_event, buf[..n].to_vec());
            }
            _ => {}
        }
        match channel.stderr().read(&mut buf) {
            Ok(n) if n > 0 => {
                got_data = true;
                if let Some(f) = log.as_mut() {
                    f.push(&buf[..n]);
                }
                let _ = app.emit(data_event, buf[..n].to_vec());
            }
            _ => {}
        }

        if channel.eof() {
            return;
        }
        if !got_data {
            thread::sleep(Duration::from_millis(4));
        }
    }
}

fn check_host_key(
    sess: &Session,
    host: &str,
    port: u16,
    known_hosts: &Path,
) -> Result<Option<String>, String> {
    let (key, kind) = sess
        .host_key()
        .ok_or_else(|| "server did not present a host key".to_string())?;
    let mut kh = sess.known_hosts().map_err(|e| e.to_string())?;
    if known_hosts.exists() {
        kh.read_file(known_hosts, KnownHostFileKind::OpenSSH)
            .map_err(|e| format!("cannot read {}: {e}", known_hosts.display()))?;
    }
    match kh.check_port(host, port, key) {
        CheckResult::Match => Ok(None),
        CheckResult::NotFound => {
            if let Some(parent) = known_hosts.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            let host_id = if port == 22 {
                host.to_string()
            } else {
                format!("[{host}]:{port}")
            };
            kh.add(&host_id, key, "ssh-workspace", kind.into())
                .map_err(|e| format!("cannot store host key: {e}"))?;
            kh.write_file(known_hosts, KnownHostFileKind::OpenSSH)
                .map_err(|e| format!("cannot write {}: {e}", known_hosts.display()))?;
            Ok(Some(format!(
                "First connection: stored host key for {host_id} in {}",
                known_hosts.display()
            )))
        }
        CheckResult::Mismatch => Err(format!(
            "HOST KEY MISMATCH for {host}:{port}.\n\
             The server's key differs from the one in {}.\n\
             If the server was reinstalled/upgraded, use \"reset key\" on the host and reconnect.",
            known_hosts.display()
        )),
        CheckResult::Failure => Err("host key verification failed".into()),
    }
}

fn authenticate(
    sess: &mut Session,
    host: &Host,
    secret: Option<String>,
    app: Option<&AppHandle>,
) -> Result<(), String> {
    // Advertised methods decide whether a keyboard-interactive fallback
    // (OTP/2FA, PAM challenges) is even possible after primary auth fails.
    let methods = sess
        .auth_methods(&host.username)
        .map(str::to_string)
        .unwrap_or_default();
    // Secret entered via fallback prompt: re-store it in the keyring on
    // success so the next connect resolves the marker again.
    let mut heal: Option<(&'static str, String)> = None;
    let marker_lost = |v: &Option<String>| -> bool {
        v.as_deref()
            .is_some_and(|s| s.starts_with(crate::secrets::MARKER_PREFIX))
            && crate::secrets::resolve_opt(v).is_none()
    };
    let res = match &host.auth {
        AuthMethod::Password { password } => {
            let mut pw = secret.or_else(|| crate::secrets::resolve_opt(password));
            if pw.is_none() && marker_lost(password) {
                if let Some(app) = app {
                    pw = ui_prompt(
                        app,
                        &format!("Password for {}@{}", host.username, host.host),
                    );
                    if let Some(v) = pw.as_deref() {
                        heal = Some(("password", v.to_string()));
                    }
                }
            }
            pw.ok_or_else(|| {
                if marker_lost(password) {
                    "stored password missing from the OS keyring".to_string()
                } else {
                    "no password provided".to_string()
                }
            })
            .and_then(|pw| {
                sess.userauth_password(&host.username, &pw)
                    .map_err(|e| format!("password authentication failed: {e}"))
            })
        }
        AuthMethod::Key { path, passphrase } => {
            let key_path = expand_tilde(path);
            let mut pp = secret.or_else(|| crate::secrets::resolve_opt(passphrase));
            if pp.is_none() && marker_lost(passphrase) {
                if let Some(app) = app {
                    pp = ui_prompt(app, &format!("Passphrase for {}", key_path.display()));
                    if let Some(v) = pp.as_deref() {
                        heal = Some(("passphrase", v.to_string()));
                    }
                }
            }
            sess.userauth_pubkey_file(
                &host.username,
                None,
                &key_path,
                pp.as_deref(),
            )
            .map_err(|e| format!("key authentication failed ({}): {e}", key_path.display()))
        }
        AuthMethod::Agent => {
            let mut agent = sess
                .agent()
                .map_err(|e| format!("ssh-agent unavailable: {e}"))?;
            agent
                .connect()
                .map_err(|e| format!("cannot connect to ssh-agent: {e}"))?;
            agent
                .list_identities()
                .map_err(|e| format!("cannot list agent identities: {e}"))?;
            let identities = agent.identities().map_err(|e| e.to_string())?;
            let mut last_err = String::new();
            for identity in &identities {
                match agent.userauth(&host.username, identity) {
                    Ok(()) if sess.authenticated() => return Ok(()),
                    Ok(()) => {}
                    Err(e) => last_err = e.to_string(),
                }
            }
            Err(format!("agent authentication failed: {last_err}"))
        }
    };
    if res.is_ok() && sess.authenticated() {
        if let Some((field, v)) = heal {
            let _ = crate::secrets::store_secret(&host.id, field, &v);
        }
        return Ok(());
    }
    let primary_err = res.err();
    // Servers using OTP/2FA (or password-over-PAM) only offer
    // keyboard-interactive: bridge each prompt to the frontend modal.
    if methods.contains("keyboard-interactive") {
        if let Some(app) = app {
            let mut prompter = UiPrompter { app };
            return sess
                .userauth_keyboard_interactive(&host.username, &mut prompter)
                .map_err(|e| format!("interactive authentication failed: {e}"));
        }
    }
    match primary_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Remove known_hosts entries for a host. Uses `ssh-keygen -R` when available
/// because it also removes hashed entries and keeps a .old backup.
pub fn reset_host_key(known_hosts: &Path, host: &str, port: u16) -> Result<usize, String> {
    if !known_hosts.exists() {
        return Ok(0);
    }
    let patterns = if port == 22 {
        vec![host.to_string()]
    } else {
        vec![host.to_string(), format!("[{host}]:{port}")]
    };

    if which("ssh-keygen") {
        let mut removed = 0;
        for pat in &patterns {
            let status = Command::new("ssh-keygen")
                .args(["-R", pat, "-f"])
                .arg(known_hosts)
                .output();
            if let Ok(out) = status {
                // ssh-keygen -R prints "Host <pat> found: line N" per match.
                let stdout = String::from_utf8_lossy(&out.stdout);
                removed += stdout.lines().filter(|l| l.contains("found: line")).count();
            }
        }
        return Ok(removed);
    }

    // Fallback: filter out non-hashed matching lines ourselves.
    let content = std::fs::read_to_string(known_hosts).map_err(|e| e.to_string())?;
    let mut removed = 0usize;
    let kept: Vec<&str> = content
        .lines()
        .filter(|line| {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') || l.starts_with("|1|") {
                return true;
            }
            let hosts_field = l.split_whitespace().next().unwrap_or("");
            let matched = hosts_field
                .split(',')
                .any(|h| patterns.iter().any(|p| p == h));
            if matched {
                removed += 1;
            }
            !matched
        })
        .collect();
    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    std::fs::write(known_hosts, out).map_err(|e| e.to_string())?;
    Ok(removed)
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn kh_file(tag: &str, content: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("sshws-kh-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let kh = dir.join("known_hosts");
        fs::write(&kh, content).unwrap();
        (dir, kh)
    }

    #[test]
    fn reset_removes_host_keeps_others() {
        let (dir, kh) = kh_file(
            "basic",
            "# comment\nexample.com ssh-ed25519 AAAA\nother.com ssh-rsa BBBB\n[example.com]:2222 ssh-ed25519 CCCC\n",
        );
        let removed = reset_host_key(&kh, "example.com", 22).unwrap();
        assert!(removed >= 1);
        let content = fs::read_to_string(&kh).unwrap();
        assert!(!content.contains("example.com ssh-ed25519"));
        assert!(content.contains("other.com"));
        // [host]:port lines are a different pattern — untouched by port 22.
        assert!(content.contains("[example.com]:2222"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reset_removes_port_variant() {
        let (dir, kh) = kh_file(
            "port",
            "example.com ssh-ed25519 AAAA\n[example.com]:2222 ssh-ed25519 CCCC\n",
        );
        reset_host_key(&kh, "example.com", 2222).unwrap();
        let content = fs::read_to_string(&kh).unwrap();
        assert!(!content.contains("ssh-ed25519 AAAA"));
        assert!(!content.contains("[example.com]:2222"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reset_missing_file_is_zero() {
        assert_eq!(
            reset_host_key(Path::new("/nonexistent-sshws-kh"), "h", 22).unwrap(),
            0
        );
    }
}
