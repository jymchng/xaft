//! Finding a browser executable and launching it.
//!
//! # Which executable
//!
//! **Playwright family** (`[tools.playwright]`), in order:
//!
//! 1. `executable_path`, when set.
//! 2. `browser_channel`, when set: `chromium` / `chromium-headless-shell`
//!    select Playwright's own downloads; `chrome`, `chrome-beta`, `chrome-dev`,
//!    `chrome-canary`, `msedge`, `msedge-beta`, `msedge-dev` and
//!    `msedge-canary` select the branded install at the location Playwright
//!    itself looks in.
//! 3. Playwright's browser cache (`PLAYWRIGHT_BROWSERS_PATH`, else
//!    `~/.cache/ms-playwright` / `~/Library/Caches/ms-playwright` /
//!    `%LOCALAPPDATA%\ms-playwright`): the newest `chromium_headless_shell-*`
//!    for headless runs, the newest `chromium-*` otherwise — the same split
//!    Playwright makes.
//! 4. A Chromium-family browser on `PATH`. Playwright itself stops at step 3;
//!    xaft keeps going so a default build reaches a system browser.
//!
//! **CloakBrowser family** (`[tools.cloakbrowser]`, `transport = "local"`),
//! following the `cloakbrowser` package's own resolution:
//!
//! 1. `CLOAKBROWSER_BINARY_PATH`.
//! 2. The cache (`CLOAKBROWSER_CACHE_DIR`, else `~/.cloakbrowser`): with a
//!    licence key configured, the Pro build (`chromium-<v>-pro`) and never
//!    the free one — the package's own rule; otherwise the newest free build.
//!
//! xaft does not download CloakBrowser: when no build is installed the tools
//! report `binary_missing` with the install command.
//!
//! # How it is launched
//!
//! On Unix the browser is driven over `--remote-debugging-pipe` (file
//! descriptors 3 and 4), so no DevTools port is ever opened and the browser
//! exits when the pipe closes. Elsewhere it listens on an ephemeral loopback
//! port (`--remote-debugging-port=0`) announced on stderr.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use super::cdp::{TransportPumps, websocket_config, websocket_pumps};
use super::errors::{BrowserErrorKind, BrowserFailure, BrowserToolError};
use super::settings::{BrowserFamily, BrowserSettings};

/// The Playwright browser types xaft can drive.
const CDP_BROWSER_TYPE: &str = "chromium";

/// Where to look for browsers. Built from the process environment by
/// [`BrowserDiscovery::from_env`]; tests construct one directly so they never
/// mutate the real environment.
#[derive(Debug, Clone, Default)]
pub struct BrowserDiscovery {
    /// `$HOME` (or the platform home directory).
    pub home: Option<PathBuf>,
    /// `$PATH`.
    pub path: Option<OsString>,
    /// `PLAYWRIGHT_BROWSERS_PATH`.
    pub playwright_browsers_path: Option<PathBuf>,
    /// `XDG_CACHE_HOME`.
    pub xdg_cache_home: Option<PathBuf>,
    /// `%LOCALAPPDATA%`.
    pub local_app_data: Option<PathBuf>,
    /// `%PROGRAMFILES%`, `%PROGRAMFILES(X86)%`.
    pub program_files: Vec<PathBuf>,
    /// `CLOAKBROWSER_BINARY_PATH`.
    pub cloakbrowser_binary_path: Option<PathBuf>,
    /// `CLOAKBROWSER_CACHE_DIR`.
    pub cloakbrowser_cache_dir: Option<PathBuf>,
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

impl BrowserDiscovery {
    /// Read the real environment.
    pub fn from_env() -> Self {
        Self {
            home: env_path("HOME").or_else(|| env_path("USERPROFILE")),
            path: std::env::var_os("PATH"),
            playwright_browsers_path: env_path("PLAYWRIGHT_BROWSERS_PATH")
                .filter(|p| p.as_os_str() != "0"),
            xdg_cache_home: env_path("XDG_CACHE_HOME"),
            local_app_data: env_path("LOCALAPPDATA"),
            program_files: ["PROGRAMFILES", "PROGRAMFILES(X86)"]
                .iter()
                .filter_map(|n| env_path(n))
                .collect(),
            cloakbrowser_binary_path: env_path("CLOAKBROWSER_BINARY_PATH"),
            cloakbrowser_cache_dir: env_path("CLOAKBROWSER_CACHE_DIR"),
        }
    }

    /// Playwright's browser cache directory.
    pub fn playwright_cache_dir(&self) -> Option<PathBuf> {
        if let Some(p) = &self.playwright_browsers_path {
            return Some(p.clone());
        }
        if cfg!(target_os = "macos") {
            return self
                .home
                .as_ref()
                .map(|h| h.join("Library/Caches/ms-playwright"));
        }
        if cfg!(windows) {
            return self
                .local_app_data
                .as_ref()
                .map(|d| d.join("ms-playwright"));
        }
        self.xdg_cache_home
            .clone()
            .or_else(|| self.home.as_ref().map(|h| h.join(".cache")))
            .map(|c| c.join("ms-playwright"))
    }

