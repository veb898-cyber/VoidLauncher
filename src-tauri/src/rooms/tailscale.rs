//! TailscaleManager: detect, install, log in, and query Tailscale on the
//! host machine.
//!
//! The launcher treats Tailscale as an opaque private-network client:
//!   * no auth keys are stored by the launcher (interactive device flow),
//!   * the MSI is downloaded from the allowlisted mirror and its SHA-256 is
//!     verified before installation,
//!   * `tailscale status --json` is the single source of truth for
//!     connectivity and peer state.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::{LauncherError, Result};
use crate::rooms::peer_discovery::{HostInfo, PeerInfo};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// Default location of the Tailscale Windows client.
#[cfg(target_os = "windows")]
const TAILSCALE_EXE: &str = "tailscale.exe";

pub struct TailscaleManager {
    data_dir: PathBuf,
}

impl TailscaleManager {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }

    /// Absolute path to the Tailscale CLI, if installed.
    pub fn exe_path(&self) -> Option<PathBuf> {
        tailscale_exe_path()
    }

    pub fn is_installed(&self) -> bool {
        self.exe_path().is_some()
    }

    /// `tailscale version` (first token), e.g. "1.102.4".
    pub fn version(&self) -> Option<String> {
        let exe = self.exe_path()?;
        let out = run_cli_output(&exe, &["version"]).ok()?;
        out.lines().next().map(|l| l.trim().to_string())
    }

    /// Run `tailscale status --json` and parse the result.
    pub fn status(&self) -> Result<TailscaleStatus> {
        let exe = self
            .exe_path()
            .ok_or_else(|| LauncherError::Auth("Tailscale is not installed".into()))?;
        let out = run_cli_output(&exe, &["status", "--json"])
            .map_err(|e| LauncherError::Auth(e))?;
        Self::parse_status(&out)
    }

    /// Parse a `tailscale status --json` document. Pure and unit-testable.
    ///
    /// Deliberately tolerant: every top-level field is extracted best-effort
    /// from raw JSON so that a single unexpected/malformed node or field
    /// (seen in real-world output) degrades gracefully instead of failing the
    /// status entirely and leaving the Rooms page stuck on setup.
    pub fn parse_status(json: &str) -> Result<TailscaleStatus> {
        let root: serde_json::Value =
            serde_json::from_str(json).map_err(LauncherError::Json)?;
        let field = |name: &str| root.get(name).cloned();
        Ok(TailscaleStatus {
            version: field("Version").and_then(|v| v.as_str().map(String::from)),
            backend_state: field("BackendState").and_then(|v| v.as_str().map(String::from)),
            auth_url: field("AuthURL").and_then(|v| v.as_str().map(String::from)),
            current_tailnet: field("CurrentTailnet")
                .and_then(|v| serde_json::from_value(v).ok()),
            self_node: field("Self").and_then(|v| serde_json::from_value(v).ok()),
            peer: field("Peer"),
            user: field("User").and_then(|v| serde_json::from_value(v).ok()),
            magic_dns_suffix: field("MagicDNSSuffix")
                .and_then(|v| v.as_str().map(String::from)),
        })
    }

    /// True once the local node is authenticated to a tailnet.
    ///
    /// `tailscale status --json` does NOT include a `LoggedIn` field on the
    /// Self node — the authoritative signal is `BackendState`: the node is
    /// logged in and reachable as long as it is `Running`/`Starting`.
    /// A `Stopped` daemon (netstack disabled) is treated as not ready so that
    /// "Check connection" re-runs `tailscale up` to restore the link.
    pub fn is_logged_in(status: &TailscaleStatus) -> bool {
        let Some(state) = status
            .backend_state
            .as_deref()
            .map(|s| s.to_ascii_lowercase())
        else {
            return false;
        };
        matches!(state.as_str(), "running" | "starting")
    }

    /// Interactive-login URL, if the node still needs approval.
    pub fn auth_url(status: &TailscaleStatus) -> Option<String> {
        if Self::is_logged_in(status) {
            return None;
        }
        status.auth_url.clone().filter(|u| !u.is_empty())
    }

    /// Local node's first tailnet IP (e.g. "100.101.102.103").
    pub fn self_ip(status: &TailscaleStatus) -> Option<String> {
        status
            .self_node
            .as_ref()
            .and_then(|n| n.tailnet_ips.as_ref())
            .and_then(|ips| ips.first().cloned())
    }

    /// MagicDNS suffix (e.g. "tail3e2a10.ts.net").
    pub fn magic_dns_suffix(status: &TailscaleStatus) -> Option<String> {
        status.magic_dns_suffix.clone().filter(|s| !s.is_empty())
    }

    /// The node's own friendly hostname — exactly the value that
    /// `tailscale set --hostname` expects. Used to restore the original name
    /// after a temporary room-hint rename.
    pub fn self_hostname(status: &TailscaleStatus) -> Option<String> {
        status
            .self_node
            .as_ref()
            .and_then(|n| n.host_name.clone())
            .and_then(|h| sanitize_hostname(&h).ok())
    }

    /// The local node as a *host* endpoint. The HOST machine never has a
    /// discovered peer, but the status UI needs a host to render
    /// port-found/endpoint state — this presents the machine itself.
    pub fn self_host_info(status: &TailscaleStatus) -> Option<HostInfo> {
        status.self_node.as_ref().map(PeerInfo::from).map(|p| HostInfo::from_peer(&p))
    }

    /// Start the interactive login flow. Returns once `tailscale up` has
    /// been spawned (it runs until the user approves the browser flow); the
    /// caller polls `status()` for `AuthURL` → open it → poll until
    /// `is_logged_in`.
    pub fn login(&self) -> Result<()> {
        let exe = self
            .exe_path()
            .ok_or_else(|| LauncherError::Auth("Tailscale is not installed".into()))?;
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("up");
        #[cfg(target_os = "windows")]
        cmd.creation_flags(crate::launch::CREATE_NO_WINDOW);
        spawn_detached(cmd).map_err(|e| LauncherError::Auth(e.to_string()))
    }

    /// Optionally stamp the node with a room-derived hostname
    /// (`<room>-host`) — used as an *experimental* discovery hint for the
    /// guest. Best-effort; failures do not block room creation.
    pub fn set_hostname(&self, hostname: &str) -> Result<()> {
        let exe = self
            .exe_path()
            .ok_or_else(|| LauncherError::Auth("Tailscale is not installed".into()))?;
        let hostname = sanitize_hostname(hostname)?;
        let out = run_cli_output(&exe, &["set", "--hostname", &hostname]).map_err(|e| {
            tracing::warn!(target: "rooms", "tailscale set --hostname failed: {}", e);
            LauncherError::Launch(format!("Failed to set Tailscale hostname: {}", e))
        })?;
        tracing::debug!(target: "rooms", "tailscale set --hostname -> {}", out.trim());
        Ok(())
    }

    /// Cache directory for the installer download.
    pub fn cache_dir(&self) -> PathBuf {
        crate::rooms::rooms_dir(&self.data_dir).join("cache")
    }

    /// Download, verify and silently install the Tailscale MSI.
    /// `progress` receives (fraction, label) — the command layer forwards
    /// these to the frontend as events.
    pub async fn install_msi(
        &self,
        progress: impl Fn(f64, &str) + Send + Sync + 'static,
    ) -> Result<PathBuf> {
        // 1. Resolve the latest stable MSI filename from the manifest and
        //    build its URL from the fixed, allowlisted base only.
        progress(0.05, "Resolving latest Tailscale version");
        let manifest_url = "https://pkgs.tailscale.com/stable/?mode=json";
        tracing::info!(target: "rooms", "Fetching Tailscale stable manifest from {}", manifest_url);
        let client = crate::download::global_http_client()?;
        let body = crate::download::send_with_fallback(client.get(manifest_url))
            .await
            .map_err(|e| LauncherError::Network(e))?
            .json::<serde_json::Value>()
            .await?;

        let filename = extract_msi_filename(&body)
            .ok_or_else(|| LauncherError::Download("No amd64 MSI in Tailscale manifest".into()))?;
        let msi_url = build_msi_url(&filename)?;
        let version = body
            .get("Version")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        tracing::info!(target: "rooms", "Tailscale stable version: {} ({})", version, filename);

        // 2. Fetch the SHA-256 from the matching `.sha256` file. Done before
        //    downloading the (large) MSI so a bad manifest fails fast.
        progress(0.1, "Fetching installer checksum");
        let sha256_hex = fetch_sha256(&client, &msi_url).await?;

        // 3. Download to the cache dir.
        let cache_dir = self.cache_dir();
        std::fs::create_dir_all(&cache_dir)?;
        let msi_path = cache_dir.join(sanitized_cache_name(&filename));
        if !msi_path.exists() {
            progress(0.15, "Downloading installer");
            let bytes = crate::download::send_with_fallback(client.get(&msi_url))
                .await
                .map_err(|e| LauncherError::Network(e))?
                .bytes()
                .await
                .map_err(|e| LauncherError::Network(e))?;
            progress(0.7, "Saving installer");
            std::fs::write(&msi_path, &bytes)?;
        } else {
            progress(0.7, "Installer already cached");
        }

        // 4. Verify SHA-256.
        progress(0.75, "Verifying installer checksum");
        verify_sha256(&msi_path, &sha256_hex)?;

        // 5. Silent install.
        progress(0.8, "Installing Tailscale");
        install_msi_silent(&msi_path)?;

        // 6. Give the service a moment to come up.
        std::thread::sleep(std::time::Duration::from_millis(1500));
        if !self.is_installed() && tailscale_exe_path().is_none() {
            // The service may still be registering; one quick retry window.
            std::thread::sleep(std::time::Duration::from_millis(3000));
        }
        progress(1.0, "Tailscale installed");
        tracing::info!(target: "rooms", "Tailscale installed from {}", msi_path.display());
        Ok(msi_path)
    }
}

