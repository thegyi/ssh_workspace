use crate::hosts::{expand_tilde, Host};
use crate::ssh;
use serde::Serialize;
use ssh2::Sftp;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: u64,
    pub perm: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct ListResult {
    pub path: String,
    pub entries: Vec<FileEntry>,
}

#[derive(Clone, Serialize)]
struct Progress {
    id: String,
    file: String,
    done: u64,
    total: u64,
}

#[derive(Clone, Serialize)]
struct TransferDone {
    id: String,
    ok: bool,
    error: Option<String>,
}

enum SftpMsg {
    List {
        path: String,
        reply: Sender<Result<ListResult, String>>,
    },
    Mkdir {
        path: String,
        reply: Sender<Result<(), String>>,
    },
    Delete {
        path: String,
        reply: Sender<Result<(), String>>,
    },
    Rename {
        old: String,
        new: String,
        reply: Sender<Result<(), String>>,
    },
    Chmod {
        path: String,
        mode: u32,
        reply: Sender<Result<(), String>>,
    },
    /// Download `remote` to `local` (no progress events) and reply when done.
    EditOpen {
        remote: String,
        local: String,
        reply: Sender<Result<(), String>>,
    },
    Transfer {
        id: String,
        upload: bool,
        src: String,
        dst: String,
        /// Continue a partial file: append at the existing destination size.
        resume: bool,
    },
    Disconnect,
}

struct SftpEntry {
    tx: Sender<SftpMsg>,
    host_id: String,
    /// Cancel flags for edit-watch threads; flipped when the session ends.
    watches: Mutex<Vec<Arc<AtomicBool>>>,
}

#[derive(Clone)]
pub struct ConnParams {
    pub host: Host,
    pub secret: Option<String>,
    pub jump: Option<(Host, Option<String>)>,
    pub known_hosts: PathBuf,
}

/// Caps concurrent transfer sessions so a big drop does not trip the
/// server's MaxStartups/MaxSessions limits.
struct Permit {
    count: Mutex<usize>,
    free: std::sync::Condvar,
}

impl Permit {
    const MAX: usize = 4;
    fn new() -> Self {
        Self {
            count: Mutex::new(0),
            free: std::sync::Condvar::new(),
        }
    }
    fn acquire(self: &Arc<Self>) -> PermitGuard {
        let mut c = self.count.lock().unwrap();
        while *c >= Self::MAX {
            c = self.free.wait(c).unwrap();
        }
        *c += 1;
        PermitGuard(self.clone())
    }
}

struct PermitGuard(Arc<Permit>);
impl Drop for PermitGuard {
    fn drop(&mut self) {
        *self.0.count.lock().unwrap() -= 1;
        self.0.free.notify_one();
    }
}

struct ManagerInner {
    sessions: Mutex<HashMap<String, SftpEntry>>,
}

#[derive(Clone)]
pub struct SftpManager {
    inner: Arc<ManagerInner>,
}