    /// The newest executable among Playwright's `<prefix>-<revision>` dirs.
    fn playwright_download(&self, prefix: &str, candidates: &[&str]) -> Option<PathBuf> {
        let cache = self.playwright_cache_dir()?;
        let mut revisions: Vec<(u64, PathBuf)> = std::fs::read_dir(&cache)
            .ok()?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                let revision = name.strip_prefix(prefix)?.strip_prefix('-')?;
                Some((revision.parse::<u64>().ok()?, entry.path()))
            })
            .collect();
        revisions.sort_by_key(|r| std::cmp::Reverse(r.0));
        revisions.into_iter().find_map(|(_, dir)| {
            candidates
                .iter()
                .map(|rel| dir.join(rel))
                .find(|p| is_executable(p))
        })
    }

    /// Playwright's full Chromium download.
    pub fn playwright_chromium(&self) -> Option<PathBuf> {
        self.playwright_download(
            "chromium",
            &[
                "chrome-linux64/chrome",
                "chrome-linux/chrome",
                "chrome-mac-arm64/Chromium.app/Contents/MacOS/Chromium",
                "chrome-mac/Chromium.app/Contents/MacOS/Chromium",
                "chrome-win64/chrome.exe",
                "chrome-win/chrome.exe",
            ],
        )
    }

    /// Playwright's `chromium_headless_shell` download.
    pub fn playwright_headless_shell(&self) -> Option<PathBuf> {
        self.playwright_download(
            "chromium_headless_shell",
            &[
                "chrome-headless-shell-linux64/chrome-headless-shell",
                "chrome-linux/headless_shell",
                "chrome-headless-shell-mac-arm64/chrome-headless-shell",
                "chrome-headless-shell-mac-x64/chrome-headless-shell",
                "chrome-mac/headless_shell",
                "chrome-headless-shell-win64/chrome-headless-shell.exe",
                "chrome-win/headless_shell.exe",
            ],
        )
    }

    /// Where Playwright looks for a branded channel on this platform.
    pub fn channel_candidates(&self, channel: &str) -> Option<Vec<PathBuf>> {
        if cfg!(target_os = "macos") {
            let app = match channel {
                "chrome" => "Google Chrome",
                "chrome-beta" => "Google Chrome Beta",
                "chrome-dev" => "Google Chrome Dev",
                "chrome-canary" => "Google Chrome Canary",
                "msedge" => "Microsoft Edge",
                "msedge-beta" => "Microsoft Edge Beta",
                "msedge-dev" => "Microsoft Edge Dev",
                "msedge-canary" => "Microsoft Edge Canary",
                _ => return None,
            };
            return Some(vec![PathBuf::from(format!(
                "/Applications/{app}.app/Contents/MacOS/{app}"
            ))]);
        }
        if cfg!(windows) {
            let rel = match channel {
                "chrome" => "Google\\Chrome\\Application\\chrome.exe",
                "chrome-beta" => "Google\\Chrome Beta\\Application\\chrome.exe",
                "chrome-dev" => "Google\\Chrome Dev\\Application\\chrome.exe",
                "chrome-canary" => "Google\\Chrome SxS\\Application\\chrome.exe",
                "msedge" => "Microsoft\\Edge\\Application\\msedge.exe",
                "msedge-beta" => "Microsoft\\Edge Beta\\Application\\msedge.exe",
                "msedge-dev" => "Microsoft\\Edge Dev\\Application\\msedge.exe",
                "msedge-canary" => "Microsoft\\Edge SxS\\Application\\msedge.exe",
                _ => return None,
            };
            let roots = self.local_app_data.iter().chain(self.program_files.iter());
            return Some(roots.map(|r| r.join(rel)).collect());
        }
        let path = match channel {
            "chrome" => "/opt/google/chrome/chrome",
            "chrome-beta" => "/opt/google/chrome-beta/chrome",
            "chrome-dev" => "/opt/google/chrome-unstable/chrome",
            "msedge" => "/opt/microsoft/msedge/msedge",
            "msedge-beta" => "/opt/microsoft/msedge-beta/msedge",
            "msedge-dev" => "/opt/microsoft/msedge-dev/msedge",
            // Canary channels exist only on macOS and Windows.
            "chrome-canary" | "msedge-canary" => return Some(Vec::new()),
            _ => return None,
        };
        Some(vec![PathBuf::from(path)])
    }

    /// A Chromium-family browser on `PATH`.
    pub fn path_browser(&self) -> Option<PathBuf> {
        let path = self.path.as_ref()?;
        let names: &[&str] = if cfg!(windows) {
            &["chrome.exe", "msedge.exe", "chromium.exe"]
        } else {
            &[
                "chromium",
                "chromium-browser",
                "google-chrome",
                "google-chrome-stable",
                "chrome",
                "microsoft-edge",
                "microsoft-edge-stable",
                "msedge",
            ]
        };
        for dir in std::env::split_paths(path) {
            for name in names {
                let candidate = dir.join(name);
                if is_executable(&candidate) {
                    return Some(candidate);
                }
            }
        }
        None
    }

    /// The executable for the Playwright family, or the agent-facing refusal.
    pub fn find_playwright_executable(
        &self,
        settings: &BrowserSettings,
    ) -> Result<PathBuf, BrowserToolError> {
        let unavailable = |detail: &str| {
            BrowserToolError::new(
                BrowserErrorKind::BrowserUnavailable,
                format!("Playwright could not start its browser runtime: {detail}"),
            )
        };
        if settings.browser_type != CDP_BROWSER_TYPE {
            return Err(BrowserToolError::new(
                BrowserErrorKind::InvalidInput,
                "Configured Playwright browser type is unavailable. xaft drives Chromium over \
                 the DevTools protocol, so tools.playwright.browser_type must be 'chromium' to \
                 launch a browser.",
            ));
        }
        if !settings.executable_path.is_empty() {
            let path = PathBuf::from(&settings.executable_path);
            return if is_executable(&path) {
                Ok(path)
            } else {
                Err(unavailable(
                    "tools.playwright.executable_path is not an executable file.",
                ))
            };
        }
        if !settings.browser_channel.is_empty() {
            let found = match settings.browser_channel.as_str() {
                "chromium" => self.playwright_chromium(),
                "chromium-headless-shell" => self.playwright_headless_shell(),
                channel => match self.channel_candidates(channel) {
                    None => {
                        return Err(unavailable(
                            "tools.playwright.browser_channel names an unsupported channel.",
                        ));
                    }
                    Some(paths) => paths.into_iter().find(|p| is_executable(p)),
                },
            };
            return found
                .ok_or_else(|| unavailable("the configured browser channel is not installed."));
        }
        let downloaded = if settings.headless {
            self.playwright_headless_shell()
                .or_else(|| self.playwright_chromium())
        } else {
            self.playwright_chromium()
        };
        downloaded.or_else(|| self.path_browser()).ok_or_else(|| {
            unavailable(
                "no Chromium executable was found. Install one with `npx playwright install \
                 chromium`, or set tools.playwright.executable_path or \
                 tools.playwright.browser_channel.",
            )
        })
    }

    /// The CloakBrowser cache directory.
    pub fn cloakbrowser_cache_dir(&self) -> Option<PathBuf> {
        self.cloakbrowser_cache_dir
            .clone()
            .or_else(|| self.home.as_ref().map(|h| h.join(".cloakbrowser")))
    }

    /// The executable for the CloakBrowser family, or the agent-facing refusal.
    pub fn find_cloakbrowser_executable(
        &self,
        licensed: bool,
    ) -> Result<PathBuf, BrowserToolError> {
        let missing = |detail: &str| {
            BrowserToolError::new(
                BrowserErrorKind::BinaryMissing,
                format!("CloakBrowser browser runtime is unavailable{detail}"),
            )
        };
        if let Some(path) = &self.cloakbrowser_binary_path {
            return if is_executable(path) {
                Ok(path.clone())
            } else {
                Err(missing(
                    ": CLOAKBROWSER_BINARY_PATH does not name an executable file.",
                ))
            };
        }
        let found = self.cloakbrowser_cache_dir().and_then(|cache| {
            let tag = cloakbrowser_platform_tag()?;
            if licensed {
                cloakbrowser_build(&cache, &format!("latest_pro_version_{tag}"), true)
            } else {
                cloakbrowser_build(&cache, &format!("latest_version_{tag}"), false)
                    .or_else(|| cloakbrowser_build(&cache, "latest_version", false))
            }
        });
        found.ok_or_else(|| {
            if licensed {
                missing(
                    ": a licence key is configured but no CloakBrowser Pro build is installed. \
                     Run `cloakbrowser install` with the key set, or point \
                     CLOAKBROWSER_BINARY_PATH at a CloakBrowser binary.",
                )
            } else {
                missing(
                    ". Install it (`pip install cloakbrowser && cloakbrowser install`), point \
                     CLOAKBROWSER_BINARY_PATH at a CloakBrowser binary, or set \
                     tools.browser_backend = \"playwright\" to use a stock Chromium.",
                )
            }
        })
    }
}