/// Locate the Tailscale CLI executable on disk.
pub fn tailscale_exe_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let mut candidates = Vec::new();
        for pf in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
            if let Ok(base) = std::env::var(pf) {
                if !base.is_empty() {
                    candidates.push(PathBuf::from(base).join("Tailscale").join(TAILSCALE_EXE));
                }
            }
        }
        for cand in candidates {
            if cand.is_file() {
                return Some(cand);
            }
        }
        None
    }
    #[cfg(not(target_os = "windows"))]
    {
        find_on_path("tailscale")
    }
}

#[cfg(not(target_os = "windows"))]
fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// Run the CLAN CLI and capture stdout; stderr is merged for diagnostics.
pub fn run_cli_output(exe: &Path, args: &[&str]) -> std::result::Result<String, String> {
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(crate::launch::CREATE_NO_WINDOW);
    let out = cmd
        .output()
        .map_err(|e| format!("Failed to run {:?}: {}", exe, e))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(format!("{} exited {:?}: {}", exe.display(), out.status.code(), stderr.trim()).to_string())
    }
}

/// Spawn a command detached (tailscale up runs until the user approves the
/// browser flow); the returned Child is intentionally dropped, which does
/// not terminate the process on Windows.
fn spawn_detached(mut cmd: std::process::Command) -> std::io::Result<()> {
    let child = cmd.spawn()?;
    drop(child);
    Ok(())
}