impl SftpManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ManagerInner {
                sessions: Mutex::new(HashMap::new()),
            }),
        }
    }

    fn insert(&self, id: &str, entry: SftpEntry) {
        self.inner
            .sessions
            .lock()
            .unwrap()
            .insert(id.to_string(), entry);
    }

    fn remove(&self, id: &str) {
        if let Some(e) = self.inner.sessions.lock().unwrap().remove(id) {
            for f in e.watches.lock().unwrap().iter() {
                f.store(true, Ordering::Relaxed);
            }
        }
    }

    fn send(&self, id: &str, msg: SftpMsg) -> Result<(), String> {
        let sessions = self.inner.sessions.lock().unwrap();
        match sessions.get(id) {
            Some(e) => e.tx.send(msg).map_err(|e| e.to_string()),
            None => Err("sftp session not found".into()),
        }
    }

    /// Blocking directory listing on the session's worker thread.
    pub fn list(&self, id: &str, path: &str) -> Result<ListResult, String> {
        let (tx, rx) = mpsc::channel();
        self.send(
            id,
            SftpMsg::List {
                path: path.to_string(),
                reply: tx,
            },
        )?;
        rx.recv().map_err(|e| e.to_string())?
    }

    /// Blocking mkdir on the session's worker thread.
    pub fn mkdir(&self, id: &str, path: &str) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.send(
            id,
            SftpMsg::Mkdir {
                path: path.to_string(),
                reply: tx,
            },
        )?;
        rx.recv().map_err(|e| e.to_string())?
    }

    /// Blocking delete (recursive for dirs) on the session's worker thread.
    pub fn delete(&self, id: &str, path: &str) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.send(
            id,
            SftpMsg::Delete {
                path: path.to_string(),
                reply: tx,
            },
        )?;
        rx.recv().map_err(|e| e.to_string())?
    }

    pub fn transfer(
        &self,
        id: &str,
        op: String,
        upload: bool,
        src: String,
        dst: String,
        resume: bool,
    ) -> Result<(), String> {
        self.send(
            id,
            SftpMsg::Transfer {
                id: op,
                upload,
                src,
                dst,
                resume,
            },
        )
    }

    /// Blocking rename on the session's worker thread (fails if target exists).
    pub fn rename(&self, id: &str, old: &str, new: &str) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.send(
            id,
            SftpMsg::Rename {
                old: old.to_string(),
                new: new.to_string(),
                reply: tx,
            },
        )?;
        rx.recv().map_err(|e| e.to_string())?
    }

    /// Blocking chmod on the session's worker thread.
    pub fn chmod(&self, id: &str, path: &str, mode: u32) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.send(
            id,
            SftpMsg::Chmod {
                path: path.to_string(),
                mode,
                reply: tx,
            },
        )?;
        rx.recv().map_err(|e| e.to_string())?
    }

    /// Download a remote file into the local cache and watch it: whenever the
    /// local mtime changes, the file is uploaded back. Returns the local path.
    pub fn edit_open(
        &self,
        app: AppHandle,
        id: &str,
        remote: &str,
    ) -> Result<String, String> {
        let name = Path::new(remote)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".into());
        let local = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("ssh-workspace/edit")
            .join(id)
            .join(&name);
        let (tx, rx) = mpsc::channel();
        self.send(
            id,
            SftpMsg::EditOpen {
                remote: remote.to_string(),
                local: local.display().to_string(),
                reply: tx,
            },
        )?;
        rx.recv().map_err(|e| e.to_string())??;

        let local_str = local.display().to_string();
        let cancel = Arc::new(AtomicBool::new(false));
        if let Some(e) = self.inner.sessions.lock().unwrap().get(id) {
            e.watches.lock().unwrap().push(cancel.clone());
        }
        thread::spawn({
            let mgr = self.clone();
            let sid = id.to_string();
            let remote = remote.to_string();
            move || watch_edit(mgr, app, sid, local, remote, cancel)
        });
        Ok(local_str)
    }

    pub fn close(&self, id: &str) {
        let _ = self.send(id, SftpMsg::Disconnect);
    }

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
            let _ = self.send(&id, SftpMsg::Disconnect);
        }
    }

    /// Disconnect all SFTP sessions (see SshManager::disconnect_all).
    pub fn disconnect_all(&self) {
        let ids: Vec<String> = self.inner.sessions.lock().unwrap().keys().cloned().collect();
        for id in ids {
            let _ = self.send(&id, SftpMsg::Disconnect);
        }
    }
}

