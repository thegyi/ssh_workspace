//! Secret storage in the OS keyring (macOS Keychain, Windows Credential
//! Manager, Linux Secret Service). hosts.json then stores only a
//! `keyring:<host>:<field>` marker. When no keyring backend is reachable the
//! plaintext value is kept (legacy behavior).

use crate::hosts::{AuthMethod, Host};
use keyring::Entry;

const SERVICE: &str = "dev.sshworkspace";
pub const MARKER_PREFIX: &str = "keyring:";

fn key(host_id: &str, field: &str) -> String {
    format!("{host_id}:{field}")
}

/// Store `value` in the keyring; returns the marker to persist, or None when
/// no keyring backend is available (caller keeps the plaintext).
fn store(host_id: &str, field: &str, value: &str) -> Option<String> {
    let k = key(host_id, field);
    let e = Entry::new(SERVICE, &k).ok()?;
    e.set_password(value).ok()?;
    Some(format!("{MARKER_PREFIX}{k}"))
}

/// Real secret behind a marker, if resolvable.
fn load(marker: &str) -> Option<String> {
    let k = marker.strip_prefix(MARKER_PREFIX)?;
    Entry::new(SERVICE, k).ok()?.get_password().ok()
}

/// Store one secret under `host_id:field`. Used to re-heal entries that
/// went missing from the keyring (e.g. locked/unavailable at save time).
pub fn store_secret(host_id: &str, field: &str, value: &str) -> Option<String> {
    store(host_id, field, value)
}

/// Resolve a stored secret field: marker → keyring value, plaintext → itself.
pub fn resolve_opt(v: &Option<String>) -> Option<String> {
    match v.as_deref() {
        Some(s) if s.starts_with(MARKER_PREFIX) => load(s),
        other => other.map(str::to_string),
    }
}

/// Move any plaintext secrets on `host` into the keyring (replacing them
/// with markers). No-op when the keyring is unavailable.
pub fn protect(host: &mut Host) {
    match &mut host.auth {
        AuthMethod::Password { password } => protect_field(&host.id, "password", password),
        AuthMethod::Key { passphrase, .. } => {
            protect_field(&host.id, "passphrase", passphrase)
        }
        AuthMethod::Agent => {}
    }
}

fn protect_field(host_id: &str, field: &str, slot: &mut Option<String>) {
    if let Some(v) = slot.as_deref() {
        if !v.is_empty() && !v.starts_with(MARKER_PREFIX) {
            if let Some(marker) = store(host_id, field, v) {
                *slot = Some(marker);
            }
        }
    }
}

/// Drop every keyring entry belonging to a host.
pub fn delete_host(host_id: &str) {
    for field in ["password", "passphrase"] {
        if let Ok(e) = Entry::new(SERVICE, &key(host_id, field)) {
            let _ = e.delete_credential();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_format_is_host_field() {
        assert_eq!(key("h1", "password"), "h1:password");
        assert_eq!(key("h1", "passphrase"), "h1:passphrase");
    }

    #[test]
    fn resolve_passes_plaintext_through() {
        assert_eq!(resolve_opt(&None), None);
        assert_eq!(
            resolve_opt(&Some("s3cret".into())),
            Some("s3cret".into())
        );
        // Empty string is a real value, not a marker.
        assert_eq!(resolve_opt(&Some(String::new())), Some(String::new()));
    }

    #[test]
    fn resolve_unresolvable_marker_is_none() {
        // No backend holds an entry for this fake id, so this is None whether
        // or not a keyring daemon is running on the test machine.
        let v = Some(format!("{MARKER_PREFIX}nonexistent-host-xyz:password"));
        assert_eq!(resolve_opt(&v), None);
    }

    #[test]
    fn protect_leaves_existing_markers_untouched() {
        // A value that is already a marker must not be re-stored/rewritten.
        let marker = format!("{MARKER_PREFIX}h1:password");
        let mut h = Host {
            id: "h1".into(),
            name: "x".into(),
            host: "x".into(),
            port: 22,
            username: "u".into(),
            auth: AuthMethod::Password {
                password: Some(marker.clone()),
            },
            x11: false,
            bookmarks: vec![],
            group: String::new(),
            jump: None,
            tunnels: vec![],
        };
        protect(&mut h);
        match h.auth {
            AuthMethod::Password { password } => assert_eq!(password, Some(marker)),
            _ => unreachable!(),
        }
    }
}
