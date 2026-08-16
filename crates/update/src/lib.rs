//! zeron-update — release checking and self-update, shared by the engine (the
//! background checker + `ApplyUpdate`), the CLI (`zeron update`), and the UI
//! (the sidebar update strip + macOS bundle swap).
//!
//! Release layout (see `.github/workflows/release.yml`): immutable versioned
//! artifacts live in GitHub Releases. A fixed stable/beta release carries an
//! Ed25519-signed manifest with the version, artifact origin, byte sizes, and
//! SHA-256 digests. Updates fail closed if any trust check is unavailable.
//!
//! Install kinds and their update paths:
//! - **Managed** (`~/.zeron/app/<ver>` + `current` symlink — the curl|sh
//!   installer): download the headless tarball into a new versioned dir, flip
//!   the symlink, restart the service. Same flow the installer script performs,
//!   natively.
//! - **MacApp** (running out of an app bundle): download the app tarball, swap the
//!   bundle directory, relaunch. Driven by the UI.
//! - **Unmanaged** (source builds, hand-copied binaries): report only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, bail};
use base64::Engine as _;
use ed25519_dalek::{Signature, VerifyingKey};
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::watch;

/// The version compiled into this binary (the workspace version).
pub const fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Background check cadence.
const CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);
/// Retry sooner after a failed check (offline boot, transient edge error).
const CHECK_RETRY: std::time::Duration = std::time::Duration::from_secs(30 * 60);
/// First check waits out engine boot (room joins, doc re-sync).
const CHECK_INITIAL_DELAY: std::time::Duration = std::time::Duration::from_secs(20);
/// While an auto-apply is deferred behind active sessions, re-probe idleness
/// this often.
const IDLE_RECHECK: std::time::Duration = std::time::Duration::from_secs(5 * 60);

// ---------------------------------------------------------------------------
// Release metadata
// ---------------------------------------------------------------------------

/// `{edge}/releases/manifest.json` — written by the release workflow.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Manifest format. Unknown versions fail closed.
    #[serde(default)]
    pub schema_version: u32,
    /// Release channel this manifest belongs to (`stable` or `beta`).
    #[serde(default)]
    pub channel: String,
    pub version: String,
    /// RFC 3339 publication timestamp, included in signed release metadata.
    #[serde(default)]
    pub published_at: String,
    /// Signed immutable directory containing this release's artifacts.
    #[serde(default)]
    pub artifact_base_url: String,
    /// Artifact file name → mandatory integrity metadata.
    #[serde(default)]
    pub files: BTreeMap<String, FileMeta>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FileMeta {
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
}

const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Update channel selected for this installation. Invalid values fail closed.
pub fn update_channel() -> anyhow::Result<String> {
    let channel = std::env::var("ZERON_UPDATE_CHANNEL").unwrap_or_else(|_| "stable".into());
    if !matches!(channel.as_str(), "stable" | "beta") {
        bail!("invalid ZERON_UPDATE_CHANNEL `{channel}` (expected stable or beta)");
    }
    Ok(channel)
}

/// Release origin is independent from the sync edge. Forks must never silently
/// consume another project's binaries. Production builds bake this value in;
/// self-hosted/dev installs can explicitly override it.
pub fn update_base_url(edge_fallback: &str) -> String {
    std::env::var("ZERON_UPDATE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| option_env!("ZERON_UPDATE_URL").map(str::to_owned))
        .unwrap_or_else(|| edge_fallback.to_owned())
}

fn require_secure_url(url: &str, purpose: &str) -> anyhow::Result<()> {
    let parsed = reqwest::Url::parse(url).with_context(|| format!("parsing {purpose} URL"))?;
    let local = matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "::1"));
    if parsed.scheme() != "https" && !(parsed.scheme() == "http" && local) {
        bail!("{purpose} URL must use HTTPS (HTTP is allowed only for localhost)");
    }
    Ok(())
}

fn verifying_key() -> anyhow::Result<VerifyingKey> {
    let encoded = std::env::var("ZERON_UPDATE_PUBLIC_KEY_OVERRIDE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| option_env!("ZERON_UPDATE_PUBLIC_KEY").map(str::to_owned))
        .context("updates are disabled: this build has no ZERON_UPDATE_PUBLIC_KEY")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .context("decoding update public key")?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("update public key must be 32 bytes"))?;
    VerifyingKey::from_bytes(&bytes).context("parsing update public key")
}

