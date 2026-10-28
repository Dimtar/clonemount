//! Runs one `rclone mount` process per configured mount and keeps it alive.
//!
//! A supervisor thread compares what should be mounted with what is running
//! once a second: it starts missing mounts, retries failed ones with backoff
//! (the network is often not up yet right after boot) and stops removed ones.

use crate::config::{MountConfig, Store};
use crate::rclone;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How long a fresh rclone process has to stay up before we call it mounted.
const SETTLE: Duration = Duration::from_secs(2);
/// How long rclone gets to unmount cleanly before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(8);

#[derive(Serialize, Clone)]
pub struct MountStatus {
    pub id: String,
    /// `unmounted`, `mounting`, `mounted`, `stopping` or `error`.
    pub state: &'static str,
    pub target: Option<String>,
    pub message: Option<String>,
}

#[derive(Default)]
struct Entry {
    wanted: bool,
    child: Option<Child>,
    started: Option<Instant>,
    stopping: Option<Instant>,
    target: Option<String>,
    error: Option<String>,
    failures: u32,
    retry_at: Option<Instant>,
}

pub struct Manager {
    store: Arc<Store>,
    entries: Mutex<HashMap<String, Entry>>,
}

impl Manager {
    pub fn new(store: Arc<Store>) -> Arc<Self> {
        kill_orphans(&store.rclone_conf());
        let wanted = store
            .settings()
            .mounts
            .into_iter()
            .filter(|m| m.auto_mount)
            .map(|m| (m.id, Entry { wanted: true, ..Default::default() }))
            .collect();
        let manager = Arc::new(Self { store, entries: Mutex::new(wanted) });
        let supervisor = manager.clone();
        thread::spawn(move || loop {
            supervisor.tick();
            thread::sleep(Duration::from_secs(1));
        });
        manager
    }

    pub fn set_wanted(&self, id: &str, wanted: bool) {
        let mut entries = self.entries.lock().unwrap();
        let entry = entries.entry(id.to_string()).or_default();
        entry.wanted = wanted;
        entry.failures = 0;
        entry.retry_at = None;
        entry.error = None;
    }

    pub fn set_all_wanted(&self, wanted: bool) {
        for m in self.store.settings().mounts {
            self.set_wanted(&m.id, wanted);
        }
    }

    pub fn statuses(&self) -> Vec<MountStatus> {
        let entries = self.entries.lock().unwrap();
        self.store
            .settings()
            .mounts
            .iter()
            .map(|m| {
                let e = entries.get(&m.id);
                let state = match e {
                    Some(e) if e.stopping.is_some() => "stopping",
                    Some(e) if e.child.is_some() => match e.started {
                        Some(t) if t.elapsed() < SETTLE => "mounting",
                        _ => "mounted",
                    },
                    Some(e) if e.error.is_some() => "error",
                    _ => "unmounted",
                };
                MountStatus {
                    id: m.id.clone(),
                    state,
                    target: e.and_then(|e| e.target.clone()),
                    message: e.and_then(|e| e.error.clone()),
                }
            })
            .collect()
    }

