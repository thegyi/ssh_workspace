use crate::hosts::{expand_tilde, AuthMethod, Host};
use ssh2::{CheckResult, KnownHostFileKind, Session};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

pub enum SshCommand {
    Data(Vec<u8>),
    Resize { cols: u32, rows: u32 },
    Disconnect,
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
/// Returns the session and an optional first-connection notice.
pub fn establish(
    host: &Host,
    secret: Option<String>,
    known_hosts: &Path,
) -> Result<(Session, Option<String>), String> {
    let addr_str = format!("{}:{}", host.host, host.port);
    let addr = addr_str
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {addr_str}: {e}"))?
        .next()
        .ok_or_else(|| format!("cannot resolve {addr_str}"))?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(10))
        .map_err(|e| format!("cannot connect to {addr_str}: {e}"))?;
    tcp.set_read_timeout(Some(Duration::from_secs(30))).ok();
    tcp.set_write_timeout(Some(Duration::from_secs(30))).ok();
    tcp.set_nodelay(true).ok();

    let mut sess = Session::new().map_err(|e| e.to_string())?;
    sess.set_tcp_stream(tcp);
    sess.handshake()
        .map_err(|e| format!("SSH handshake failed: {e}"))?;

    let notice = check_host_key(&sess, &host.host, host.port, known_hosts)?;

    authenticate(&mut sess, host, secret)?;
    if !sess.authenticated() {
        return Err("authentication failed".into());
    }
    // SSH-level keepalive (ignore messages) every 30s; want_reply=true so a
    // dead connection is detected instead of silently hanging.
    sess.set_keepalive(true, 30);
    Ok((sess, notice))
}

/// Connect to a host, open a shell with a PTY and spawn the I/O pump thread.
/// Returns the session id and an optional notice to display in the terminal.
pub fn connect(
    app: AppHandle,
    mgr: SshManager,
    host: Host,
    secret: Option<String>,
    cols: u32,
    rows: u32,
    known_hosts: &Path,
) -> Result<(String, Option<String>), String> {
    let (sess, notice) = establish(&host, secret.clone(), known_hosts)?;

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

    // X11 forwarding: second SSH session holding a remote port forward,
    // each inbound connection proxied to the local X server.
    #[cfg(unix)]
    if host.x11 {
        let (x_app, x_event, x_host, x_secret, x_kh, x_tx, x_alive) = (
            app.clone(),
            data_event.clone(),
            host.clone(),
            secret,
            known_hosts.to_path_buf(),
            tx,
            alive.clone(),
        );
        thread::spawn(move || {
            crate::x11::start(x_app, x_event, x_host, x_secret, x_kh, x_tx, x_alive);
        });
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
                let _ = app.emit(data_event, buf[..n].to_vec());
            }
            _ => {}
        }
        match channel.stderr().read(&mut buf) {
            Ok(n) if n > 0 => {
                got_data = true;
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

fn authenticate(sess: &mut Session, host: &Host, secret: Option<String>) -> Result<(), String> {
    match &host.auth {
        AuthMethod::Password { password } => {
            let pw = secret
                .or_else(|| password.clone())
                .ok_or_else(|| "no password provided".to_string())?;
            sess.userauth_password(&host.username, &pw)
                .map_err(|e| format!("password authentication failed: {e}"))
        }
        AuthMethod::Key { path, passphrase } => {
            let key_path = expand_tilde(path);
            let pp = secret.or_else(|| passphrase.clone());
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