fn verify_manifest(bytes: &[u8], signature_b64: &str) -> anyhow::Result<()> {
    verify_manifest_with_key(bytes, signature_b64, &verifying_key()?)
}

fn verify_manifest_with_key(
    bytes: &[u8],
    signature_b64: &str,
    key: &VerifyingKey,
) -> anyhow::Result<()> {
    let bytes_signature = base64::engine::general_purpose::STANDARD
        .decode(signature_b64.trim())
        .context("decoding manifest signature")?;
    let signature =
        Signature::from_slice(&bytes_signature).context("parsing manifest signature")?;
    key.verify_strict(bytes, &signature)
        .context("manifest signature verification failed")
}

/// Artifact-name platform pair — `uname`-style strings matching the packaging
/// scripts: `linux-x86_64`, `linux-aarch64`, `macos-arm64`.
pub fn platform_key() -> (&'static str, &'static str) {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let arch = match (os, std::env::consts::ARCH) {
        ("macos", "aarch64") => "arm64",
        (_, arch) => arch,
    };
    (os, arch)
}

/// `zeron-<ver>-<os>-<arch>.tar.gz` — the headless/CLI tarball (Linux CI builds).
pub fn headless_artifact(version: &str) -> String {
    let (os, arch) = platform_key();
    format!("zeron-{version}-{os}-{arch}.tar.gz")
}

/// `zeron-<ver>-macos-<arch>-app.tar.gz` — the macOS app update payload.
pub fn mac_app_artifact(version: &str) -> String {
    let (_, arch) = platform_key();
    format!("zeron-{version}-macos-{arch}-app.tar.gz")
}

/// Strictly-newer dotted-numeric compare (`0.1.10` > `0.1.9` > `0.1`).
/// Unparseable versions never count as newer — a garbage `latest.txt` must not
/// trigger an update loop.
pub fn version_newer(latest: &str, current: &str) -> bool {
    let parse = |value: &str| semver::Version::parse(value.trim().trim_start_matches('v')).ok();
    match (parse(latest), parse(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

/// Fetch and authenticate the newest release metadata. There is intentionally
/// no unsigned fallback: a missing signature disables updates instead of
/// weakening the trust boundary.
pub async fn fetch_latest(edge_url: &str) -> anyhow::Result<Manifest> {
    let base_url = update_base_url(edge_url);
    let base = base_url.trim_end_matches('/');
    require_secure_url(base, "update")?;
    let channel = update_channel()?;
    let client = http_client()?;
    let channel_base = format!("{base}/channel-{channel}");
    let manifest_url = format!("{channel_base}/manifest.json");
    let signature_url = format!("{channel_base}/manifest.json.sig");
    let manifest_bytes = client
        .get(&manifest_url)
        .send()
        .await
        .context("fetching signed update manifest")?
        .error_for_status()
        .context("fetching signed update manifest")?
        .bytes()
        .await
        .context("reading update manifest")?;
    let signature = client
        .get(&signature_url)
        .send()
        .await
        .context("fetching update manifest signature")?
        .error_for_status()
        .context("fetching update manifest signature")?
        .text()
        .await
        .context("reading update manifest signature")?;
    verify_manifest(&manifest_bytes, &signature)?;
    let manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).context("parsing signed manifest.json")?;
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
        bail!(
            "unsupported update manifest schema {}",
            manifest.schema_version
        );
    }
    if manifest.channel != channel {
        bail!(
            "signed manifest channel mismatch: expected {channel}, got {}",
            manifest.channel
        );
    }
    if manifest.version.trim().is_empty()
        || manifest.published_at.trim().is_empty()
        || manifest.artifact_base_url.trim().is_empty()
    {
        bail!("signed manifest is missing version, publishedAt, or artifactBaseUrl");
    }
    require_secure_url(&manifest.artifact_base_url, "artifact")?;
    Ok(manifest)
}

fn http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("zeron/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(5 * 60))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .context("building http client")
}

// ---------------------------------------------------------------------------
// Install-kind detection
// ---------------------------------------------------------------------------

/// How this binary was installed — decides the update path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallKind {
    /// `~/.zeron/app/<ver>/zeron` behind the `current` symlink
    /// (curl|sh installer / a previous `zeron update`).
    Managed { app_root: PathBuf },
    /// Running out of a macOS `.app` bundle.
    MacApp { bundle: PathBuf },
    /// Source build or hand-copied binary — updates are report-only.
    Unmanaged,
}