/// The `cloakbrowser` package's platform tag for this host.
fn cloakbrowser_platform_tag() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("linux-x64"),
        ("linux", "aarch64") => Some("linux-arm64"),
        ("macos", "aarch64") => Some("darwin-arm64"),
        ("macos", "x86_64") => Some("darwin-x64"),
        ("windows", "x86_64") => Some("windows-x64"),
        _ => None,
    }
}

/// The executable inside one `chromium-<version>[-pro]` directory.
fn cloakbrowser_binary(dir: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        dir.join("Chromium.app/Contents/MacOS/Chromium")
    } else if cfg!(windows) {
        dir.join("chrome.exe")
    } else {
        dir.join("chrome")
    }
}

fn version_tuple(version: &str) -> Option<Vec<u64>> {
    version.split('.').map(|p| p.parse().ok()).collect()
}

/// The build a marker file names, else the newest installed build.
fn cloakbrowser_build(cache: &Path, marker: &str, pro: bool) -> Option<PathBuf> {
    let suffix = if pro { "-pro" } else { "" };
    if let Ok(version) = std::fs::read_to_string(cache.join(marker)) {
        let version = version.trim();
        if version_tuple(version).is_some() {
            let binary = cloakbrowser_binary(&cache.join(format!("chromium-{version}{suffix}")));
            if is_executable(&binary) {
                return Some(binary);
            }
        }
    }
    let mut builds: Vec<(Vec<u64>, PathBuf)> = std::fs::read_dir(cache)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let rest = name.strip_prefix("chromium-")?;
            let version = if pro {
                rest.strip_suffix("-pro")?
            } else if rest.ends_with("-pro") {
                return None;
            } else {
                rest
            };
            Some((version_tuple(version)?, entry.path()))
        })
        .collect();
    builds.sort_by(|a, b| b.0.cmp(&a.0));
    builds
        .into_iter()
        .map(|(_, dir)| cloakbrowser_binary(&dir))
        .find(|p| is_executable(p))
}

