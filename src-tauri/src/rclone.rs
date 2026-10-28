//! Finding and running the rclone binary.

use std::path::{Path, PathBuf};
use std::process::Command;

const EXE: &str = if cfg!(windows) { "rclone.exe" } else { "rclone" };

/// The custom path if set, else the copy bundled next to the app, else PATH.
pub fn find(custom: &str) -> Option<PathBuf> {
    if !custom.is_empty() {
        let p = PathBuf::from(custom);
        return p.is_file().then_some(p);
    }
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(EXE)))
        .filter(|p| p.is_file());
    if bundled.is_some() {
        return bundled;
    }
    // Apps launched from Finder don't inherit the shell's PATH, so also look
    // where Homebrew and the rclone install script put it.
    which::which(EXE).ok().or_else(|| {
        ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"]
            .iter()
            .map(|d| Path::new(d).join(EXE))
            .find(|p| p.is_file())
    })
}

pub fn command(bin: &Path, conf: &Path) -> Command {
    let mut cmd = Command::new(bin);
    cmd.arg("--config").arg(conf);
    hide_console(&mut cmd);
    cmd
}

pub fn run(bin: &Path, conf: &Path, args: &[&str]) -> Result<String, String> {
    let out = command(bin, conf).args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.trim().lines().last().unwrap_or("rclone failed").to_string())
    }
}

pub fn version(bin: &Path) -> Option<String> {
    let mut cmd = Command::new(bin);
    hide_console(&mut cmd);
    let out = cmd.arg("version").output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().next().map(str::to_string)
}

/// `name: type` pairs from `rclone listremotes --long`.
pub fn list_remotes(bin: &Path, conf: &Path) -> Result<Vec<(String, String)>, String> {
    let out = run(bin, conf, &["listremotes", "--long"])?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let (name, kind) = l.split_once(':')?;
            Some((name.trim().to_string(), kind.trim().to_string()))
        })
        .collect())
}

#[cfg(windows)]
pub fn hide_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub fn hide_console(_: &mut Command) {}