/// Locate the amd64 MSI entry inside the pkgs.tailscale.com stable manifest.
/// Fixed, allowlisted base for Tailscale stable Windows installers. MSI URLs
/// are built ONLY from this constant — a filename coming from the remote
/// manifest is validated and appended, never trusted as a URL itself.
const TAILSCALE_STABLE_BASE: &str = "https://pkgs.tailscale.com/stable/";

/// Pick the amd64 MSI *filename* from the live stable manifest. Current
/// schema: `"MSIs": { "amd64": "tailscale-setup-<ver>-amd64.msi", ... }` —
/// values are bare filenames, not `{URL, SHA256}` objects.
fn extract_msi_filename(body: &serde_json::Value) -> Option<String> {
    let msis = body.get("MSIs")?.as_object()?;
    let candidate = msis
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("amd64"))
        .and_then(|(_, value)| value.as_str())?;
    validate_msi_filename(candidate).ok()?;
    Some(candidate.to_string())
}

/// A manifest filename must be a bare, well-formed Tailscale MSI name. This
/// blocks path traversal and URL injection from a compromised/misbehaving
/// manifest before any URL is built.
fn validate_msi_filename(name: &str) -> Result<()> {
    let lower = name.to_ascii_lowercase();
    let safe = !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        && lower.starts_with("tailscale-setup")
        && lower.ends_with(".msi");
    if safe {
        Ok(())
    } else {
        Err(LauncherError::Download(format!(
            "Unsafe Tailscale MSI filename in manifest: {:?}",
            name
        )))
    }
}

/// Build the MSI URL from the fixed base and an already-validated filename,
/// then re-check the result against the download allowlist (defense in depth).
fn build_msi_url(filename: &str) -> Result<String> {
    validate_msi_filename(filename)?;
    let url = format!("{}{}", TAILSCALE_STABLE_BASE, filename);
    if !crate::is_allowed_download_host(&url) {
        return Err(LauncherError::Download(format!(
            "Tailscale MSI URL not in the download allowlist: {}",
            url
        )));
    }
    Ok(url)
}

/// Cache file name: keep only the final path component as belt-and-braces
/// (the filename is already validated to contain no separators).
fn sanitized_cache_name(filename: &str) -> String {
    std::path::Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("tailscale-setup-amd64.msi")
        .to_string()
}

/// Fetch and validate the SHA-256 published next to the installer as
/// `<msi_url>.sha256` (a plain hex digest, tolerating a trailing filename).
async fn fetch_sha256(client: &reqwest::Client, msi_url: &str) -> Result<String> {
    let sha_url = format!("{}.sha256", msi_url);
    if !crate::is_allowed_download_host(&sha_url) {
        return Err(LauncherError::Download(format!(
            "Tailscale checksum URL not in the download allowlist: {}",
            sha_url
        )));
    }
    let resp = crate::download::send_with_fallback(client.get(&sha_url))
        .await
        .map_err(|e| LauncherError::Network(e))?;
    if !resp.status().is_success() {
        return Err(LauncherError::Download(format!(
            "Failed to fetch Tailscale checksum (HTTP {}) from {}",
            resp.status(),
            sha_url
        )));
    }
    let text = resp.text().await.map_err(|e| LauncherError::Network(e))?;
    parse_sha256(&text).ok_or_else(|| {
        LauncherError::Download(format!(
            "Malformed Tailscale SHA-256 from {}: {:?}",
            sha_url,
            text.trim()
        ))
    })
}