/// Open an SSH session and its SFTP subsystem on a dedicated worker thread.
/// Returns (session_id, first-connection notice, remote home directory).
pub fn open(
    app: AppHandle,
    mgr: SftpManager,
    host: Host,
    secret: Option<String>,
    jump: Option<(Host, Option<String>)>,
    known_hosts: &Path,
) -> Result<(String, Option<String>, String), String> {
    let (sess, notice) = ssh::establish(
        &host,
        secret.clone(),
        jump.as_ref().map(|(h, s)| (h, s.clone())),
        Some(&app),
        known_hosts,
    )?;
    let sftp = sess
        .sftp()
        .map_err(|e| format!("sftp subsystem failed: {e}"))?;
    let home = sftp
        .realpath(Path::new("."))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".into());

    let session_id = format!("f{}", SESSION_COUNTER.fetch_add(1, Ordering::Relaxed));
    let (tx, rx) = mpsc::channel::<SftpMsg>();
    // Connection params for spawning per-transfer sessions (parallel
    // transfers). Channels can't multiplex across threads in ssh2, so
    // each transfer runs on its own authenticated session.
    let thread_conn = ConnParams {
        host: host.clone(),
        secret,
        jump,
        known_hosts: known_hosts.to_path_buf(),
    };
    mgr.insert(
        &session_id,
        SftpEntry {
            tx,
            host_id: host.id.clone(),
            watches: Mutex::new(Vec::new()),
        },
    );

    let sid = session_id.clone();
    let thread_mgr = mgr.clone();
    let sess_ka = sess.clone();
    let permits = Arc::new(Permit::new());
    thread::spawn(move || {
        loop {
            let msg = match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => {
                    // Idle wake-up: send the SSH keepalive configured in establish().
                    if let Err(e) = sess_ka.keepalive_send().map_err(std::io::Error::from) {
                        if e.kind() != std::io::ErrorKind::WouldBlock {
                            break; // connection is dead
                        }
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            };
            match msg {
                SftpMsg::List { path, reply } => {
                    let _ = reply.send(list_remote(&sftp, &path));
                }
                SftpMsg::Mkdir { path, reply } => {
                    let _ = reply.send(
                        sftp.mkdir(Path::new(&path), 0o755)
                            .map_err(|e| format!("cannot create {path}: {e}")),
                    );
                }
                SftpMsg::Delete { path, reply } => {
                    let _ = reply.send(delete_remote(&sftp, Path::new(&path)));
                }
                SftpMsg::Rename { old, new, reply } => {
                    let _ = reply.send(
                        sftp
                            .rename(Path::new(&old), Path::new(&new), None)
                            .map_err(|e| format!("cannot rename {old}: {e}")),
                    );
                }
                SftpMsg::Chmod { path, mode, reply } => {
                    let _ = reply.send(chmod_remote(&sftp, Path::new(&path), mode));
                }
                SftpMsg::EditOpen {
                    remote,
                    local,
                    reply,
                } => {
                    let _ = reply.send(download_file(&sftp, Path::new(&remote), Path::new(&local)));
                }
                SftpMsg::Transfer {
                    id,
                    upload,
                    src,
                    dst,
                    resume,
                } => {
                    // Parallel transfers: run on a dedicated SSH session so
                    // directory ops on this worker stay responsive and drops
                    // overlap. Capped by `permits` to respect MaxStartups.
                    let (c_app, c_sid, c_permits, c_conn) = (
                        app.clone(),
                        sid.clone(),
                        permits.clone(),
                        thread_conn.clone(),
                    );
                    thread::spawn(move || {
                        let _permit = c_permits.acquire();
                        let result = transfer_sftp(&c_conn, &c_app).and_then(|sftp| {
                                if upload {
                                    upload_path(&sftp, &c_app, &c_sid, &id, Path::new(&src), Path::new(&dst), resume)
                                } else {
                                    download_path(&sftp, &c_app, &c_sid, &id, Path::new(&src), Path::new(&dst), resume)
                                }
                            },
                        );
                        let _ = c_app.emit(
                            &format!("sftp-done-{c_sid}"),
                            TransferDone {
                                id,
                                ok: result.is_ok(),
                                error: result.err(),
                            },
                        );
                    });
                }
                SftpMsg::Disconnect => break,
            }
        }
        thread_mgr.remove(&sid);
    });

    Ok((session_id, notice, home))
}

/// Fresh SSH session + SFTP subsystem for one transfer op (parallel
/// transfers run off the main session worker).
fn transfer_sftp(conn: &ConnParams, app: &AppHandle) -> Result<Sftp, String> {
    let (sess, _) = ssh::establish(
        &conn.host,
        conn.secret.clone(),
        conn.jump.as_ref().map(|(h, s)| (h, s.clone())),
        Some(app),
        &conn.known_hosts,
    )?;
    sess.sftp()
        .map_err(|e| format!("sftp subsystem failed: {e}"))
}

fn list_remote(sftp: &Sftp, path: &str) -> Result<ListResult, String> {
    let p = if path.is_empty() {
        Path::new(".")
    } else {
        Path::new(path)
    };
    let real = sftp.realpath(p).unwrap_or_else(|_| p.to_path_buf());
    let rd = sftp
        .readdir(&real)
        .map_err(|e| format!("cannot list {}: {e}", real.display()))?;
    let mut entries = Vec::new();
    for (entry_path, stat) in rd {
        let name = entry_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        entries.push(FileEntry {
            path: real.join(&name).display().to_string(),
            name,
            is_dir: stat.is_dir(),
            size: stat.size.unwrap_or(0),
            mtime: stat.mtime.unwrap_or(0),
            perm: stat.perm,
            uid: stat.uid,
            gid: stat.gid,
        });
    }
    sort_entries(&mut entries);
    Ok(ListResult {
        path: real.display().to_string(),
        entries,
    })
}

/// Local filesystem listing for the right-hand pane.
pub fn local_list(path: &str) -> Result<ListResult, String> {
    let p = if path.is_empty() {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
    } else {
        expand_tilde(path)
    };
    let real = p.canonicalize().unwrap_or(p);
    let mut entries = Vec::new();
    for e in fs::read_dir(&real).map_err(|e| format!("cannot list {}: {e}", real.display()))? {
        let e = e.map_err(|e| e.to_string())?;
        let meta = e.metadata().map_err(|e| e.to_string())?;
        #[cfg(unix)]
        let (perm, uid, gid) = {
            use std::os::unix::fs::MetadataExt;
            (Some(meta.mode() & 0o7777), Some(meta.uid()), Some(meta.gid()))
        };
        #[cfg(not(unix))]
        let (perm, uid, gid) = (None, None, None);
        entries.push(FileEntry {
            name: e.file_name().to_string_lossy().to_string(),
            path: e.path().display().to_string(),
            is_dir: meta.is_dir(),
            size: meta.len(),
            mtime: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0),
            perm,
            uid,
            gid,
        });
    }
    sort_entries(&mut entries);
    Ok(ListResult {
        path: real.display().to_string(),
        entries,
    })
}