pub fn detect_install() -> InstallKind {
    let Ok(exe) = std::env::current_exe() else {
        return InstallKind::Unmanaged;
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    detect_install_from(&exe, home.as_deref())
}

fn detect_install_from(exe: &Path, home: Option<&Path>) -> InstallKind {
    if let Some(home) = home {
        // `current_exe` resolves the `current` symlink to the versioned dir.
        let app_root = home.join(".zeron").join("app");
        if exe.starts_with(&app_root) {
            return InstallKind::Managed { app_root };
        }
    }
    for ancestor in exe.ancestors() {
        if ancestor.extension().is_some_and(|ext| ext == "app")
            && exe.starts_with(ancestor.join("Contents").join("MacOS"))
        {
            return InstallKind::MacApp {
                bundle: ancestor.to_path_buf(),
            };
        }
    }
    InstallKind::Unmanaged
}

// ---------------------------------------------------------------------------
// Download + verify
// ---------------------------------------------------------------------------

/// Stream `{edge}/releases/<file>` to `dest`, verifying the manifest sha256 when
/// present. Writes through a `.partial` sidecar so an interrupted download never
/// leaves a plausible-looking artifact behind.
pub async fn download_release_file(
    edge_url: &str,
    manifest: &Manifest,
    file: &str,
    dest: &Path,
) -> anyhow::Result<()> {
    let _ = edge_url;
    require_secure_url(&manifest.artifact_base_url, "artifact")?;
    let url = format!(
        "{}/{file}",
        manifest.artifact_base_url.trim_end_matches('/')
    );
    let metadata = manifest
        .files
        .get(file)
        .with_context(|| format!("signed manifest has no metadata for {file}"))?;
    if metadata.sha256.len() != 64 || !metadata.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("signed manifest has invalid sha256 for {file}");
    }
    if metadata.size == 0 {
        bail!("signed manifest has invalid size for {file}");
    }
    let partial = dest.with_extension("partial");
    let resp = http_client()?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("downloading {url}"))?
        .error_for_status()
        .with_context(|| format!("downloading {url}"))?;
    let mut out = tokio::fs::File::create(&partial)
        .await
        .with_context(|| format!("creating {}", partial.display()))?;
    let mut hasher = Sha256::new();
    let mut downloaded = 0u64;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading download stream")?;
        downloaded = downloaded
            .checked_add(chunk.len() as u64)
            .context("download size overflow")?;
        if downloaded > metadata.size {
            tokio::fs::remove_file(&partial).await.ok();
            bail!("download exceeded signed size for {file}");
        }
        hasher.update(&chunk);
        out.write_all(&chunk).await.context("writing download")?;
    }
    out.flush().await.ok();
    drop(out);
    if downloaded != metadata.size {
        tokio::fs::remove_file(&partial).await.ok();
        bail!(
            "size mismatch for {file}: expected {}, got {downloaded}",
            metadata.size
        );
    }
    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(metadata.sha256.trim()) {
        tokio::fs::remove_file(&partial).await.ok();
        bail!(
            "checksum mismatch for {file}: expected {}, got {actual}",
            metadata.sha256
        );
    }
    tokio::fs::rename(&partial, dest)
        .await
        .with_context(|| format!("moving {} into place", dest.display()))?;
    Ok(())
}