/// Parse a `.sha256` payload: first whitespace-delimited token must be a
/// 64-character hex digest (lowercased).
fn parse_sha256(text: &str) -> Option<String> {
    let token = text.split_whitespace().next()?;
    if token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(token.to_ascii_lowercase())
    } else {
        None
    }
}

/// Validate and normalize a Tailscale hostname: must be all lowercase
/// alphanumeric plus hyphens, start/end with alphanumeric, ≤ 52 chars so the
/// MagicDNS suffix still fits.
pub fn sanitize_hostname(raw: &str) -> Result<String> {
    let mut out = String::new();
    let mut pending_dash = false;
    for c in raw.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash {
                pending_dash = false;
                if !out.is_empty() {
                    out.push('-');
                }
            }
            out.push(c);
        } else {
            pending_dash = true;
        }
    }
    out = out.trim_matches('-').to_string();
    if out.len() > 52 {
        out.truncate(52);
        out = out.trim_end_matches('-').to_string();
    }
    if out.is_empty()
        || out.chars().next().map(|c| !c.is_ascii_alphanumeric()).unwrap_or(true)
        || out.chars().last().map(|c| !c.is_ascii_alphanumeric()).unwrap_or(true)
    {
        return Err(LauncherError::Auth("Invalid Tailscale hostname".into()));
    }
    Ok(out)
}

fn verify_sha256(path: &Path, expected_hex: &str) -> Result<()> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path)?;
    let digest = Sha256::digest(&bytes);
    let actual = hex::encode(digest);
    if !actual.eq_ignore_ascii_case(expected_hex.trim()) {
        return Err(LauncherError::Download(format!(
            "SHA-256 mismatch for {}: expected {}, got {}",
            path.display(),
            expected_hex,
            actual
        )));
    }
    Ok(())
}

/// Run msiexec for a silent install; 0 and 3010 (reboot required) are OK.
#[cfg(target_os = "windows")]
fn install_msi_silent(msi_path: &Path) -> Result<()> {
    let status = std::process::Command::new("msiexec")
        .arg("/i")
        .arg(msi_path)
        .arg("/qn")
        .arg("/norestart")
        .arg("TS_SERVICE_UI_PREFERENCES={\"NoUI\"}")
        .status()
        .map_err(|e| LauncherError::Launch(format!("Failed to start msiexec: {}", e)))?;
    let code = status.code().unwrap_or(-1);
    if code == 0 || code == 3010 {
        Ok(())
    } else if code == 1603 {
        // 1603 = fatal error during installation. When msiexec runs from a
        // non-elevated process (typical: VoidLauncher not started as
        // administrator) the Tailscale per-machine MSI often fails with this
        // code, so give the user an actionable hint instead of a bare code.
        Err(LauncherError::Launch(format!(
            "msiexec exited with code 1603 while installing Tailscale \
             (installation requires administrator rights — run VoidLauncher as administrator)",
        )))
    } else {
        Err(LauncherError::Launch(format!(
            "msiexec exited with code {} while installing Tailscale",
            code
        )))
    }
}

#[cfg(not(target_os = "windows"))]
fn install_msi_silent(_msi_path: &Path) -> Result<()> {
    Err(LauncherError::Launch(
        "Tailscale auto-install is only supported on Windows".into(),
    ))
}

// ======================================================================
// status --json types
// ======================================================================

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct TailscaleStatus {
    pub version: Option<String>,
    pub backend_state: Option<String>,
    #[serde(rename = "AuthURL")]
    pub auth_url: Option<String>,
    pub current_tailnet: Option<TailnetInfo>,
    #[serde(rename = "Self")]
    pub self_node: Option<PeerNode>,
    /// Raw `Peer` value. Its shape varies across Tailscale versions/OSes
    /// (array vs map keyed by node key, sometimes with malformed entries), so
    /// it is kept untyped and extracted node-by-node by [`TailscaleStatus::peers`]
    /// — one bad node can never fail the whole status document again.
    #[serde(rename = "Peer")]
    pub peer: Option<serde_json::Value>,
    pub user: Option<Vec<SelfUser>>,
    #[serde(rename = "MagicDNSSuffix")]
    pub magic_dns_suffix: Option<String>,
}

