//! Updates from the Anomaly release service (`anomalyco/release`), as Hex's updater does
//! them, for macOS alone. A build from the Release workflow knows its channel (stable or
//! dev, `CLIP_BUILD_CHANNEL`) and polls `anoma.ly/release/clip/desktop/<channel>.json`: 10 s
//! after launch, then hourly (dev every 10 minutes), with its ETag. Versions are never
//! compared: whatever release is current on the channel and isn't this build gets
//! installed (`protocol::decide`), once this install's rollout bucket is inside the rollout.
//!
//! Installing: the zip is downloaded and checked against the manifest's size and SHA-256,
//! unpacked beside Clip.app (same volume), and must be `dev.vogel.clip` at the release's
//! version with a notarized Developer ID signature from team 9G68SMNHEU
//! (scripts/update-requirement.txt, which the Release workflow checks its builds against).
//! "Restart to Update" (or quitting) then swaps the two bundles in one step
//! (`renamex_np` with `RENAME_SWAP`); the old one is removed when the new one launches.
//! A Clip that can't replace itself (read-only, translocated, not in a Clip.app) offers the
//! DMG instead. Local builds don't update; `defaults write dev.vogel.clip DisableUpdates
//! -bool true` turns it off.

mod protocol;

use std::ffi::CString;
use std::fs;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use protocol::Decision;
use sha2::{Digest, Sha256};

const APP: &str = "clip";
const ARTIFACT: &str = "desktop";
const ORIGIN: &str = "https://anoma.ly/release";
const TARGET: &str = "macos-arm64";
const DOWNLOAD_TARGET: &str = "macos-arm64-dmg";
const BUNDLE_ID: &str = "dev.vogel.clip";
const REQUIREMENT: &str = include_str!("../scripts/update-requirement.txt");
/// Unpacked updates wait beside Clip.app under this prefix.
const STAGING_PREFIX: &str = ".clip-update-";

const STARTUP_DELAY: Duration = Duration::from_secs(10);
const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
const DEV_CHECK_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// After a failed check, the first retry; it doubles up to the check interval.
const RETRY: Duration = Duration::from_secs(60);
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

/// This build's version: the release's (`CLIP_VERSION`, set by scripts/bundle.sh), else the
/// crate's.
pub const VERSION: &str = match option_env!("CLIP_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// The channel this build was published on; `None` for a local build, which never updates.
fn channel() -> Option<&'static str> {
    option_env!("CLIP_BUILD_CHANNEL").filter(|c| matches!(*c, "stable" | "beta" | "dev"))
}

/// What the menu bar shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Checking,
    UpToDate,
    Downloading(String),
    /// Unpacked, verified, and swapped in on restart or quit.
    Ready(String),
    /// Clip can't replace itself where it is: the DMG to download.
    Manual { version: String, url: String },
    Failed(String),
}

struct Staged {
    /// The unpacked update, beside the installed bundle.
    update: PathBuf,
    installed: PathBuf,
}

#[derive(Default)]
struct Inner {
    status: Option<Status>,
    staged: Option<Staged>,
    /// "Check for Updates…" was chosen.
    check_now: bool,
}

struct Shared {
    inner: Mutex<Inner>,
    wake: Condvar,
    on_change: Box<dyn Fn(Status) + Send + Sync>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set(&self, status: Status) {
        self.lock().status = Some(status.clone());
        (self.on_change)(status);
    }

    /// Sleeps `delay`, or until a check is asked for. True when it was asked for.
    fn wait(&self, delay: Duration) -> bool {
        let inner = self.lock();
        let (mut inner, _) = self.wake.wait_timeout_while(inner, delay, |i| !i.check_now).unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut inner.check_now)
    }
}

static UPDATER: OnceLock<Arc<Shared>> = OnceLock::new();

/// Whether this build updates itself (a release build, not turned off).
pub fn enabled() -> bool {
    channel().is_some() && !disabled_by_policy()
}

fn disabled_by_policy() -> bool {
    Command::new("/usr/bin/defaults").args(["read", BUNDLE_ID, "DisableUpdates"]).stderr(Stdio::null()).output()
        .is_ok_and(|o| matches!(String::from_utf8_lossy(&o.stdout).trim(), "1" | "true" | "YES"))
}