fn run(program: &str, args: &[&str]) -> anyhow::Result<()> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} {} failed ({}): {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Managed (symlink) installs — the daemon/VPS path
// ---------------------------------------------------------------------------

/// Download + unpack the headless tarball into `app_root/<ver>` (idempotent —
/// an already-staged version is reused). Returns the versioned dir.
pub async fn stage_headless(
    edge_url: &str,
    manifest: &Manifest,
    app_root: &Path,
) -> anyhow::Result<PathBuf> {
    let version = &manifest.version;
    let dest = app_root.join(version);
    if dest.join("zeron").exists() {
        return Ok(dest);
    }
    let file = headless_artifact(version);
    let stage = app_root.join(format!(".stage-{version}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).with_context(|| format!("creating {}", stage.display()))?;
    let result = async {
        let tarball = stage.join(&file);
        download_release_file(edge_url, manifest, &file, &tarball).await?;
        let unpacked = stage.join("unpacked");
        std::fs::create_dir_all(&unpacked)?;
        // Tarball root is the versioned stage dir (see scripts/package-linux.sh);
        // strip it exactly as install.sh does.
        run(
            "tar",
            &[
                "-xzf",
                &tarball.to_string_lossy(),
                "-C",
                &unpacked.to_string_lossy(),
                "--strip-components=1",
            ],
        )?;
        if !unpacked.join("zeron").is_file() {
            bail!("tarball {file} did not contain a zeron binary");
        }
        match std::fs::rename(&unpacked, &dest) {
            Ok(()) => {}
            // Lost a race with another stager — the staged copy is equivalent.
            Err(_) if dest.join("zeron").exists() => {}
            Err(err) => {
                return Err(err).with_context(|| format!("moving {} into place", dest.display()));
            }
        }
        Ok(dest.clone())
    }
    .await;
    let _ = std::fs::remove_dir_all(&stage);
    result
}

/// Atomically repoint `app_root/current` at `app_root/<ver>` (symlink to a temp
/// name, then rename over — never a window with no `current`).
pub fn apply_headless(app_root: &Path, version: &str) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let target = app_root.join(version);
        if !target.join("zeron").exists() {
            bail!("{} is not a staged install", target.display());
        }
        if let Ok(previous) = std::fs::read_link(app_root.join("current")) {
            swap_symlink(app_root, "previous", &previous)?;
        }
        swap_symlink(app_root, "current", &target)
    }
    #[cfg(not(unix))]
    {
        let _ = (app_root, version);
        bail!("managed installs are unix-only");
    }
}

#[cfg(unix)]
fn swap_symlink(app_root: &Path, name: &str, target: &Path) -> anyhow::Result<()> {
    let tmp = app_root.join(format!(".{name}-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp).with_context(|| format!("creating {name} symlink"))?;
    std::fs::rename(&tmp, app_root.join(name)).with_context(|| format!("swapping {name} symlink"))
}

/// Restore the version saved by the most recent managed update.
pub fn rollback_headless(app_root: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let previous = std::fs::read_link(app_root.join("previous"))
            .context("no previous managed version is available")?;
        if !previous.join("zeron").is_file() {
            bail!("previous managed version is incomplete");
        }
        swap_symlink(app_root, "current", &previous)
    }
    #[cfg(not(unix))]
    {
        let _ = app_root;
        bail!("managed installs are unix-only");
    }
}

/// Restart the installed engine service (the same units `zeron daemon` and the
/// curl|sh installer manage). Called after a symlink swap so the running daemon
/// picks up the new binary.
pub fn restart_service() -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        let output = std::process::Command::new("id").arg("-u").output()?;
        let uid = String::from_utf8_lossy(&output.stdout).trim().to_string();
        run(
            "launchctl",
            &["kickstart", "-k", &format!("gui/{uid}/sh.zeron.app")],
        )
    } else {
        run("systemctl", &["--user", "restart", "zeron.service"])
    }
}

// ---------------------------------------------------------------------------
// macOS app-bundle installs — the desktop path
// ---------------------------------------------------------------------------

/// Download + unpack the app tarball into `{data_dir}/updates/<ver>/Zeron.app`
/// (idempotent). Returns the staged bundle path.
pub async fn stage_mac_app(
    edge_url: &str,
    manifest: &Manifest,
    data_dir: &Path,
) -> anyhow::Result<PathBuf> {
    let version = &manifest.version;
    let dir = data_dir.join("updates").join(version);
    let staged = dir.join("Zeron.app");
    if staged.join("Contents/MacOS/zeron").exists() {
        validate_mac_app(&staged)?;
        return Ok(staged);
    }
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let file = mac_app_artifact(version);
    let tarball = dir.join(&file);
    download_release_file(edge_url, manifest, &file, &tarball).await?;
    run(
        "tar",
        &[
            "-xzf",
            &tarball.to_string_lossy(),
            "-C",
            &dir.to_string_lossy(),
        ],
    )?;
    std::fs::remove_file(&tarball).ok();
    if !staged.join("Contents/MacOS/zeron").exists() {
        bail!("app tarball {file} did not contain Zeron.app");
    }
    validate_mac_app(&staged)?;
    Ok(staged)
}

