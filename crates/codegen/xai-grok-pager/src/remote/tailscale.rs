//! Tailscale detection and address discovery for `/remote`.

use std::path::PathBuf;
use std::process::Command;

/// Outcome of probing the local Tailscale installation / daemon.
#[derive(Debug, Clone)]
pub enum TailscaleStatus {
    /// `tailscale` binary not found on PATH (or common install locations).
    NotInstalled { install_hint: String },
    /// Binary found but daemon is not running / not logged in.
    NotRunning { binary: PathBuf, hint: String },
    /// Ready: we have at least one Tailscale IP (and optional MagicDNS name).
    Ready(TailscaleInfo),
}

/// Addresses usable to reach this host over the tailnet.
#[derive(Debug, Clone)]
pub struct TailscaleInfo {
    pub binary: PathBuf,
    /// Preferred IPv4 (or first IP) on the tailnet.
    pub ip: String,
    /// Optional MagicDNS hostname (e.g. `macbook.tail-scale.ts.net`).
    pub dns_name: Option<String>,
    /// Backend state string from `tailscale status --json` when available.
    pub backend_state: Option<String>,
}

/// Probe Tailscale: installed? running? what IP/DNS?
pub fn probe() -> TailscaleStatus {
    let Some(binary) = find_tailscale_binary() else {
        return TailscaleStatus::NotInstalled {
            install_hint: install_instructions(),
        };
    };

    if let Some(info) = try_status_json(&binary) {
        return TailscaleStatus::Ready(info);
    }

    // Fallback: `tailscale ip -4`
    if let Some(ip) = try_ip_cmd(&binary) {
        return TailscaleStatus::Ready(TailscaleInfo {
            binary,
            ip,
            dns_name: None,
            backend_state: None,
        });
    }

    TailscaleStatus::NotRunning {
        binary,
        hint: not_running_hint(),
    }
}

fn find_tailscale_binary() -> Option<PathBuf> {
    if let Ok(path) = which("tailscale") {
        return Some(path);
    }
    // Common absolute locations (macOS app + Linux packages).
    const CANDIDATES: &[&str] = &[
        "/usr/local/bin/tailscale",
        "/opt/homebrew/bin/tailscale",
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
        "/usr/bin/tailscale",
        "/usr/sbin/tailscale",
    ];
    for c in CANDIDATES {
        let p = PathBuf::from(c);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn which(name: &str) -> Result<PathBuf, ()> {
    let output = Command::new("which").arg(name).output().map_err(|_| ())?;
    if !output.status.success() {
        return Err(());
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return Err(());
    }
    Ok(PathBuf::from(path))
}

fn try_status_json(binary: &PathBuf) -> Option<TailscaleInfo> {
    let output = Command::new(binary)
        .args(["status", "--json"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let backend = v
        .get("BackendState")
        .and_then(|s| s.as_str())
        .map(str::to_string);

    // Prefer Self.TailscaleIPs; fall back to top-level TailscaleIPs.
    let ips = v
        .pointer("/Self/TailscaleIPs")
        .or_else(|| v.get("TailscaleIPs"))
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();

    let mut ip_v4 = None;
    let mut ip_any = None;
    for ip in ips {
        let Some(s) = ip.as_str() else { continue };
        if s.contains(':') {
            ip_any.get_or_insert_with(|| s.to_string());
        } else {
            ip_v4.get_or_insert_with(|| s.to_string());
            break;
        }
    }
    let ip = ip_v4.or(ip_any)?;

    let dns_name = v
        .pointer("/Self/DNSName")
        .and_then(|s| s.as_str())
        .map(|s| s.trim_end_matches('.').to_string())
        .filter(|s| !s.is_empty());

    // Running but not connected yet.
    if let Some(ref state) = backend {
        let lower = state.to_ascii_lowercase();
        if lower == "stopped" || lower == "needslogin" || lower == "nowindows" {
            return None;
        }
    }

    Some(TailscaleInfo {
        binary: binary.clone(),
        ip,
        dns_name,
        backend_state: backend,
    })
}

fn try_ip_cmd(binary: &PathBuf) -> Option<String> {
    let output = Command::new(binary).args(["ip", "-4"]).output().ok()?;
    if !output.status.success() {
        // Try without -4
        let output = Command::new(binary).arg("ip").output().ok()?;
        if !output.status.success() {
            return None;
        }
        let ip = String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        return (!ip.is_empty()).then_some(ip);
    }
    let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!ip.is_empty()).then_some(ip)
}

pub fn install_instructions() -> String {
    r#"Tailscale is not installed.

Install Tailscale, then sign in with the same account on this machine and your phone:

  macOS:   https://tailscale.com/download/mac
           or:  brew install --cask tailscale

  Linux:   https://tailscale.com/download/linux
           or:  curl -fsSL https://tailscale.com/install.sh | sh

  Windows: https://tailscale.com/download/windows

After installing:
  1. Open Tailscale and log in
  2. On your phone, install the Tailscale app and log into the SAME account
  3. Run /remote again"#
        .to_string()
}

fn not_running_hint() -> String {
    r#"Tailscale is installed but not connected.

Start Tailscale and sign in on this machine:

  macOS:   open the Tailscale app from the menu bar and click Log in
           or:  tailscale up

  Linux:   sudo tailscale up

Then make sure your phone is logged into the SAME Tailscale account
(so it can reach this machine on the private tailnet), and run /remote again."#
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_instructions_mention_same_account() {
        let s = install_instructions();
        assert!(s.to_lowercase().contains("same"));
        assert!(s.contains("tailscale.com"));
    }

    #[test]
    fn not_running_hint_mentions_phone() {
        let s = not_running_hint();
        assert!(s.to_lowercase().contains("phone"));
        assert!(s.to_lowercase().contains("same"));
    }
}