/// Starts checking in the background. `on_change` hears every status, on the updater's
/// thread. Does nothing for a build that doesn't update.
pub fn start(on_change: impl Fn(Status) + Send + Sync + 'static) {
    let Some(channel) = channel() else { return };
    if disabled_by_policy() { return }
    let shared = Arc::new(Shared { inner: Mutex::default(), wake: Condvar::new(), on_change: Box::new(on_change) });
    if UPDATER.set(shared.clone()).is_err() { return }
    std::thread::Builder::new().name("clip-updater".into()).spawn(move || {
        finish_on_launch();
        run(&shared, channel);
    }).expect("updater thread");
}

/// "Check for Updates…": checks now.
pub fn check_now() {
    let Some(shared) = UPDATER.get() else { return };
    shared.lock().check_now = true;
    shared.wake.notify_all();
}

/// "Restart to Update": swaps in the staged update and opens it once this process has
/// exited. True when the caller should quit now.
pub fn restart_into_update() -> bool {
    let Some(shared) = UPDATER.get() else { return false };
    let Some(staged) = shared.lock().staged.take() else { return false };
    if let Err(e) = swap(&staged) {
        shared.set(Status::Failed(e));
        return false;
    }
    Command::new("/bin/sh")
        .args(["-c", "while kill -0 \"$CLIP_PID\" 2>/dev/null; do sleep 0.2; done; exec /usr/bin/open \"$CLIP_BUNDLE\""])
        .env("CLIP_PID", std::process::id().to_string())
        .env("CLIP_BUNDLE", &staged.installed)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .spawn().is_ok()
}

/// Quitting with an update ready installs it for the next launch.
pub fn install_on_quit() {
    let Some(shared) = UPDATER.get() else { return };
    if let Some(staged) = shared.lock().staged.take()
        && let Err(e) = swap(&staged) { eprintln!("clip: couldn't install the update at quit: {e}") }
}

fn run(shared: &Shared, channel: &str) {
    let interval = if channel == "dev" { DEV_CHECK_INTERVAL } else { CHECK_INTERVAL };
    let user_agent = format!("clip/{channel}/{VERSION}");
    let manifest_url = format!("{ORIGIN}/{APP}/{ARTIFACT}/{channel}.json");
    let mut cache = Cache::default();
    let mut retry = RETRY;
    let mut asked = shared.wait(STARTUP_DELAY);
    loop {
        // A staged update stays ready; only an asked-for check looks again.
        let ready = shared.lock().staged.is_some();
        if !ready || asked {
            if asked { shared.set(Status::Checking) }
            match check(shared, &mut cache, &manifest_url, &user_agent, channel) {
                Ok(status) => {
                    retry = RETRY;
                    shared.set(status);
                }
                Err(e) => {
                    shared.set(Status::Failed(e));
                    asked = shared.wait(retry);
                    retry = (retry * 2).min(interval);
                    continue;
                }
            }
        }
        asked = shared.wait(interval);
    }
}

fn check(shared: &Shared, cache: &mut Cache, url: &str, user_agent: &str, channel: &str) -> Result<Status, String> {
    let text = cache.fetch(url, user_agent)?;
    let install = protocol::Install {
        version: VERSION.into(),
        target: TARGET.into(),
        distribution: "direct".into(),
        rollout_bucket: rollout_bucket(),
        // A zip of the bundle the app can't reproduce: builds are told apart by version.
        sha256: None,
        channel: Some(channel.into()),
    };
    match protocol::decide(APP, ARTIFACT, channel, &text, &install) {
        Decision::Reject { reason } => Err(format!("the update manifest was rejected ({reason})")),
        Decision::None { .. } => Ok(Status::UpToDate),
        Decision::Notify { version, .. } => Ok(Status::Manual { version, url: "https://anoma.ly/release/clip".into() }),
        Decision::Install { version, target, .. } => {
            if let Some(Status::Ready(v)) = &shared.lock().status && *v == version { return Ok(Status::Ready(version)) }
            let Some(installed) = installed_bundle().filter(|b| replaceable(b)) else {
                let dmg = protocol::parse_manifest(&text).and_then(|m| m.release.target(DOWNLOAD_TARGET).cloned())
                    .and_then(|t| protocol::resolve_url(url, &t.url))
                    .unwrap_or_else(|| "https://anoma.ly/release/clip".into());
                return Ok(Status::Manual { version, url: dmg });
            };
            shared.set(Status::Downloading(version.clone()));
            let file_url = protocol::resolve_url(url, &target.url).ok_or("the update's URL is not on the release service")?;
            let zip = download(&file_url, user_agent, &target)?;
            let update = stage(&zip, &installed, &version)?;
            shared.lock().staged = Some(Staged { update, installed });
            Ok(Status::Ready(version))
        }
    }
}

