use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

static ID_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthMethod {
    Password { password: Option<String> },
    Key { path: String, passphrase: Option<String> },
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TunnelKind {
    /// -L: local listener → remote target
    Local,
    /// -R: remote listener → local target
    Remote,
    /// -D: local SOCKS5 proxy
    Dynamic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tunnel {
    pub kind: TunnelKind,
    /// Address the listener binds to (local side for local/dynamic,
    /// remote side for remote).
    #[serde(default = "default_bind")]
    pub bind: String,
    pub listen_port: u16,
    /// Forward target for local/remote tunnels; unused for dynamic.
    #[serde(default)]
    pub target_host: String,
    #[serde(default)]
    pub target_port: u16,
}

fn default_bind() -> String {
    "127.0.0.1".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Host {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthMethod,
    #[serde(default)]
    pub x11: bool,
    #[serde(default)]
    pub bookmarks: Vec<String>,
    /// Sidebar grouping label; empty means ungrouped.
    #[serde(default)]
    pub group: String,
    /// Id of another saved host used as a ProxyJump/bastion.
    #[serde(default)]
    pub jump: Option<String>,
    #[serde(default)]
    pub tunnels: Vec<Tunnel>,
}

pub struct HostStore {
    path: PathBuf,
    hosts: Mutex<Vec<Host>>,
}

impl HostStore {
    pub fn load(path: PathBuf) -> Self {
        let hosts = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<Host>>(&s).ok())
            .unwrap_or_default();
        Self {
            path,
            hosts: Mutex::new(hosts),
        }
    }

    fn persist(&self, hosts: &[Host]) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(hosts).map_err(|e| e.to_string())?;
        fs::write(&self.path, json).map_err(|e| e.to_string())
    }

    pub fn list(&self) -> Vec<Host> {
        self.hosts.lock().unwrap().clone()
    }

    pub fn get(&self, id: &str) -> Option<Host> {
        self.hosts.lock().unwrap().iter().find(|h| h.id == id).cloned()
    }

    pub fn upsert(&self, mut host: Host) -> Result<Host, String> {
        let mut hosts = self.hosts.lock().unwrap();
        if host.id.is_empty() {
            host.id = format!(
                "h{:x}{:x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0),
                ID_COUNTER.fetch_add(1, Ordering::Relaxed)
            );
        } else if let Some(old) = hosts.iter().find(|h| h.id == host.id) {
            // A None secret means "keep the previously stored one" when the
            // auth kind did not change.
            host.auth = match (host.auth.clone(), &old.auth) {
                (AuthMethod::Password { password: None }, AuthMethod::Password { password }) => {
                    AuthMethod::Password {
                        password: password.clone(),
                    }
                }
                (
                    AuthMethod::Key {
                        path,
                        passphrase: None,
                    },
                    AuthMethod::Key { passphrase, .. },
                ) => AuthMethod::Key {
                    path,
                    passphrase: passphrase.clone(),
                },
                (new_auth, _) => new_auth,
            };
        }
        // Swap plaintext secrets for keyring markers when a backend exists.
        crate::secrets::protect(&mut host);
        hosts.retain(|h| h.id != host.id);
        hosts.push(host.clone());
        self.persist(&hosts)?;
        Ok(host)
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        let mut hosts = self.hosts.lock().unwrap();
        hosts.retain(|h| h.id != id);
        self.persist(&hosts)
    }

    /// Clone a host under a fresh id/name and persist it. Keyring markers
    /// are resolved to real secrets first so upsert re-stores them under
    /// the new id (deleting the original must not orphan the copy).
    pub fn duplicate(&self, id: &str) -> Result<Host, String> {
        let orig = self.get(id).ok_or("host not found")?;
        let mut copy = orig;
        copy.id = String::new();
        copy.name = format!("{} copy", copy.name);
        match &mut copy.auth {
            AuthMethod::Password { password } => {
                *password = crate::secrets::resolve_opt(password)
            }
            AuthMethod::Key { passphrase, .. } => {
                *passphrase = crate::secrets::resolve_opt(passphrase)
            }
            AuthMethod::Agent => {}
        }
        self.upsert(copy)
    }
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

/// One importable entry parsed from a `Host` block in `~/.ssh/config`.
/// Wildcard/negated patterns and Match blocks are skipped; global options
/// from `Host *` are intentionally not applied.
#[derive(Debug, Clone, Serialize)]
pub struct SshConfigEntry {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub identity_file: Option<String>,
}

pub fn parse_ssh_config(path: &Path) -> Vec<SshConfigEntry> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let default_user = std::env::var("USER").unwrap_or_default();
    let mut entries = Vec::new();
    let mut cur: Option<SshConfigEntry> = None;
    let mut skip = false; // inside a wildcard Host or Match block
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // OpenSSH accepts both "Key value" and "Key=value".
        let mut it = line.splitn(2, |c: char| c == ' ' || c == '\t' || c == '=');
        let kw = it.next().unwrap_or("").to_ascii_lowercase();
        let val = it
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .to_string();
        match kw.as_str() {
            "host" => {
                if let Some(e) = cur.take() {
                    entries.push(e);
                }
                let pats: Vec<&str> = val.split_whitespace().collect();
                skip = true;
                if pats.len() == 1 && !pats[0].contains(['*', '?', '!']) {
                    cur = Some(SshConfigEntry {
                        name: pats[0].to_string(),
                        host: pats[0].to_string(),
                        port: 22,
                        username: default_user.clone(),
                        identity_file: None,
                    });
                    skip = false;
                }
            }
            "match" => {
                if let Some(e) = cur.take() {
                    entries.push(e);
                }
                skip = true;
            }
            _ if skip => {}
            "hostname" => {
                if let Some(e) = cur.as_mut() {
                    if !val.is_empty() {
                        e.host = val;
                    }
                }
            }
            "port" => {
                if let Some(e) = cur.as_mut() {
                    if let Ok(p) = val.parse() {
                        e.port = p;
                    }
                }
            }
            "user" => {
                if let Some(e) = cur.as_mut() {
                    e.username = val;
                }
            }
            "identityfile" => {
                if let Some(e) = cur.as_mut() {
                    if e.identity_file.is_none() {
                        e.identity_file = Some(val);
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(e) = cur {
        entries.push(e);
    }
    entries
}