/// Stat a single local path (used to resolve OS drag-and-drop file lists,
/// which arrive as bare paths without metadata).
pub fn local_stat(path: &str) -> Result<FileEntry, String> {
    let p = expand_tilde(path);
    let meta =
        fs::metadata(&p).map_err(|e| format!("cannot stat {}: {e}", p.display()))?;
    #[cfg(unix)]
    let (perm, uid, gid) = {
        use std::os::unix::fs::MetadataExt;
        (Some(meta.mode() & 0o7777), Some(meta.uid()), Some(meta.gid()))
    };
    #[cfg(not(unix))]
    let (perm, uid, gid) = (None, None, None);
    Ok(FileEntry {
        name: p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| p.display().to_string()),
        path: p.display().to_string(),
        is_dir: meta.is_dir(),
        size: meta.len(),
        mtime: meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0),
        perm,
        uid,
        gid,
    })
}

/// Recursively delete a remote path. Uses lstat so a symlinked dir is
/// unlinked rather than traversed into.
fn delete_remote(sftp: &Sftp, path: &Path) -> Result<(), String> {
    let stat = sftp
        .lstat(path)
        .map_err(|e| format!("cannot stat {}: {e}", path.display()))?;
    if stat.is_dir() {
        for (entry_path, _) in sftp
            .readdir(path)
            .map_err(|e| format!("cannot list {}: {e}", path.display()))?
        {
            let name = match entry_path.file_name() {
                Some(n) => n.to_string_lossy().to_string(),
                None => continue,
            };
            delete_remote(sftp, &path.join(&name))?;
        }
        sftp.rmdir(path)
            .map_err(|e| format!("cannot remove dir {}: {e}", path.display()))
    } else {
        sftp.unlink(path)
            .map_err(|e| format!("cannot delete {}: {e}", path.display()))
    }
}