/// Whether `path` is an existing executable file.
pub fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The Chromium switches every launch carries — Playwright's defaults, minus
/// `--disable-popup-blocking` (popups are closed on sight anyway, and the
/// built-in blocker is a second line of defence).
fn base_switches(settings: &BrowserSettings) -> Vec<String> {
    let mut args: Vec<String> = [
        "--disable-field-trial-config",
        "--disable-background-networking",
        "--disable-background-timer-throttling",
        "--disable-backgrounding-occluded-windows",
        "--disable-back-forward-cache",
        "--disable-breakpad",
        "--disable-client-side-phishing-detection",
        "--disable-component-extensions-with-background-pages",
        "--disable-component-update",
        "--no-default-browser-check",
        "--disable-default-apps",
        "--disable-dev-shm-usage",
        "--disable-extensions",
        "--disable-features=ImprovedCookieControls,LazyFrameLoading,GlobalMediaControls,\
         DestroyProfileOnBrowserClose,MediaRouter,DialMediaRouteProvider,AcceptCHFrame,\
         AutoExpandDetailsElement,CertificateTransparencyComponentUpdater,\
         AvoidUnnecessaryBeforeUnloadCheckSync,Translate,HttpsUpgrades,PaintHolding,\
         ThirdPartyStoragePartitioning,LensOverlay,PlzDedicatedWorker",
        "--allow-pre-commit-input",
        "--disable-hang-monitor",
        "--disable-ipc-flooding-protection",
        "--disable-prompt-on-repost",
        "--disable-renderer-backgrounding",
        "--force-color-profile=srgb",
        "--metrics-recording-only",
        "--no-first-run",
        "--password-store=basic",
        "--use-mock-keychain",
        "--no-service-autorun",
        "--export-tagged-pdf",
        "--disable-search-engine-choice-screen",
        "--disable-sync",
        "--no-sandbox",
        "--no-startup-window",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    match settings.family {
        // Playwright marks its browsers as automated (`navigator.webdriver`).
        BrowserFamily::Playwright => args.push("--enable-automation".into()),
        // The cloakbrowser package suppresses exactly that switch, and adds
        // `--ignore-gpu-blocklist` for headed runs.
        BrowserFamily::CloakBrowser => {
            if !settings.headless {
                args.push("--ignore-gpu-blocklist".into());
            }
        }
    }
    if settings.headless {
        args.push("--headless".into());
        args.push("--hide-scrollbars".into());
        args.push("--mute-audio".into());
    }
    args
}

/// Everything needed to start one browser process.
#[derive(Debug, Clone)]
pub(crate) struct LaunchSpec {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub use_pipe: bool,
    pub timeout: Duration,
}

impl LaunchSpec {
    /// The command line for `settings` with its profile at `user_data_dir`.
    pub fn new(
        executable: PathBuf,
        settings: &BrowserSettings,
        user_data_dir: &Path,
        extra_args: &[String],
    ) -> Self {
        let mut args = base_switches(settings);
        args.push(format!("--user-data-dir={}", user_data_dir.display()));
        args.extend(extra_args.iter().cloned());
        Self {
            executable,
            args,
            env: Vec::new(),
            use_pipe: cfg!(unix),
            timeout: Duration::from_secs(60),
        }
    }
}

/// A launched browser process and the transport to it.
pub(crate) struct LaunchedBrowser {
    pub child: Child,
    pub pumps: TransportPumps,
    pub stderr_tail: Arc<Mutex<VecDeque<String>>>,
    pub stderr_task: tokio::task::JoinHandle<()>,
}

const STDERR_TAIL_LINES: usize = 40;

fn launch_failure() -> BrowserFailure {
    BrowserFailure::Execution("browser process could not be started")
}

/// Start the browser described by `spec`.
pub(crate) async fn launch(spec: &LaunchSpec) -> Result<LaunchedBrowser, BrowserFailure> {
    let mut command = Command::new(&spec.executable);
    command
        .args(&spec.args)
        .envs(spec.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        // Its own process group: a Ctrl+C aimed at xaft's terminal must not
        // kill the browser out from under a cancellation-aware tool, and
        // `kill_process_group` can reap renderers with the browser.
        command.process_group(0);
    }

    if spec.use_pipe {
        #[cfg(unix)]
        {
            return launch_with_pipe(command).await;
        }
    }
    launch_with_port(command, spec.timeout).await
}

fn drain_stderr(
    stderr: tokio::process::ChildStderr,
    tail: Arc<Mutex<VecDeque<String>>>,
    announce: Option<tokio::sync::oneshot::Sender<String>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut announce = announce;
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(rest) = line.strip_prefix("DevTools listening on ") {
                if let Some(tx) = announce.take() {
                    let _ = tx.send(rest.trim().to_string());
                }
            }
            let mut tail = tail.lock().expect("stderr tail poisoned");
            if tail.len() == STDERR_TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(line);
        }
    })
}