    /// Unmounts everything and waits (briefly) for rclone to finish. Called on quit.
    pub fn shutdown(&self) {
        let mut entries = self.entries.lock().unwrap();
        for e in entries.values_mut() {
            e.wanted = false;
            if let Some(child) = &mut e.child {
                terminate(child);
            }
        }
        let deadline = Instant::now() + STOP_GRACE;
        for e in entries.values_mut() {
            if let Some(child) = &mut e.child {
                while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
                    thread::sleep(Duration::from_millis(100));
                }
                if matches!(child.try_wait(), Ok(None)) {
                    let _ = child.kill();
                    let _ = child.wait();
                    if let Some(t) = &e.target {
                        force_unmount(t);
                    }
                }
            }
        }
    }

    fn tick(&self) {
        let settings = self.store.settings();
        let mut entries = self.entries.lock().unwrap();

        let used_targets: Vec<String> =
            entries.values().filter_map(|e| e.child.as_ref().and(e.target.clone())).collect();

        for cfg in &settings.mounts {
            let e = entries.entry(cfg.id.clone()).or_default();
            self.reap(cfg, e);

            if e.child.is_none() && e.wanted && e.retry_at.is_none_or(|t| Instant::now() >= t) {
                match rclone::find(&settings.rclone_path)
                    .ok_or_else(|| "rclone not found. Set its location in Settings.".to_string())
                    .and_then(|bin| self.start(&bin, cfg, &used_targets))
                {
                    Ok((child, target)) => {
                        e.child = Some(child);
                        e.target = Some(target);
                        e.started = Some(Instant::now());
                        e.error = None;
                    }
                    Err(msg) => self.failed(e, msg),
                }
            }

            if let Some(child) = &mut e.child {
                if !e.wanted && e.stopping.is_none() {
                    terminate(child);
                    e.stopping = Some(Instant::now());
                } else if e.stopping.is_some_and(|t| t.elapsed() > STOP_GRACE) {
                    let _ = child.kill();
                }
            }
        }

        // Mounts deleted from settings get stopped, then forgotten.
        for (id, e) in entries.iter_mut() {
            if settings.mounts.iter().any(|m| &m.id == id) {
                continue;
            }
            e.wanted = false;
            let Some(child) = &mut e.child else { continue };
            if !matches!(child.try_wait(), Ok(None)) {
                e.child = None;
            } else if e.stopping.is_none() {
                terminate(child);
                e.stopping = Some(Instant::now());
            } else if e.stopping.is_some_and(|t| t.elapsed() > STOP_GRACE) {
                let _ = child.kill();
            }
        }
        entries.retain(|id, e| e.child.is_some() || settings.mounts.iter().any(|m| &m.id == id));
    }

    /// Notices a rclone process that has exited and decides what that means.
    fn reap(&self, cfg: &MountConfig, e: &mut Entry) {
        let Some(child) = &mut e.child else { return };
        let Ok(Some(status)) = child.try_wait() else { return };
        e.child = None;
        let was_stopping = e.stopping.take().is_some();
        if was_stopping || !e.wanted {
            if was_stopping && !status.success() {
                if let Some(t) = &e.target {
                    force_unmount(t);
                }
            }
            e.error = None;
            e.failures = 0;
            return;
        }
        let msg = last_log_lines(&self.store.log_file(&cfg.id))
            .unwrap_or_else(|| format!("rclone exited ({status})"));
        self.failed(e, msg);
    }

    fn failed(&self, e: &mut Entry, msg: String) {
        e.failures += 1;
        let backoff = 5u64.saturating_mul(1 << e.failures.min(4)).min(60);
        e.retry_at = Some(Instant::now() + Duration::from_secs(backoff));
        e.error = Some(msg);
    }

    fn start(&self, bin: &Path, cfg: &MountConfig, used: &[String]) -> Result<(Child, String), String> {
        if cfg.remote.trim().is_empty() {
            return Err("No remote chosen".into());
        }
        let target = resolve_target(cfg, used)?;
        let log = File::create(self.store.log_file(&cfg.id)).map_err(|e| e.to_string())?;
        let extra = shell_words::split(&cfg.extra_args).map_err(|e| format!("Extra options: {e}"))?;

        let mut cmd = rclone::command(bin, &self.store.rclone_conf());
        cmd.arg(mount_subcommand())
            .arg(cfg.remote.trim())
            .arg(&target)
            .args(["--vfs-cache-mode", if cfg.cache_mode.is_empty() { "full" } else { &cfg.cache_mode }])
            .args(["--log-level", "NOTICE"]);
        if !cfg!(target_os = "linux") {
            cmd.args(["--volname", display_name(cfg)]);
        }
        if cfg.read_only {
            cmd.arg("--read-only");
        }
        cmd.args(extra)
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log);
        let child = cmd.spawn().map_err(|e| format!("Couldn't start rclone: {e}"))?;
        Ok((child, target))
    }
}

fn display_name(cfg: &MountConfig) -> &str {
    let name = cfg.name.trim();
    if name.is_empty() { cfg.remote.trim_end_matches(':') } else { name }
}

fn mount_subcommand() -> &'static str {
    // On macOS, rclone's NFS mount needs no kernel extension, so it works out
    // of the box. Use the FUSE mount only if macFUSE is installed.
    if cfg!(target_os = "macos") && !Path::new("/Library/Filesystems/macfuse.fs").exists() {
        "nfsmount"
    } else {
        "mount"
    }
}

fn is_drive_letter(t: &str) -> bool {
    let b = t.as_bytes();
    (b.len() == 2 || (b.len() == 3 && (b[2] == b'\\' || b[2] == b'/')))
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
}

