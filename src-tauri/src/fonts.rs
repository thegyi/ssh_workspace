//! System font enumeration for the terminal font picker.
//! Result is cached — font lists don't change within an app run.

use std::collections::BTreeSet;
use std::process::Command;
use std::sync::OnceLock;

pub fn list() -> Vec<String> {
    static FONTS: OnceLock<Vec<String>> = OnceLock::new();
    FONTS.get_or_init(detect).clone()
}

fn sorted(set: BTreeSet<String>) -> Vec<String> {
    set.into_iter().collect()
}

/// Linux/BSD: fontconfig is guaranteed by the GTK dependency.
/// Prefer monospace families; fall back to all families.
#[cfg(all(unix, not(target_os = "macos")))]
fn detect() -> Vec<String> {
    for args in [":spacing=mono", ":"] {
        let Ok(o) = Command::new("fc-list").arg(args).arg("family").output() else {
            continue;
        };
        if !o.status.success() {
            continue;
        }
        let fonts: BTreeSet<String> = String::from_utf8_lossy(&o.stdout)
            .lines()
            .flat_map(|l| l.split(','))
            .map(|f| f.trim().to_string())
            .filter(|f| !f.is_empty())
            .collect();
        if !fonts.is_empty() {
            return sorted(fonts);
        }
    }
    Vec::new()
}

/// macOS: system_profiler lists every installed font family.
#[cfg(target_os = "macos")]
fn detect() -> Vec<String> {
    let Ok(o) = Command::new("system_profiler")
        .arg("SPFontsDataType")
        .output()
    else {
        return Vec::new();
    };
    let fonts: BTreeSet<String> = String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            l.strip_prefix("Family:").map(|f| f.trim().to_string())
        })
        .filter(|f| !f.is_empty())
        .collect();
    sorted(fonts)
}

/// Windows: font families live in the registry.
#[cfg(windows)]
fn detect() -> Vec<String> {
    let Ok(o) = Command::new("reg")
        .args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts",
        ])
        .output()
    else {
        return Vec::new();
    };
    let fonts: BTreeSet<String> = String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| {
            let name = l.trim().split("    REG_").next()?.trim();
            let name = name
                .strip_suffix(" (TrueType)")
                .or_else(|| name.strip_suffix(" (OpenType)"))
                .unwrap_or(name);
            if name.is_empty() || name.starts_with("HK") {
                None
            } else {
                Some(name.to_string())
            }
        })
        .collect();
    sorted(fonts)
}