/// Require a valid deep signature and Gatekeeper acceptance before an app
/// bundle can replace the running installation. The release manifest protects
/// transport integrity; this protects the platform identity/notarization layer.
fn validate_mac_app(bundle: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        run(
            "codesign",
            &["--verify", "--deep", "--strict", &bundle.to_string_lossy()],
        )?;
        run(
            "spctl",
            &["--assess", "--type", "execute", &bundle.to_string_lossy()],
        )?;
    }
    #[cfg(not(target_os = "macos"))]
    let _ = bundle;
    Ok(())
}

fn mac_rollback_path(bundle: &Path) -> anyhow::Result<PathBuf> {
    let parent = bundle
        .parent()
        .context("app bundle has no parent directory")?;
    let name = bundle
        .file_name()
        .context("app bundle has no name")?
        .to_string_lossy();
    Ok(parent.join(format!(".{name}.rollback")))
}

/// Swap the installed bundle for the staged one: `ditto` the staged copy next to
/// the target (metadata-preserving, cross-volume safe), then two renames — the
/// old bundle is restored if the second rename fails.
pub fn apply_mac_app(staged: &Path, bundle: &Path) -> anyhow::Result<()> {
    validate_mac_app(staged)?;
    let parent = bundle
        .parent()
        .context("app bundle has no parent directory")?;
    let name = bundle
        .file_name()
        .context("app bundle has no name")?
        .to_string_lossy();
    let pid = std::process::id();
    let fresh = parent.join(format!(".{name}.new-{pid}"));
    let rollback = mac_rollback_path(bundle)?;
    let _ = std::fs::remove_dir_all(&fresh);
    run(
        "ditto",
        &[&staged.to_string_lossy(), &fresh.to_string_lossy()],
    )?;
    if rollback.exists() {
        std::fs::remove_dir_all(&rollback).context("removing stale app rollback")?;
    }
    std::fs::rename(bundle, &rollback).context("preserving the current app for rollback")?;
    if let Err(err) = std::fs::rename(&fresh, bundle) {
        let _ = std::fs::rename(&rollback, bundle);
        let _ = std::fs::remove_dir_all(&fresh);
        return Err(err).context("installing the new app bundle");
    }
    Ok(())
}

/// Detached relauncher: waits for THIS process to exit, then `open`s the bundle.
/// (Opening before exit would race the single-instance engine lock and the IPC
/// port.) The caller quits the app after this returns.
pub fn relaunch_app_after_exit(bundle: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let pid = std::process::id();
        let rollback = mac_rollback_path(bundle).ok();
        let Some(rollback) = rollback else {
            tracing::error!("failed to resolve macOS rollback path");
            return;
        };
        // Arguments carry paths separately from the script, so bundle names
        // cannot become shell syntax. If the new process does not stay alive
        // through the health window, restore the previous bundle and reopen it.
        let script = r#"
            old_pid="$1"; bundle="$2"; rollback="$3"
            while /bin/kill -0 "$old_pid" 2>/dev/null; do sleep 0.2; done
            /usr/bin/open "$bundle" || true
            sleep 15
            executable="$bundle/Contents/MacOS/zeron"
            if /usr/bin/pgrep -f "$executable" >/dev/null 2>&1; then
                /bin/rm -rf -- "$rollback"
            elif [ -d "$rollback" ]; then
                failed="$bundle.failed-$(date +%s)"
                /bin/mv "$bundle" "$failed" 2>/dev/null || true
                /bin/mv "$rollback" "$bundle"
                /usr/bin/open "$bundle" || true
            fi
        "#;
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args([
                "-c",
                script,
                "zeron-update-healthcheck",
                &pid.to_string(),
                &bundle.to_string_lossy(),
                &rollback.to_string_lossy(),
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0);
        if let Err(err) = command.spawn() {
            tracing::error!(error = %err, "failed to spawn the relauncher");
        }
    }
    #[cfg(not(unix))]
    let _ = bundle;
}