/// Delete a local file or directory (recursive for dirs).
pub fn local_delete(path: &str) -> Result<(), String> {
    let p = expand_tilde(path);
    let meta =
        fs::symlink_metadata(&p).map_err(|e| format!("cannot stat {}: {e}", p.display()))?;
    if meta.is_dir() {
        fs::remove_dir_all(&p).map_err(|e| format!("cannot remove dir {}: {e}", p.display()))
    } else {
        fs::remove_file(&p).map_err(|e| format!("cannot delete {}: {e}", p.display()))
    }
}

/// Create a local directory (recursive, like `mkdir -p`).
pub fn local_mkdir(path: &str) -> Result<(), String> {
    fs::create_dir_all(expand_tilde(path)).map_err(|e| format!("cannot create {path}: {e}"))
}

fn sort_entries(entries: &mut [FileEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

fn upload_path(
    sftp: &Sftp,
    app: &AppHandle,
    sid: &str,
    op: &str,
    src: &Path,
    dst: &Path,
    resume: bool,
) -> Result<(), String> {
    let meta = fs::metadata(src).map_err(|e| format!("{}: {e}", src.display()))?;
    if meta.is_dir() {
        let _ = sftp.mkdir(dst, 0o755);
        for e in fs::read_dir(src).map_err(|e| e.to_string())? {
            let e = e.map_err(|e| e.to_string())?;
            upload_path(sftp, app, sid, op, &e.path(), &dst.join(e.file_name()), resume)?;
        }
        Ok(())
    } else {
        let mut local = fs::File::open(src).map_err(|e| e.to_string())?;
        let mut offset = 0u64;
        let mut remote = if resume {
            // Append after the existing partial remote file.
            let have = sftp.stat(dst).ok().and_then(|s| s.size).unwrap_or(0);
            if have > 0 && have < meta.len() {
                local
                    .seek(std::io::SeekFrom::Start(have))
                    .map_err(|e| e.to_string())?;
                offset = have;
                sftp.open_mode(
                    dst,
                    ssh2::OpenFlags::WRITE | ssh2::OpenFlags::APPEND,
                    0o644,
                    ssh2::OpenType::File,
                )
                .map_err(|e| format!("cannot resume {}: {e}", dst.display()))?
            } else {
                sftp
                    .create(dst)
                    .map_err(|e| format!("cannot create {}: {e}", dst.display()))?
            }
        } else {
            sftp
                .create(dst)
                .map_err(|e| format!("cannot create {}: {e}", dst.display()))?
        };
        copy_progress(
            &mut local,
            &mut remote,
            meta.len(),
            offset,
            app,
            sid,
            op,
            &src.display().to_string(),
        )
    }
}

fn download_path(
    sftp: &Sftp,
    app: &AppHandle,
    sid: &str,
    op: &str,
    src: &Path,
    dst: &Path,
    resume: bool,
) -> Result<(), String> {
    let stat = sftp
        .stat(src)
        .map_err(|e| format!("cannot stat {}: {e}", src.display()))?;
    if stat.is_dir() {
        fs::create_dir_all(dst).map_err(|e| e.to_string())?;
        for (entry_path, _) in sftp
            .readdir(src)
            .map_err(|e| format!("cannot list {}: {e}", src.display()))?
        {
            let name = match entry_path.file_name() {
                Some(n) => n.to_string_lossy().to_string(),
                None => continue,
            };
            download_path(sftp, app, sid, op, &src.join(&name), &dst.join(&name), resume)?;
        }
        Ok(())
    } else {
        let total = stat.size.unwrap_or(0);
        let mut offset = 0u64;
        let mut remote = sftp
            .open(src)
            .map_err(|e| format!("cannot open {}: {e}", src.display()))?;
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).ok();
        }
        // Resume: keep the existing partial local file and append to it.
        let have = if resume {
            fs::metadata(dst).map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };
        let mut local = if have > 0 && have < total {
            remote
                .seek(std::io::SeekFrom::Start(have))
                .map_err(|e| e.to_string())?;
            offset = have;
            fs::OpenOptions::new()
                .append(true)
                .open(dst)
                .map_err(|e| e.to_string())?
        } else {
            fs::File::create(dst).map_err(|e| e.to_string())?
        };
        copy_progress(
            &mut remote,
            &mut local,
            total,
            offset,
            app,
            sid,
            op,
            &src.display().to_string(),
        )
    }
}