impl TailscaleStatus {
    /// Extract peer nodes from the raw `Peer` value, skipping any entry that
    /// is not a parseable peer object. Never fails the whole status document.
    pub fn peers(&self) -> Vec<PeerNode> {
        let Some(peer) = &self.peer else {
            return Vec::new();
        };
        let values: Vec<&serde_json::Value> = match peer {
            serde_json::Value::Array(list) => list.iter().collect(),
            serde_json::Value::Object(map) => map.values().collect(),
            _ => return Vec::new(),
        };
        values
            .into_iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct TailnetInfo {
    pub name: Option<String>,
    #[serde(rename = "MAGICDNSEnabled")]
    pub magic_dns_enabled: Option<bool>,
    pub node_capabilities: Option<Vec<String>>,
}

/// A machine on the local tailnet (also used for the Self node).
    ///
    /// Only `node_key` is strictly required by real-world output; everything
    /// else is optional. `NodeKey` is an identifier, never a connection
    /// target — discovery resolves hosts by DNS name / tailnet IP — so a node
    /// that omits it must not break the whole status document.
    #[derive(Debug, Clone, Deserialize, Serialize)]
    #[serde(rename_all = "PascalCase")]
    pub struct PeerNode {
        pub node_key: Option<String>,
        pub host_name: Option<String>,
        #[serde(rename = "DNSName")]
        pub dns_name: Option<String>,
        #[serde(rename = "TailscaleIPs")]
        pub tailnet_ips: Option<Vec<String>>,
        pub online: Option<bool>,
        pub last_seen: Option<String>,
        pub sharee_node: Option<bool>,
        pub logged_in: Option<bool>,
        pub user_id: Option<i64>,
    }

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SelfUser {
    pub id: Option<i64>,
    pub login_name: Option<String>,
    pub display_name: Option<String>,
}

// ======================================================================

/// Public `RoomInfo`-style shape for the frontend (serialized by commands).
#[derive(Debug, Clone, Serialize)]
pub struct TailscaleStatusPublic {
    pub installed: bool,
    pub service_ok: bool,
    pub logged_in: bool,
    pub version: Option<String>,
    pub tailnet: Option<String>,
    pub login_name: Option<String>,
    pub self_ip: Option<String>,
    pub auth_url: Option<String>,
}

/// Snapshot-backed status for the UI; never exposes keys or secrets.
#[allow(dead_code)]
pub fn public_status(installed: bool, status: Option<&TailscaleStatus>) -> TailscaleStatusPublic {
    let logged_in = status.map(TailscaleManager::is_logged_in).unwrap_or(false);
    TailscaleStatusPublic {
        installed,
        service_ok: installed && logged_in,
        logged_in,
        version: status.and_then(|s| s.version.clone()),
        tailnet: status
            .and_then(|s| s.current_tailnet.as_ref())
            .and_then(|t| t.name.clone()),
        login_name: status
            .and_then(|s| s.user.as_ref())
            .and_then(|users| users.first())
            .and_then(|u| u.login_name.clone()),
        self_ip: status.and_then(TailscaleManager::self_ip),
        auth_url: status.and_then(TailscaleManager::auth_url),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS_V2: &str = r#"{
      "Version": "1.102.4",
      "TUN": true,
      "BackendState": "Running",
      "AuthURL": "https://login.tailscale.com/a/abc123",
      "TailscaleIPs": ["100.101.102.103"],
      "Self": {
        "NodeKey": "nodekey:self:aaa",
        "HostName": "desktop-pc",
        "DNSName": "desktop-pc.tail3e2a10.ts.net.",
        "TailscaleIPs": ["100.101.102.103"],
        "Online": true,
        "OS": "windows"
      },
      "Peer": [
        {
          "NodeKey": "nodekey:peer:bbb",
          "HostName": "android-phone",
          "DNSName": "android-phone.tail3e2a10.ts.net.",
          "TailscaleIPs": ["100.101.102.104"],
          "Online": false,
          "LastSeen": "2026-09-18T10:00:00Z"
        }
      ],
      "User": [
        { "ID": 1, "LoginName": "veb898@example.com", "DisplayName": "veb898" }
      ],
      "CurrentTailnet": { "Name": "tail3e2a10.ts.net", "MAGICDNSEnabled": true },
      "MagicDNSSuffix": "tail3e2a10.ts.net"
    }"#;

    #[test]
    fn parses_status_v2_and_reports_login() {
        let status = TailscaleManager::parse_status(STATUS_V2).unwrap();
        assert_eq!(status.version.as_deref(), Some("1.102.4"));
        assert_eq!(status.backend_state.as_deref(), Some("Running"));
        assert!(TailscaleManager::is_logged_in(&status));
        assert_eq!(TailscaleManager::auth_url(&status), None);
        assert_eq!(TailscaleManager::self_ip(&status).as_deref(), Some("100.101.102.103"));
        assert_eq!(
            TailscaleManager::magic_dns_suffix(&status).as_deref(),
            Some("tail3e2a10.ts.net")
        );

        let peers = status.peers();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].host_name.as_deref(), Some("android-phone"));
        assert_eq!(peers[0].online, Some(false));

        let users = status.user.expect("user list");
        assert_eq!(users[0].login_name.as_deref(), Some("veb898@example.com"));
    }