// ---------------------------------------------------------------------------
// Engine-side checker
// ---------------------------------------------------------------------------

/// What the engine reports over the `UpdateStatus` stream. Version facts only —
/// download/apply progress is owned by whoever drives the update (UI or CLI).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub current_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    #[serde(default)]
    pub update_available: bool,
    /// Epoch ms of the last successful check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl UpdateStatus {
    fn initial() -> Self {
        Self {
            current_version: current_version().to_string(),
            latest_version: None,
            update_available: false,
            checked_at: None,
            error: None,
        }
    }
}

/// Managed headless installs update automatically by default. Set
/// `ZERON_AUTO_UPDATE=0|false|no` to disable unattended application.
pub fn auto_update_enabled() -> bool {
    std::env::var("ZERON_AUTO_UPDATE")
        .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no"))
        .unwrap_or(true)
}

/// Desktop builds automatically authenticate and stage updates. Installation
/// remains an explicit restart action so active work is never interrupted.
pub fn desktop_auto_update_enabled() -> bool {
    std::env::var("ZERON_AUTO_UPDATE")
        .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no"))
        .unwrap_or(true)
}

/// "Nothing would be interrupted by a restart right now" — wired by the engine
/// to its live-run and open-terminal registries. `None` = no gate.
pub type QuiescentCheck = Arc<dyn Fn() -> bool + Send + Sync>;

/// Background release checker: polls the signed release channel on a 6h cadence and
/// publishes [`UpdateStatus`] over a watch channel (the `UpdateStatus` RPC
/// stream). Managed installs stage + apply + restart by default
/// restart on their own — but only in a quiet window: while `quiescent` reports
/// activity, the apply defers and re-probes every [`IDLE_RECHECK`].
#[derive(Clone)]
pub struct Updater {
    edge_url: String,
    status_tx: Arc<watch::Sender<UpdateStatus>>,
    check_tx: Arc<watch::Sender<u64>>,
    quiescent: Option<QuiescentCheck>,
    /// Flips to true exactly once; the check loop selects against it so
    /// cancellation lands at any await point (no tokio-util in this crate).
    shutdown_tx: Arc<watch::Sender<bool>>,
    check_task: Arc<std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl Updater {
    /// Spawn the check loop (must run on a tokio runtime).
    pub fn spawn(edge_url: String, quiescent: Option<QuiescentCheck>) -> Self {
        let (status_tx, _) = watch::channel(UpdateStatus::initial());
        let (check_tx, _) = watch::channel(0);
        let (shutdown_tx, _) = watch::channel(false);
        let updater = Self {
            edge_url,
            status_tx: Arc::new(status_tx),
            check_tx: Arc::new(check_tx),
            quiescent,
            shutdown_tx: Arc::new(shutdown_tx),
            check_task: Arc::new(std::sync::Mutex::new(None)),
        };
        let for_loop = updater.clone();
        let task = tokio::spawn(async move { for_loop.check_loop().await });
        *updater.check_task.lock().unwrap() = Some(task);
        updater
    }

    /// Stop the check loop and wait for it to exit — a replaced runtime must
    /// not keep polling `{edge}/releases` (or auto-applying) in the background.
    /// Idempotent, and callable from any clone.
    pub async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
        let task = self
            .check_task
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }

    pub fn watch(&self) -> watch::Receiver<UpdateStatus> {
        self.status_tx.subscribe()
    }

    /// Wake the release checker immediately, for example when authentication
    /// recovers after the process started offline.
    pub fn check_now(&self) {
        self.check_tx
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }

    fn quiescent_now(&self) -> bool {
        self.quiescent.as_ref().is_none_or(|check| check())
    }

    async fn check_loop(&self) {
        let mut shutdown = self.shutdown_tx.subscribe();
        // Shutdown must cut the loop at ANY await point — including mid
        // `check_once()` / `auto_apply_when_idle()` HTTP — so the whole body
        // races the flag rather than checking it between iterations.
        tokio::select! {
            _ = shutdown.wait_for(|stop| *stop) => {}
            _ = async {
                let mut checks = self.check_tx.subscribe();
                tokio::select! {
                    _ = tokio::time::sleep(CHECK_INITIAL_DELAY) => {}
                    _ = checks.changed() => {}
                }
                loop {
                    let ok = self.check_once().await;
                    if ok
                        && self.status_tx.borrow().update_available
                        && auto_update_enabled()
                        && let InstallKind::Managed { .. } = detect_install()
                    {
                        self.auto_apply_when_idle().await;
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(if ok { CHECK_INTERVAL } else { CHECK_RETRY }) => {}
                        _ = checks.changed() => {}
                    }
                }
            } => {}
        }
    }