fn copy_progress(
    reader: &mut impl Read,
    writer: &mut impl Write,
    total: u64,
    offset: u64,
    app: &AppHandle,
    sid: &str,
    op: &str,
    file: &str,
) -> Result<(), String> {
    let event = format!("sftp-progress-{sid}");
    let mut buf = [0u8; 65536];
    let mut done = offset;
    let mut last = Instant::now();
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        if last.elapsed() >= Duration::from_millis(150) {
            last = Instant::now();
            let _ = app.emit(
                &event,
                Progress {
                    id: op.into(),
                    file: file.into(),
                    done,
                    total,
                },
            );
        }
    }
    let _ = app.emit(
        &event,
        Progress {
            id: op.into(),
            file: file.into(),
            done,
            total,
        },
    );
    Ok(())
}

/// Remote chmod via lstat + setstat (preserves all other attributes).
fn chmod_remote(sftp: &Sftp, path: &Path, mode: u32) -> Result<(), String> {
    let mut st = sftp
        .lstat(path)
        .map_err(|e| format!("cannot stat {}: {e}", path.display()))?;
    st.perm = Some(mode);
    sftp.setstat(path, st)
        .map_err(|e| format!("cannot chmod {}: {e}", path.display()))
}

/// Plain remote→local file copy without progress events (used by edit-open).
fn download_file(sftp: &Sftp, src: &Path, dst: &Path) -> Result<(), String> {
    let mut r = sftp
        .open(src)
        .map_err(|e| format!("cannot open {}: {e}", src.display()))?;
    if let Some(p) = dst.parent() {
        fs::create_dir_all(p).ok();
    }
    let mut w = fs::File::create(dst).map_err(|e| e.to_string())?;
    std::io::copy(&mut r, &mut w).map_err(|e| e.to_string())?;
    Ok(())
}

fn file_mtime(p: &Path) -> Option<u64> {
    fs::metadata(p)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

static WATCH_SEQ: AtomicU64 = AtomicU64::new(1);

/// Poll a locally-edited file; when its mtime changes, queue an upload back
/// to the remote path on the owning session. Ends on cancel or if the local
/// file disappears.
fn watch_edit(
    mgr: SftpManager,
    app: AppHandle,
    sid: String,
    local: PathBuf,
    remote: String,
    cancel: Arc<AtomicBool>,
) {
    let mut last = file_mtime(&local);
    while !cancel.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(1500));
        let now = file_mtime(&local);
        match (last, now) {
            (Some(a), Some(b)) if b > a => {
                last = now;
                let op = format!("w{}", WATCH_SEQ.fetch_add(1, Ordering::Relaxed));
                if mgr
                    .send(
                        &sid,
                        SftpMsg::Transfer {
                            id: op,
                            upload: true,
                            src: local.display().to_string(),
                            dst: remote.clone(),
                            resume: false,
                        },
                    )
                    .is_err()
                {
                    break; // session is gone
                }
                let _ = app.emit(
                    &format!("sftp-edit-synced-{sid}"),
                    local.display().to_string(),
                );
            }
            (_, None) => break,
            _ => {}
        }
    }
}

/// Rename a local file or directory.
pub fn local_rename(path: &str, new_path: &str) -> Result<(), String> {
    fs::rename(expand_tilde(path), expand_tilde(new_path))
        .map_err(|e| format!("cannot rename {path}: {e}"))
}

/// Change permissions on a local file or directory (octal mode).
pub fn local_chmod(path: &str, mode: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return fs::set_permissions(expand_tilde(path), fs::Permissions::from_mode(mode))
            .map_err(|e| format!("cannot chmod {path}: {e}"));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Err("chmod is not supported on this platform".into())
    }
}

/// Open a local path with the desktop's default application.
pub fn local_open(path: &str) -> Result<(), String> {
    let p = expand_tilde(path);
    open_cmd(&p).map_err(|e| format!("cannot open {}: {e}", p.display()))
}

fn open_cmd(p: &Path) -> std::io::Result<()> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(p).spawn().map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(p).spawn().map(|_| ())
    }
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(p)
            .spawn()
            .map(|_| ())
    }
}