#[cfg(unix)]
fn cloexec_pipe() -> std::io::Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    use std::os::fd::{FromRawFd, OwnedFd};
    let mut fds = [0 as libc::c_int; 2];
    #[cfg(target_os = "linux")]
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    #[cfg(not(target_os = "linux"))]
    let rc = unsafe {
        let rc = libc::pipe(fds.as_mut_ptr());
        if rc == 0 {
            libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(fds[1], libc::F_SETFD, libc::FD_CLOEXEC);
        }
        rc
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `pipe` returned two fresh descriptors this function now owns.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

#[cfg(unix)]
async fn launch_with_pipe(mut command: Command) -> Result<LaunchedBrowser, BrowserFailure> {
    use std::os::fd::AsRawFd;
    command.arg("--remote-debugging-pipe");
    // The browser reads commands from fd 3 and writes responses to fd 4.
    let (child_read, parent_write) = cloexec_pipe().map_err(|_| launch_failure())?;
    let (parent_read, child_write) = cloexec_pipe().map_err(|_| launch_failure())?;
    let child_read_fd = child_read.as_raw_fd();
    let child_write_fd = child_write.as_raw_fd();
    // SAFETY: the closure runs between fork and exec and only calls the
    // async-signal-safe `fcntl` and `dup2`. Both descriptors are first copied
    // above 4 (close-on-exec), so placing one on 3 cannot clobber the other
    // before it is placed on 4; `dup2` clears close-on-exec on 3 and 4.
    unsafe {
        command.pre_exec(move || {
            let read = libc::fcntl(child_read_fd, libc::F_DUPFD_CLOEXEC, 10);
            if read < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let write = libc::fcntl(child_write_fd, libc::F_DUPFD_CLOEXEC, 10);
            if write < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::dup2(read, 3) < 0 || libc::dup2(write, 4) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|_| launch_failure())?;
    drop(child_read);
    drop(child_write);
    let reader = tokio::net::unix::pipe::Receiver::from_owned_fd(parent_read)
        .map_err(|_| launch_failure())?;
    let writer = tokio::net::unix::pipe::Sender::from_owned_fd(parent_write)
        .map_err(|_| launch_failure())?;
    let tail = Arc::new(Mutex::new(VecDeque::new()));
    let stderr = child.stderr.take().ok_or_else(launch_failure)?;
    let stderr_task = drain_stderr(stderr, Arc::clone(&tail), None);
    Ok(LaunchedBrowser {
        child,
        pumps: super::cdp::pipe_pumps(reader, writer),
        stderr_tail: tail,
        stderr_task,
    })
}

async fn launch_with_port(
    mut command: Command,
    timeout: Duration,
) -> Result<LaunchedBrowser, BrowserFailure> {
    command.arg("--remote-debugging-port=0");
    let mut child = command.spawn().map_err(|_| launch_failure())?;
    let tail = Arc::new(Mutex::new(VecDeque::new()));
    let stderr = child.stderr.take().ok_or_else(launch_failure)?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let stderr_task = drain_stderr(stderr, Arc::clone(&tail), Some(tx));
    let ws_url = match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(url)) => url,
        Ok(Err(_)) => return Err(launch_failure()),
        Err(_) => return Err(BrowserFailure::Timeout),
    };
    if !is_loopback_ws_url(&ws_url) {
        return Err(launch_failure());
    }
    let (socket, _) = tokio_tungstenite::connect_async_with_config(
        ws_url.as_str(),
        Some(websocket_config()),
        false,
    )
    .await
    .map_err(|_| launch_failure())?;
    Ok(LaunchedBrowser {
        child,
        pumps: websocket_pumps(socket),
        stderr_tail: tail,
        stderr_task,
    })
}

/// Whether a DevTools WebSocket URL points at this machine over plain `ws`.
pub(crate) fn is_loopback_ws_url(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != "ws" {
        return false;
    }
    match parsed.host() {
        Some(url::Host::Domain(d)) => d == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Kill the browser and every process in its group (renderers, GPU, zygote).
pub(crate) fn kill_process_tree(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: plain syscall; the group was created by `process_group(0)`
        // at spawn, so its id is the browser's pid.
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
    }
    let _ = child.start_kill();
}

/// A minimal HTTP/1.1 `GET` of a loopback JSON document (the DevTools
/// `/json/version` endpoint), without pulling in an HTTP client.
pub(crate) async fn http_get_json(
    host: &str,
    port: u16,
    path: &str,
    timeout: Duration,
) -> Result<serde_json::Value, BrowserFailure> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let unavailable = || {
        BrowserFailure::tool(
            BrowserErrorKind::BrowserUnavailable,
            "CDP browser endpoint is unavailable.",
        )
    };
    let work = async {
        let connect_host = host.trim_start_matches('[').trim_end_matches(']');
        let mut stream = tokio::net::TcpStream::connect((connect_host, port))
            .await
            .map_err(|_| unavailable())?;
        let host_header = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host_header}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|_| unavailable())?;
        // Read only as much as the response frames. Chromium's DevTools HTTP
        // server keeps the connection open despite `Connection: close`, so
        // reading to end-of-stream would wait out the whole deadline.
        let mut raw = Vec::new();
        let mut chunk = [0u8; 8192];
        let payload = loop {
            if let Some(payload) = complete_http_body(&raw) {
                break payload.map_err(|_| unavailable())?;
            }
            let n = stream.read(&mut chunk).await.map_err(|_| unavailable())?;
            if n == 0 {
                // End of stream: whatever arrived is the whole response.
                break eof_http_body(&raw).ok_or_else(unavailable)?;
            }
            raw.extend_from_slice(&chunk[..n]);
            if raw.len() > 1 << 20 {
                return Err(unavailable());
            }
        };
        serde_json::from_str(&payload).map_err(|_| unavailable())
    };
    tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| unavailable())?
}