    /// Sessions must never die to an update: pre-stage the download now
    /// (harmless while busy), wait for a quiet window (no live runs, no open
    /// terminals), then apply — which re-fetches the manifest (so a long defer
    /// lands on whatever is newest) and reuses the staged dir, keeping the
    /// idle→restart gap to well under a second.
    async fn auto_apply_when_idle(&self) {
        if let InstallKind::Managed { app_root } = detect_install() {
            match fetch_latest(&self.edge_url).await {
                Ok(manifest) if version_newer(&manifest.version, current_version()) => {
                    if let Err(err) = stage_headless(&self.edge_url, &manifest, &app_root).await {
                        tracing::warn!(error = %err, "auto-update staging failed");
                        return;
                    }
                }
                Ok(_) => return,
                Err(err) => {
                    tracing::warn!(error = %err, "auto-update staging fetch failed");
                    return;
                }
            }
        }
        let mut deferred = false;
        while !self.quiescent_now() {
            if !deferred {
                deferred = true;
                tracing::info!("auto-update deferred: sessions or terminals active");
            }
            tokio::time::sleep(IDLE_RECHECK).await;
        }
        match self.apply().await {
            Ok(version) => {
                tracing::info!(%version, "auto-update applied; service restarting")
            }
            Err(err) => tracing::warn!(error = %err, "auto-update failed"),
        }
    }

    /// One check; returns false on fetch failure (retry sooner).
    async fn check_once(&self) -> bool {
        match fetch_latest(&self.edge_url).await {
            Ok(manifest) => {
                let status = UpdateStatus {
                    current_version: current_version().to_string(),
                    update_available: version_newer(&manifest.version, current_version()),
                    latest_version: Some(manifest.version),
                    checked_at: Some(now_ms()),
                    error: None,
                };
                if status.update_available {
                    tracing::info!(
                        latest = status.latest_version.as_deref().unwrap_or(""),
                        current = %status.current_version,
                        "update available"
                    );
                }
                self.status_tx.send_replace(status);
                true
            }
            Err(err) => {
                tracing::debug!(error = %err, "update check failed");
                self.status_tx
                    .send_modify(|s| s.error = Some(format!("{err:#}")));
                false
            }
        }
    }