    #[test]
    fn parses_map_keyed_peer_field() {
        let json = r#"{
          "Version": "1.40.0",
          "BackendState": "Running",
          "Self": { "NodeKey": "n:self", "LoggedIn": true },
          "Peer": {
            "nodekey:peer:ccc": {
              "NodeKey": "nodekey:peer:ccc",
              "HostName": "old-peer",
              "DNSName": "old-peer.ts.net.",
              "TailscaleIPs": ["100.102.0.5"],
              "Online": true
            }
          }
        }"#;
        let status = TailscaleManager::parse_status(json).unwrap();
        let peers = status.peers();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].host_name.as_deref(), Some("old-peer"));
    }

    #[test]
    fn not_logged_in_exposes_auth_url() {
        // Real `tailscale status --json` shape: no LoggedIn field on Self,
        // login state is carried by BackendState alone.
        let json = r#"{
          "Version": "1.102.4",
          "BackendState": "NeedsLogin",
          "AuthURL": "https://login.tailscale.com/a/xyz",
          "Self": {
            "NodeKey": "nodekey:self:n",
            "HostName": "desktop-pc",
            "DNSName": "desktop-pc.ts.net.",
            "Online": false
          }
        }"#;
        let status = TailscaleManager::parse_status(json).unwrap();
        assert!(!TailscaleManager::is_logged_in(&status));
        assert_eq!(
            TailscaleManager::auth_url(&status).as_deref(),
            Some("https://login.tailscale.com/a/xyz")
        );
        assert_eq!(TailscaleManager::self_ip(&status), None);
    }

    #[test]
    fn peer_without_nodekey_does_not_break_status_parsing() {
        // Regression: real `tailscale status --json` can contain a peer that
        // omits `NodeKey`; previously the whole document failed to parse and
        // the Rooms setup was stuck on "waiting for sign-in".
        let json = r#"{
          "Version": "1.104.4",
          "BackendState": "Running",
          "AuthURL": "",
          "Self": {
            "NodeKey": "nodekey:self:x",
            "HostName": "desktop-pc",
            "DNSName": "desktop-pc.my-tail.ts.net.",
            "OS": "windows",
            "TailscaleIPs": ["100.100.100.100"],
            "Online": true
          },
          "Peer": [
            {
              "HostName": "shared-pc",
              "DNSName": "shared-pc.my-tail.ts.net.",
              "OS": "windows",
              "TailscaleIPs": ["100.100.100.5"],
              "Online": true
            }
          ]
        }"#;
        let status = TailscaleManager::parse_status(json).unwrap();
        assert!(TailscaleManager::is_logged_in(&status));
        assert_eq!(
            TailscaleManager::self_ip(&status).as_deref(),
            Some("100.100.100.100")
        );
        let peers = crate::rooms::peer_discovery::peers_from_status(&status);
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].host_name, "shared-pc");
    }

    #[test]
    fn malformed_peers_map_never_fails_status() {
        // Regression: the user's machine fed us a `Peer` that is a map with
        // a malformed entry. The whole status must still parse and expose
        // login state; the bad node is skipped, the good one survives.
        let json = r#"{
          "Version": "1.104.4",
          "BackendState": "Running",
          "AuthURL": "",
          "Self": {
            "NodeKey": "nodekey:self:x",
            "HostName": "desktop-pc",
            "DNSName": "desktop-pc.my-tail.ts.net.",
            "TailscaleIPs": ["100.100.100.100"],
            "Online": true
          },
          "Peer": {
            "nodekey:peer:good": {
              "NodeKey": "nodekey:peer:good",
              "HostName": "shared-pc",
              "DNSName": "shared-pc.my-tail.ts.net.",
              "TailscaleIPs": ["100.100.100.5"],
              "Online": true
            },
            "nodekey:peer:broken": "not-an-object"
          }
        }"#;
        let status = TailscaleManager::parse_status(json).unwrap();
        assert!(TailscaleManager::is_logged_in(&status));
        let peers = status.peers();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].host_name.as_deref(), Some("shared-pc"));
    }

    #[test]
    fn connected_node_without_loggedin_field_is_logged_in() {
        // Regression: an authenticated node reports BackendState "Running"
        // with no LoggedIn field on Self — the launcher must treat it as
        // logged in (previously this returned false indefinitely, leaving
        // the Rooms setup stuck on "waiting for sign-in").
        let json = r#"{
          "Version": "1.104.4",
          "BackendState": "Running",
          "AuthURL": "",
          "Self": {
            "NodeKey": "nodekey:self:abc",
            "HostName": "desktop-pc",
            "DNSName": "desktop-pc.my-tail.ts.net.",
            "OS": "windows",
            "TailscaleIPs": ["100.100.100.100"],
            "Online": true,
            "LastSeen": "2026-09-20T00:00:00Z",
            "ShareeNode": false
          },
          "User": [
            { "ID": 1, "LoginName": "me@example.com", "DisplayName": "me" }
          ]
        }"#;
        let status = TailscaleManager::parse_status(json).unwrap();
        assert!(TailscaleManager::is_logged_in(&status));
        assert_eq!(TailscaleManager::auth_url(&status), None);
        assert_eq!(
            TailscaleManager::self_ip(&status).as_deref(),
            Some("100.100.100.100")
        );
    }

    #[test]
    fn sanitizes_hostnames_for_room_discovery() {
        assert_eq!(sanitize_hostname("ABCD-1234").unwrap(), "abcd-1234");
        assert_eq!(
            sanitize_hostname("AbCd-1234-host").unwrap(),
            "abcd-1234-host"
        );
        assert_eq!(sanitize_hostname("_weird_ name!").unwrap(), "weird-name");
        assert_eq!(
            sanitize_hostname(&"a".repeat(80)).unwrap().len(),
            52,
            "hostname clamped to 52 chars"
        );
        assert!(sanitize_hostname("-").is_err());
        assert!(sanitize_hostname("").is_err());
    }

    #[test]
    fn self_hostname_is_the_sanitized_machine_name() {
        let status = TailscaleManager::parse_status(STATUS_V2).unwrap();
        assert_eq!(TailscaleManager::self_hostname(&status).as_deref(), Some("desktop-pc"));
    }

    #[test]
    fn self_host_info_exposes_the_node_as_a_host_endpoint() {
        let status = TailscaleManager::parse_status(STATUS_V2).unwrap();
        let info = TailscaleManager::self_host_info(&status).expect("self host info");
        assert_eq!(info.host_name, "desktop-pc");
        assert_eq!(info.dns(), Some("desktop-pc.tail3e2a10.ts.net"));
        assert_eq!(info.ip(), Some("100.101.102.103"));
        assert!(info.online, "self node is online");
    }

    #[test]
    fn extracts_amd64_filename_from_live_manifest_shape() {
        // Exact shape returned by https://pkgs.tailscale.com/stable/?mode=json
        // (MSIs values are bare filenames, not {URL, SHA256} objects).
        let body: serde_json::Value = serde_json::json!({
            "Version": "1.102.4",
            "MSIsVersion": "1.102.4",
            "MSIs": {
                "amd64": "tailscale-setup-1.102.4-amd64.msi",
                "arm64": "tailscale-setup-1.102.4-arm64.msi",
                "x86": "tailscale-setup-1.102.4-x86.msi"
            }
        });
        let filename = extract_msi_filename(&body).expect("amd64 filename");
        assert_eq!(filename, "tailscale-setup-1.102.4-amd64.msi");
    }

    #[test]
    fn amd64_key_is_matched_case_insensitively() {
        let body: serde_json::Value = serde_json::json!({
            "MSIs": { "AMD64": "tailscale-setup-1.102.4-amd64.msi" }
        });
        assert_eq!(
            extract_msi_filename(&body).as_deref(),
            Some("tailscale-setup-1.102.4-amd64.msi")
        );
    }

    #[test]
    fn missing_amd64_arch_yields_none() {
        let only_others: serde_json::Value = serde_json::json!({
            "MSIs": {
                "arm64": "tailscale-setup-1.102.4-arm64.msi",
                "x86": "tailscale-setup-1.102.4-x86.msi"
            }
        });
        assert!(extract_msi_filename(&only_others).is_none());

        let no_msis: serde_json::Value = serde_json::json!({ "Version": "1.102.4" });
        assert!(extract_msi_filename(&no_msis).is_none());

        // Legacy object value (old schema) is not a filename -> rejected.
        let legacy: serde_json::Value = serde_json::json!({
            "MSIs": { "amd64": { "URL": "https://pkgs.tailscale.com/x.msi", "SHA256": "aa" } }
        });
        assert!(extract_msi_filename(&legacy).is_none());
    }

    #[test]
    fn rejects_unsafe_msi_filenames() {
        assert!(validate_msi_filename("tailscale-setup-1.102.4-amd64.msi").is_ok());
        assert!(validate_msi_filename("tailscale-setup-1.102.4-arm64.msi").is_ok());

        for bad in [
            "",
            "../evil.msi",
            "..\\evil.msi",
            "sub/dir/tailscale-setup-1.0-amd64.msi",
            "http://evil.example/x.msi",
            "https://evil.example/tailscale-setup-1.0-amd64.msi",
            "tailscale-setup-1.102.4-amd64.exe",
            "evil-1.102.4-amd64.msi",
            "tailscale-setup-1.0-amd64.msi ",
            "tailscale-setup-1.0-amd64.msi\n",
        ] {
            assert!(
                validate_msi_filename(bad).is_err(),
                "must reject unsafe filename {:?}",
                bad
            );
        }

        let oversize = format!("tailscale-setup-{}-amd64.msi", "9".repeat(200));
        assert!(validate_msi_filename(&oversize).is_err(), "must reject oversized name");

        // An unsafe filename in the manifest is dropped, not returned.
        let body: serde_json::Value = serde_json::json!({
            "MSIs": { "amd64": "../../../Windows/System32/calc.msi" }
        });
        assert!(extract_msi_filename(&body).is_none());
    }

    #[test]
    fn build_msi_url_uses_fixed_allowlisted_base() {
        let url = build_msi_url("tailscale-setup-1.102.4-amd64.msi").unwrap();
        assert_eq!(
            url,
            "https://pkgs.tailscale.com/stable/tailscale-setup-1.102.4-amd64.msi"
        );
        assert!(crate::is_allowed_download_host(&url));

        assert!(build_msi_url("../evil.msi").is_err());
        assert!(build_msi_url("https://evil.example/x.msi").is_err());
    }

    #[test]
    fn parse_sha256_accepts_hex_and_rejects_garbage() {
        let good = "80eb007e39dfebe17299fa1a09c79a8e1d934f76e0246c0817ebe3af675b7ef6";
        assert_eq!(parse_sha256(good).as_deref(), Some(good));
        assert_eq!(parse_sha256(&good.to_uppercase()).as_deref(), Some(good));
        // `sha256sum`-style "hash  filename" keeps the first token.
        let with_name = format!("{}  tailscale-setup-1.102.4-amd64.msi\n", good);
        assert_eq!(parse_sha256(&with_name).as_deref(), Some(good));

        assert_eq!(parse_sha256(""), None);
        assert_eq!(parse_sha256("   \n"), None);
        assert_eq!(parse_sha256("abcd"), None);
        assert_eq!(parse_sha256(&"a".repeat(63)), None);
        assert_eq!(parse_sha256(&"a".repeat(65)), None);
        assert_eq!(parse_sha256(&"z".repeat(64)), None);
    }

    /// Real-network, opt-in: fetch the live Tailscale stable manifest, resolve
    /// the amd64 filename, build the allowlisted URL and fetch its `.sha256`.
    /// Validates the download-resolution half of the install path WITHOUT
    /// installing anything (silent MSI install needs elevation).
    /// Run: `cargo test tailscale_manifest -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "network; opt-in E2E"]
    async fn tailscale_manifest_resolves_allowlisted_msi() {
        let client = crate::download::global_http_client().expect("http client");
        let body = crate::download::send_with_fallback(
            client.get("https://pkgs.tailscale.com/stable/?mode=json"),
        )
        .await
        .expect("fetch manifest")
        .json::<serde_json::Value>()
        .await
        .expect("manifest json");

        let filename = extract_msi_filename(&body).expect("amd64 MSI filename in live manifest");
        let url = build_msi_url(&filename).expect("allowlisted MSI url");
        assert!(
            crate::is_allowed_download_host(&url),
            "live MSI host must be allowlisted: {}",
            url
        );
        let sha = fetch_sha256(&client, &url).await.expect("live sha256");
        assert_eq!(sha.len(), 64, "SHA-256 hex length for {}", url);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
        println!("[tailscale] live manifest -> {} (sha256 {}...)", url, &sha[..12]);
    }

    /// Real-network, opt-in: a missing installer must surface a checksum
    /// fetch error instead of silently proceeding.
    #[tokio::test]
    #[ignore = "network; opt-in E2E"]
    async fn tailscale_manifest_sha256_fetch_error_surfaces() {
        let client = crate::download::global_http_client().expect("http client");
        let missing = "https://pkgs.tailscale.com/stable/tailscale-setup-0.0.0-amd64.msi";
        let result = fetch_sha256(&client, missing).await;
        assert!(result.is_err(), "missing installer checksum must error");
        println!(
            "[tailscale] checksum fetch error surfaced: {}",
            result.unwrap_err()
        );
    }

    #[test]
    fn sha256_verification_rejects_mismatch() {
        // generate a file with known content
        let dir = std::env::temp_dir().join(format!("vl_sha_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("check.bin");
        std::fs::write(&f, b"hello tailscale").unwrap();
        assert!(verify_sha256(&f, "wrong").is_err());

        // Correct hash precomputed for "hello tailscale"
        let good = "889cca03bac0bea98fa8f51776da6e4ebbab7e6d473f9932d79b1b7f0f137047";
        assert!(verify_sha256(&f, good).is_ok());
        assert!(verify_sha256(&f, &good.to_uppercase()).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }
}