/// Split a raw response into a 200 status check, its headers and its body
/// bytes; `None` until the header block is complete.
fn split_http_response(raw: &[u8]) -> Option<Result<(String, &[u8]), ()>> {
    let end = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..end]).to_ascii_lowercase();
    if !head.starts_with("http/1.1 200") && !head.starts_with("http/1.0 200") {
        return Some(Err(()));
    }
    Some(Ok((head, &raw[end + 4..])))
}

fn header_value<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == name).then(|| value.trim())
    })
}

/// The body once the response is complete by its own framing
/// (`Content-Length` or chunked); `None` while more bytes are needed.
fn complete_http_body(raw: &[u8]) -> Option<Result<String, ()>> {
    let (head, body) = match split_http_response(raw)? {
        Ok(parts) => parts,
        Err(()) => return Some(Err(())),
    };
    if header_value(&head, "transfer-encoding").is_some_and(|v| v.contains("chunked")) {
        let text = String::from_utf8_lossy(body);
        return decode_chunked(&text).map(Ok);
    }
    let length: usize = header_value(&head, "content-length")?.parse().ok()?;
    if body.len() < length {
        return None;
    }
    Some(String::from_utf8(body[..length].to_vec()).map_err(|_| ()))
}

/// The body of a response whose end is the end of the stream.
fn eof_http_body(raw: &[u8]) -> Option<String> {
    let (head, body) = split_http_response(raw)?.ok()?;
    if header_value(&head, "transfer-encoding").is_some_and(|v| v.contains("chunked")) {
        return decode_chunked(&String::from_utf8_lossy(body));
    }
    String::from_utf8(body.to_vec()).ok()
}