    /// Stage + apply the newest release on THIS device (managed installs only),
    /// then restart the service after a short delay so the caller's RPC reply
    /// flushes before systemd/launchd kills this process.
    pub async fn apply(&self) -> anyhow::Result<String> {
        let InstallKind::Managed { app_root } = detect_install() else {
            bail!(
                "this install is not update-managed — the desktop app updates from its UI; \
                 source builds update via git"
            );
        };
        let manifest = fetch_latest(&self.edge_url).await?;
        if !version_newer(&manifest.version, current_version()) {
            bail!("already up to date ({})", current_version());
        }
        stage_headless(&self.edge_url, &manifest, &app_root).await?;
        apply_headless(&app_root, &manifest.version)?;
        let rollback_root = app_root.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            if let Err(err) = restart_service() {
                tracing::error!(error = %err, "service restart failed; rolling back update");
                if let Err(rollback_err) = rollback_headless(&rollback_root) {
                    tracing::error!(error = %rollback_err, "automatic update rollback failed");
                } else if let Err(restart_err) = restart_service() {
                    tracing::error!(error = %restart_err, "service restart after rollback failed");
                }
            }
        });
        Ok(manifest.version)
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};

    #[test]
    fn version_compare() {
        assert!(version_newer("0.1.1", "0.1.0"));
        assert!(version_newer("0.2.0", "0.1.9"));
        assert!(version_newer("0.1.10", "0.1.9"));
        assert!(version_newer("v0.1.1", "0.1.0"));
        assert!(version_newer("0.2.0-beta.2", "0.2.0-beta.1"));
        assert!(!version_newer("0.1.0", "0.1.0"));
        assert!(!version_newer("0.1.0", "0.1.1"));
        assert!(!version_newer("0.2.0-beta.1", "0.2.0"));
        // Garbage never counts as newer.
        assert!(!version_newer("", "0.1.0"));
        assert!(!version_newer("nightly", "0.1.0"));
    }

    #[test]
    fn install_kind_detection() {
        assert_eq!(
            detect_install_from(
                Path::new("/home/u/.zeron/app/0.1.1/zeron"),
                Some(Path::new("/home/u")),
            ),
            InstallKind::Managed {
                app_root: PathBuf::from("/home/u/.zeron/app")
            }
        );
        assert_eq!(
            detect_install_from(
                Path::new("/Applications/Zeron.app/Contents/MacOS/zeron"),
                Some(Path::new("/Users/u")),
            ),
            InstallKind::MacApp {
                bundle: PathBuf::from("/Applications/Zeron.app")
            }
        );
        // A path merely containing `.app` without the bundle layout is not a bundle.
        assert_eq!(
            detect_install_from(Path::new("/tmp/foo.app/zeron"), None),
            InstallKind::Unmanaged
        );
        assert_eq!(
            detect_install_from(
                Path::new("/src/target/release/zeron"),
                Some(Path::new("/home/u"))
            ),
            InstallKind::Unmanaged
        );
    }

    #[test]
    fn artifact_names_match_packaging() {
        let (os, arch) = platform_key();
        assert!(headless_artifact("0.2.0").starts_with("zeron-0.2.0-"));
        assert_eq!(
            headless_artifact("0.2.0"),
            format!("zeron-0.2.0-{os}-{arch}.tar.gz")
        );
        assert!(mac_app_artifact("0.2.0").ends_with("-app.tar.gz"));
    }

    #[test]
    fn manifest_metadata_parses() {
        let full: Manifest = serde_json::from_str(
            r#"{"schemaVersion":1,"channel":"stable","version":"0.1.1","publishedAt":"2026-01-01T00:00:00Z","artifactBaseUrl":"https://example.com/v0.1.1","files":{"zeron-0.1.1-linux-x86_64.tar.gz":{"sha256":"abc","size":42}}}"#,
        )
        .unwrap();
        assert_eq!(full.version, "0.1.1");
        assert_eq!(full.files["zeron-0.1.1-linux-x86_64.tar.gz"].sha256, "abc");
        assert_eq!(full.files["zeron-0.1.1-linux-x86_64.tar.gz"].size, 42);
    }

    #[test]
    fn signed_manifest_rejects_tampering() {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let body = br#"{"schemaVersion":1,"channel":"stable","version":"1.0.0"}"#;
        let signature = signing.sign(body);
        let encoded = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());
        verify_manifest_with_key(body, &encoded, &signing.verifying_key()).unwrap();
        assert!(
            verify_manifest_with_key(
                br#"{"schemaVersion":1,"channel":"stable","version":"9.9.9"}"#,
                &encoded,
                &signing.verifying_key(),
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn headless_symlink_swap() {
        let tmp = tempfile::tempdir().unwrap();
        let app_root = tmp.path().join("app");
        for ver in ["0.1.0", "0.1.1"] {
            std::fs::create_dir_all(app_root.join(ver)).unwrap();
            std::fs::write(app_root.join(ver).join("zeron"), ver).unwrap();
        }
        apply_headless(&app_root, "0.1.0").unwrap();
        assert_eq!(
            std::fs::read_link(app_root.join("current")).unwrap(),
            app_root.join("0.1.0")
        );
        // Swap over an existing symlink.
        apply_headless(&app_root, "0.1.1").unwrap();
        assert_eq!(
            std::fs::read_link(app_root.join("current")).unwrap(),
            app_root.join("0.1.1")
        );
        assert_eq!(
            std::fs::read_link(app_root.join("previous")).unwrap(),
            app_root.join("0.1.0")
        );
        rollback_headless(&app_root).unwrap();
        assert_eq!(
            std::fs::read_link(app_root.join("current")).unwrap(),
            app_root.join("0.1.0")
        );
        // Unstaged version refuses.
        assert!(apply_headless(&app_root, "0.2.0").is_err());
    }
}
