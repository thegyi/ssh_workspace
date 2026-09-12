use crate::hosts::{expand_tilde, Host};
use crate::ssh;
use serde::Serialize;
use ssh2::Sftp;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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
    Transfer {
        id: String,
        upload: bool,
        src: String,
        dst: String,
    },
    Disconnect,
}

struct SftpEntry {
    tx: Sender<SftpMsg>,
    host_id: String,
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
        self.inner.sessions.lock().unwrap().remove(id);
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

    pub fn transfer(&self, id: &str, op: String, upload: bool, src: String, dst: String) -> Result<(), String> {
        self.send(id, SftpMsg::Transfer { id: op, upload, src, dst })
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
    known_hosts: &Path,
) -> Result<(String, Option<String>, String), String> {
    let (sess, notice) = ssh::establish(&host, secret, known_hosts)?;
    let sftp = sess
        .sftp()
        .map_err(|e| format!("sftp subsystem failed: {e}"))?;
    let home = sftp
        .realpath(Path::new("."))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".into());

    let session_id = format!("f{}", SESSION_COUNTER.fetch_add(1, Ordering::Relaxed));
    let (tx, rx) = mpsc::channel::<SftpMsg>();
    mgr.insert(
        &session_id,
        SftpEntry {
            tx,
            host_id: host.id.clone(),
        },
    );

    let sid = session_id.clone();
    let thread_mgr = mgr.clone();
    let sess_ka = sess.clone();
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
                SftpMsg::Transfer { id, upload, src, dst } => {
                    let result = if upload {
                        upload_path(&sftp, &app, &sid, &id, Path::new(&src), Path::new(&dst))
                    } else {
                        download_path(&sftp, &app, &sid, &id, Path::new(&src), Path::new(&dst))
                    };
                    let _ = app.emit(
                        &format!("sftp-done-{sid}"),
                        TransferDone {
                            id,
                            ok: result.is_ok(),
                            error: result.err(),
                        },
                    );
                }
                SftpMsg::Disconnect => break,
            }
        }
        thread_mgr.remove(&sid);
    });

    Ok((session_id, notice, home))
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
        });
    }
    sort_entries(&mut entries);
    Ok(ListResult {
        path: real.display().to_string(),
        entries,
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
) -> Result<(), String> {
    let meta = fs::metadata(src).map_err(|e| format!("{}: {e}", src.display()))?;
    if meta.is_dir() {
        let _ = sftp.mkdir(dst, 0o755);
        for e in fs::read_dir(src).map_err(|e| e.to_string())? {
            let e = e.map_err(|e| e.to_string())?;
            upload_path(sftp, app, sid, op, &e.path(), &dst.join(e.file_name()))?;
        }
        Ok(())
    } else {
        let mut local = fs::File::open(src).map_err(|e| e.to_string())?;
        let mut remote = sftp
            .create(dst)
            .map_err(|e| format!("cannot create {}: {e}", dst.display()))?;
        copy_progress(&mut local, &mut remote, meta.len(), app, sid, op, &src.display().to_string())
    }
}

fn download_path(
    sftp: &Sftp,
    app: &AppHandle,
    sid: &str,
    op: &str,
    src: &Path,
    dst: &Path,
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
            download_path(sftp, app, sid, op, &src.join(&name), &dst.join(&name))?;
        }
        Ok(())
    } else {
        let mut remote = sftp
            .open(src)
            .map_err(|e| format!("cannot open {}: {e}", src.display()))?;
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).ok();
        }
        let mut local = fs::File::create(dst).map_err(|e| e.to_string())?;
        copy_progress(
            &mut remote,
            &mut local,
            stat.size.unwrap_or(0),
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
    app: &AppHandle,
    sid: &str,
    op: &str,
    file: &str,
) -> Result<(), String> {
    let event = format!("sftp-progress-{sid}");
    let mut buf = [0u8; 65536];
    let mut done = 0u64;
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