fn agent(timeout: Duration) -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
    ureq::Agent::config_builder()
        .tls_config(TlsConfig::builder().provider(TlsProvider::NativeTls).root_certs(RootCerts::PlatformVerifier).build())
        .https_only(true)
        .max_redirects(10)
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(timeout))
        .build()
        .into()
}

/// The last manifest, revalidated with its ETag.
#[derive(Default)]
struct Cache {
    etag: Option<String>,
    body: Option<String>,
}

impl Cache {
    fn fetch(&mut self, url: &str, user_agent: &str) -> Result<String, String> {
        let mut request = agent(Duration::from_secs(60)).get(url).header("User-Agent", user_agent);
        if let (Some(etag), Some(_)) = (&self.etag, &self.body) { request = request.header("If-None-Match", etag.as_str()) }
        let mut response = request.call().map_err(|e| format!("couldn't reach the update service ({e})"))?;
        match response.status().as_u16() {
            304 => self.body.clone().ok_or_else(|| "the update service sent 304 for nothing cached".into()),
            200 => {
                let etag = response.headers().get("etag").and_then(|v| v.to_str().ok()).map(|v| v.trim().to_owned())
                    .filter(|e| !e.is_empty() && !e.chars().any(char::is_control));
                let mut text = String::new();
                response.body_mut().with_config().limit(MAX_MANIFEST_BYTES).reader().read_to_string(&mut text)
                    .map_err(|e| format!("couldn't read the update manifest ({e})"))?;
                self.etag = etag;
                self.body = Some(text.clone());
                Ok(text)
            }
            status => Err(format!("the update manifest request failed with HTTP {status}")),
        }
    }
}

fn sha256(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| e.to_string())?;
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Downloads a target into the cache, by its SHA-256, checking size and hash before any byte
/// is used; a finished download is reused.
fn download(url: &str, user_agent: &str, target: &protocol::Target) -> Result<PathBuf, String> {
    let dir = cache_dir().join("updates");
    // Only the one being fetched is kept.
    if let Ok(entries) = fs::read_dir(&dir) {
        for e in entries.flatten() { if e.file_name() != target.sha256.as_str() { let _ = fs::remove_dir_all(e.path()); } }
    }
    let folder = dir.join(&target.sha256);
    let file = folder.join("Clip.zip");
    let verify = |path: &Path| -> Result<(), String> {
        let size = fs::metadata(path).map_err(|e| e.to_string())?.len();
        if size != target.size { return Err(format!("the update has {size} bytes, expected {}", target.size)) }
        if sha256(path)? != target.sha256 { return Err("the update's SHA-256 doesn't match the manifest".into()) }
        Ok(())
    };
    if verify(&file).is_ok() { return Ok(file) }
    fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let partial = folder.join("Clip.zip.partial");
    let mut response = agent(Duration::from_secs(30 * 60)).get(url).header("User-Agent", user_agent).call()
        .map_err(|e| format!("couldn't download the update ({e})"))?;
    if response.status().as_u16() != 200 { return Err(format!("the update download failed with HTTP {}", response.status().as_u16())) }
    let mut out = fs::File::create(&partial).map_err(|e| e.to_string())?;
    std::io::copy(&mut response.body_mut().with_config().limit(target.size).reader(), &mut out)
        .map_err(|e| format!("the update download failed ({e})"))?;
    drop(out);
    if let Err(e) = verify(&partial) {
        let _ = fs::remove_file(&partial);
        return Err(e);
    }
    fs::rename(&partial, &file).map_err(|e| e.to_string())?;
    Ok(file)
}

fn cache_dir() -> PathBuf {
    crate::home().join("Library/Caches/Clip")
}