fn decode_chunked(body: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = body;
    loop {
        let (size_line, after) = rest.split_once("\r\n")?;
        let size = usize::from_str_radix(size_line.split(';').next()?.trim(), 16).ok()?;
        if size == 0 {
            return Some(out);
        }
        out.push_str(after.get(..size)?);
        rest = after.get(size..)?.strip_prefix("\r\n")?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use xaft_config::{CloakBrowserConfig, PlaywrightConfig};

    fn executable(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn discovery(home: &Path) -> BrowserDiscovery {
        BrowserDiscovery {
            home: Some(home.to_path_buf()),
            ..Default::default()
        }
    }

    fn playwright(f: impl FnOnce(&mut PlaywrightConfig)) -> BrowserSettings {
        let mut config = PlaywrightConfig::default();
        f(&mut config);
        BrowserSettings::from_playwright(&config)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn playwright_downloads_prefer_the_newest_revision_and_the_headless_shell() {
        let home = TempDir::new().unwrap();
        let cache = home.path().join(".cache/ms-playwright");
        executable(&cache.join("chromium-1100/chrome-linux64/chrome"));
        executable(&cache.join("chromium-1228/chrome-linux64/chrome"));
        executable(&cache.join(
            "chromium_headless_shell-1228/chrome-headless-shell-linux64/chrome-headless-shell",
        ));
        let d = discovery(home.path());
        let headless = d.find_playwright_executable(&playwright(|_| {})).unwrap();
        assert!(headless.ends_with("chrome-headless-shell"), "{headless:?}");
        let headed = d
            .find_playwright_executable(&playwright(|c| c.headless = false))
            .unwrap();
        assert!(
            headed.starts_with(cache.join("chromium-1228")),
            "{headed:?}"
        );
        let channel = d
            .find_playwright_executable(&playwright(|c| c.browser_channel = "chromium".into()))
            .unwrap();
        assert!(channel.starts_with(cache.join("chromium-1228")));
    }

    #[test]
    fn an_explicit_executable_wins_and_must_exist() {
        let home = TempDir::new().unwrap();
        let exe = home.path().join("bin/my-chrome");
        executable(&exe);
        let d = discovery(home.path());
        let found = d
            .find_playwright_executable(&playwright(|c| {
                c.executable_path = exe.display().to_string()
            }))
            .unwrap();
        assert_eq!(found, exe);
        let err = d
            .find_playwright_executable(&playwright(|c| c.executable_path = "/no/such".into()))
            .unwrap_err();
        assert_eq!(err.kind, BrowserErrorKind::BrowserUnavailable);
        assert!(
            err.message
                .starts_with("Playwright could not start its browser runtime")
        );
        assert!(!err.message.contains("/no/such"), "never echo a path");
    }

    #[test]
    fn non_chromium_browser_types_are_reported_not_attempted() {
        let d = BrowserDiscovery::default();
        let err = d
            .find_playwright_executable(&playwright(|c| c.browser_type = "firefox".into()))
            .unwrap_err();
        assert_eq!(err.kind, BrowserErrorKind::InvalidInput);
        assert!(
            err.message
                .starts_with("Configured Playwright browser type is unavailable.")
        );
    }

    #[test]
    fn unknown_and_missing_channels_are_distinguished() {
        let d = BrowserDiscovery::default();
        let err = d
            .find_playwright_executable(&playwright(|c| c.browser_channel = "netscape".into()))
            .unwrap_err();
        assert!(err.message.contains("unsupported channel"));
        let err = d
            .find_playwright_executable(&playwright(|c| c.browser_channel = "chrome-canary".into()))
            .unwrap_err();
        assert!(
            err.message.contains("not installed") || err.message.contains("unsupported"),
            "{}",
            err.message
        );
    }

    #[test]
    fn the_path_is_the_last_resort() {
        let home = TempDir::new().unwrap();
        let bin = home.path().join("bin");
        let name = if cfg!(windows) {
            "chrome.exe"
        } else {
            "chromium"
        };
        executable(&bin.join(name));
        let d = BrowserDiscovery {
            home: Some(home.path().to_path_buf()),
            path: Some(bin.clone().into_os_string()),
            ..Default::default()
        };
        assert_eq!(
            d.find_playwright_executable(&playwright(|_| {})).unwrap(),
            bin.join(name)
        );
        let none = discovery(home.path());
        let err = none
            .find_playwright_executable(&playwright(|_| {}))
            .unwrap_err();
        assert!(err.message.contains("npx playwright install chromium"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cloakbrowser_builds_follow_the_package_resolution() {
        let home = TempDir::new().unwrap();
        let cache = home.path().join(".cloakbrowser");
        executable(&cache.join("chromium-145.0.7632.109.2/chrome"));
        executable(&cache.join("chromium-146.0.7680.177.5/chrome"));
        let d = discovery(home.path());
        let free = d.find_cloakbrowser_executable(false).unwrap();
        assert!(free.starts_with(cache.join("chromium-146.0.7680.177.5")));

        // A licence key never falls back to the free build.
        let err = d.find_cloakbrowser_executable(true).unwrap_err();
        assert_eq!(err.kind, BrowserErrorKind::BinaryMissing);
        assert!(err.message.contains("Pro build"));
        executable(&cache.join("chromium-148.0.1.2.3-pro/chrome"));
        let pro = d.find_cloakbrowser_executable(true).unwrap();
        assert!(pro.starts_with(cache.join("chromium-148.0.1.2.3-pro")));

        // A marker file names the build to use.
        std::fs::write(cache.join("latest_version_linux-x64"), "145.0.7632.109.2\n").unwrap();
        let marked = d.find_cloakbrowser_executable(false).unwrap();
        assert!(marked.starts_with(cache.join("chromium-145.0.7632.109.2")));
    }

    #[test]
    fn cloakbrowser_binary_override_and_absence() {
        let home = TempDir::new().unwrap();
        let exe = home.path().join("cloak");
        executable(&exe);
        let d = BrowserDiscovery {
            cloakbrowser_binary_path: Some(exe.clone()),
            ..discovery(home.path())
        };
        assert_eq!(d.find_cloakbrowser_executable(true).unwrap(), exe);
        let err = BrowserDiscovery {
            cloakbrowser_binary_path: Some(home.path().join("missing")),
            ..Default::default()
        }
        .find_cloakbrowser_executable(false)
        .unwrap_err();
        assert!(err.message.contains("CLOAKBROWSER_BINARY_PATH"));
        let err = discovery(home.path())
            .find_cloakbrowser_executable(false)
            .unwrap_err();
        assert_eq!(err.kind, BrowserErrorKind::BinaryMissing);
        assert!(
            err.message
                .starts_with("CloakBrowser browser runtime is unavailable")
        );
        assert!(err.message.contains("cloakbrowser install"));
    }

    #[test]
    fn switches_follow_the_family() {
        let pw = BrowserSettings::from_playwright(&PlaywrightConfig::default());
        let args = base_switches(&pw);
        assert!(args.contains(&"--enable-automation".to_string()));
        assert!(args.contains(&"--headless".to_string()));
        assert!(!args.iter().any(|a| a == "--disable-popup-blocking"));
        let cloak = BrowserSettings::from_cloakbrowser(&CloakBrowserConfig {
            headless: false,
            ..Default::default()
        });
        let args = base_switches(&cloak);
        assert!(!args.contains(&"--enable-automation".to_string()));
        assert!(!args.contains(&"--headless".to_string()));
        assert!(args.contains(&"--ignore-gpu-blocklist".to_string()));
    }

    #[test]
    fn only_plain_loopback_websockets_are_accepted() {
        assert!(is_loopback_ws_url("ws://127.0.0.1:9222/devtools/browser/x"));
        assert!(is_loopback_ws_url("ws://localhost:9222/devtools/browser/x"));
        assert!(is_loopback_ws_url("ws://[::1]:9222/devtools/browser/x"));
        assert!(!is_loopback_ws_url("ws://10.0.0.5:9222/devtools/browser/x"));
        assert!(!is_loopback_ws_url(
            "wss://127.0.0.1:9222/devtools/browser/x"
        ));
        assert!(!is_loopback_ws_url("not a url"));
    }

    #[test]
    fn responses_complete_by_their_own_framing() {
        let full = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
        assert_eq!(complete_http_body(full), Some(Ok("{}".to_string())));
        assert_eq!(complete_http_body(&full[..full.len() - 1]), None);
        assert_eq!(complete_http_body(b"HTTP/1.1 200 OK\r\nContent-"), None);
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n";
        assert_eq!(complete_http_body(chunked), Some(Ok("{}".to_string())));
        assert_eq!(
            complete_http_body(b"HTTP/1.1 404 Not Found\r\n\r\n"),
            Some(Err(()))
        );
        // No framing: complete only at end of stream.
        let unframed = b"HTTP/1.1 200 OK\r\n\r\n{\"a\":1}";
        assert_eq!(complete_http_body(unframed), None);
        assert_eq!(eof_http_body(unframed).as_deref(), Some("{\"a\":1}"));
    }

    #[test]
    fn chunked_bodies_decode() {
        assert_eq!(
            decode_chunked("4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n").unwrap(),
            "Wikipedia"
        );
        assert!(decode_chunked("zz\r\n").is_none());
    }

    #[tokio::test]
    async fn http_get_json_reads_a_loopback_document() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await.unwrap();
            let body = r#"{"webSocketDebuggerUrl":"ws://127.0.0.1:1/devtools/browser/x"}"#;
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(reply.as_bytes()).await.unwrap();
            // Like Chromium's DevTools server: answer, then keep the
            // connection open. The client must stop at Content-Length.
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(socket);
        });
        let started = std::time::Instant::now();
        let json = http_get_json("127.0.0.1", port, "/json/version", Duration::from_secs(5))
            .await
            .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "waited for end-of-stream"
        );
        assert_eq!(
            json["webSocketDebuggerUrl"],
            "ws://127.0.0.1:1/devtools/browser/x"
        );
        // Nothing listening: a safe, fixed refusal.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_port = closed.local_addr().unwrap().port();
        drop(closed);
        let err = http_get_json(
            "127.0.0.1",
            dead_port,
            "/json/version",
            Duration::from_secs(5),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err,
            BrowserFailure::tool(
                BrowserErrorKind::BrowserUnavailable,
                "CDP browser endpoint is unavailable."
            )
        );
    }
}