/// Turns the configured target into a concrete one for this OS. A target
/// written for another OS (a drive letter on macOS, a path on Windows) is
/// treated as "automatic", so imported configs keep working.
fn resolve_target(cfg: &MountConfig, used: &[String]) -> Result<String, String> {
    let t = cfg.target.trim();
    if cfg!(windows) {
        if is_drive_letter(t) {
            return Ok(t[..2].to_ascii_uppercase());
        }
        for letter in ('D'..='Z').rev() {
            let drive = format!("{letter}:");
            if !used.contains(&drive) && !Path::new(&format!("{drive}\\")).exists() {
                return Ok(drive);
            }
        }
        return Err("No free drive letter".into());
    }

    let dir = if t.is_empty() || is_drive_letter(t) {
        home_dir().join("clonemount").join(sanitize(display_name(cfg)))
    } else if let Some(rest) = t.strip_prefix("~/") {
        home_dir().join(rest)
    } else {
        PathBuf::from(t)
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't create {}: {e}", dir.display()))?;
    let target = dir.to_string_lossy().into_owned();
    // A mount left behind by a previous run that was killed rather than quit.
    if is_mount_point(&dir) {
        force_unmount(&target);
    }
    Ok(target)
}

#[cfg(unix)]
fn is_mount_point(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(dir), dir.parent().map(std::fs::metadata)) {
        (Ok(d), Some(Ok(p))) => d.dev() != p.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn is_mount_point(_: &Path) -> bool {
    false
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || " -_.".contains(c) { c } else { '_' })
        .collect();
    if s.trim().is_empty() { "mount".into() } else { s.trim().to_string() }
}

fn last_log_lines(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return None;
    }
    Some(lines[lines.len().saturating_sub(3)..].join("\n"))
}

/// Asks rclone to unmount and exit. On Unix that's SIGTERM. Windows has no
/// equivalent for a console-less process, but WinFsp removes the drive when
/// the process ends, so there we just end it.
fn terminate(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    #[cfg(windows)]
    let _ = child.kill();
}

/// Ends rclone processes left running by a previous run of the app that was
/// killed instead of quit. They're recognised by clonemount's own config path,
/// which nothing else uses.
fn kill_orphans(conf: &Path) {
    let conf = conf.to_string_lossy();
    #[cfg(unix)]
    let mut cmd = {
        let pattern: String = conf
            .chars()
            .flat_map(|c| if c.is_alphanumeric() || c == '/' { vec![c] } else { vec!['\\', c] })
            .collect();
        let mut cmd = Command::new("pkill");
        cmd.arg("-f").arg(pattern);
        cmd
    };
    #[cfg(windows)]
    let mut cmd = {
        let script = format!(
            "Get-CimInstance Win32_Process -Filter \"Name='rclone.exe'\" | Where-Object {{ $_.CommandLine -like '*{}*' }} | ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force }}",
            conf.replace('\'', "''")
        );
        let mut cmd = Command::new("powershell");
        cmd.args(["-NoProfile", "-Command", &script]);
        rclone::hide_console(&mut cmd);
        cmd
    };
    let _ = cmd.stdout(Stdio::null()).stderr(Stdio::null()).status();
    // Give a SIGTERMed rclone a moment to unmount before we mount again.
    thread::sleep(Duration::from_millis(500));
}

/// Cleans up a mount point left behind by an rclone that had to be killed.
fn force_unmount(target: &str) {
    let attempts: &[&[&str]] = if cfg!(target_os = "macos") {
        &[&["umount", "-f"], &["diskutil", "unmount", "force"]]
    } else if cfg!(target_os = "linux") {
        &[&["fusermount3", "-uz"], &["fusermount", "-uz"], &["umount", "-l"]]
    } else {
        &[]
    };
    for args in attempts {
        let ok = Command::new(args[0])
            .args(&args[1..])
            .arg(target)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            break;
        }
    }
}

/// A missing system component the user has to install, with a download link.
#[derive(Serialize)]
pub struct Requirement {
    pub name: &'static str,
    pub url: &'static str,
    pub why: &'static str,
}

pub fn missing_requirements() -> Vec<Requirement> {
    let mut missing = vec![];
    if cfg!(windows) {
        let pf = std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());
        if !Path::new(&pf).join(r"WinFsp\bin").is_dir() {
            missing.push(Requirement {
                name: "WinFsp",
                url: "https://winfsp.dev/rel/",
                why: "Windows needs WinFsp to show rclone remotes as drives.",
            });
        }
    }
    if cfg!(target_os = "linux") && which::which("fusermount3").is_err() && which::which("fusermount").is_err() {
        missing.push(Requirement {
            name: "FUSE",
            url: "https://rclone.org/commands/rclone_mount/#linux",
            why: "Linux needs FUSE (the fuse3 package) to mount rclone remotes.",
        });
    }
    missing
}
