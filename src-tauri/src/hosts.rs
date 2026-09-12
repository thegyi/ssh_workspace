use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
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
pub struct Host {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthMethod,
    #[serde(default)]
    pub x11: bool,
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
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}