/// 0–99, drawn once per install and never sent anywhere.
fn rollout_bucket() -> u64 {
    let path = crate::home().join("Library/Application Support/Clip/rollout-bucket");
    if let Some(n) = fs::read_to_string(&path).ok().and_then(|s| s.trim().parse::<u64>().ok()).filter(|n| *n < 100) { return n }
    let mut bytes = [0u8; 8];
    let _ = fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes));
    let n = u64::from_le_bytes(bytes) % 100;
    let _ = fs::create_dir_all(path.parent().expect("has a parent"));
    let _ = fs::write(&path, n.to_string());
    n
}

/// The Clip.app this process runs from.
fn installed_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let bundle = exe.parent()?.parent()?.parent()?;
    (bundle.extension().is_some_and(|e| e == "app") && exe.parent()?.ends_with("Contents/MacOS")).then(|| bundle.to_path_buf())
}

/// Clip replaces its bundle by renaming inside the parent folder, so both must be writable
/// by this user, and it must not be a Gatekeeper App Translocation copy.
fn replaceable(bundle: &Path) -> bool {
    let writable = |path: &Path| CString::new(path.as_os_str().as_bytes())
        // access has no side effects; the string is NUL-terminated.
        .is_ok_and(|p| unsafe { libc::access(p.as_ptr(), libc::W_OK) } == 0);
    !bundle.components().any(|c| c.as_os_str() == "AppTranslocation") && bundle.parent().is_some_and(writable) && writable(bundle)
}

/// Removes what an earlier update left beside the bundle: the replaced one, now that the new
/// version runs, or an unpacked update never applied.
fn finish_on_launch() {
    let Some(parent) = installed_bundle().and_then(|b| b.parent().map(Path::to_path_buf)) else { return };
    let Ok(entries) = fs::read_dir(&parent) else { return };
    for e in entries.flatten() {
        if e.file_name().to_str().is_some_and(|n| n.starts_with(STAGING_PREFIX)) { let _ = fs::remove_dir_all(e.path()); }
    }
}

/// Unpacks the zip beside the installed bundle and checks it before anything runs from it.
fn stage(zip: &Path, installed: &Path, version: &str) -> Result<PathBuf, String> {
    let parent = installed.parent().ok_or("Clip.app has no parent folder")?;
    let staging = parent.join(format!("{STAGING_PREFIX}{version}"));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir(&staging).map_err(|e| e.to_string())?;
    let fail = |e: String| { let _ = fs::remove_dir_all(&staging); Err(e) };
    let unpacked = Command::new("/usr/bin/ditto").arg("-x").arg("-k").arg(zip).arg(&staging).stdin(Stdio::null()).status();
    if !unpacked.is_ok_and(|s| s.success()) { return fail("couldn't unpack the update".into()) }
    let update = staging.join("Clip.app");
    if let Err(e) = verify_bundle(&update, version) { return fail(e) }
    Ok(update)
}

/// The update's identity, version and code signature.
fn verify_bundle(bundle: &Path, version: &str) -> Result<(), String> {
    let plist = bundle.join("Contents/Info.plist");
    let value = |key: &str| -> String {
        Command::new("/usr/libexec/PlistBuddy").arg("-c").arg(format!("Print :{key}")).arg(&plist).stdin(Stdio::null()).output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).unwrap_or_default()
    };
    if value("CFBundleIdentifier") != BUNDLE_ID { return Err(format!("the update is not {BUNDLE_ID}")) }
    if value("CFBundleShortVersionString") != version { return Err(format!("the update is not version {version}")) }
    let out = Command::new("/usr/bin/codesign").args(["--verify", "--deep", "--strict"]).arg(format!("-R={}", REQUIREMENT.trim()))
        .arg(bundle).stdin(Stdio::null()).output().map_err(|e| format!("couldn't run codesign ({e})"))?;
    if !out.status.success() {
        return Err(format!("the update isn't signed by Clip's notarized Developer ID: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

/// Swaps the two bundles in one step: afterwards the installed path holds the update and
/// the staging folder the previous version.
fn swap(staged: &Staged) -> Result<(), String> {
    let from = CString::new(staged.update.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let to = CString::new(staged.installed.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    // Both paths are NUL-terminated and outlive the call.
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_SWAP) } != 0 {
        return Err(format!("couldn't swap in the updated Clip.app ({})", std::io::Error::last_os_error()));
    }
    Ok(())
}
