//! Settings and the app-owned rclone.conf, kept together in one data folder so
//! the whole setup can be copied or exported to another machine.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A folder with this name next to the executable switches the app into
/// portable mode: settings and rclone.conf live there instead of the user's
/// config directory.
const PORTABLE_DIR: &str = "clonemount-data";

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    /// Custom rclone binary. Empty means bundled, then PATH.
    pub rclone_path: String,
    pub mounts: Vec<MountConfig>,
    /// Set after the first launch, when autostart gets switched on.
    pub initialized: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MountConfig {
    pub id: String,
    pub name: String,
    /// rclone remote and optional path, e.g. `drive:` or `nas:media/photos`.
    pub remote: String,
    /// Drive letter (`X:`) on Windows or a folder elsewhere. Empty means pick
    /// one automatically, which keeps the config portable between systems.
    pub target: String,
    pub auto_mount: bool,
    pub read_only: bool,
    pub cache_mode: String,
    pub extra_args: String,
}

impl Default for MountConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            remote: String::new(),
            target: String::new(),
            auto_mount: true,
            read_only: false,
            cache_mode: "full".into(),
            extra_args: String::new(),
        }
    }
}

/// Everything needed to recreate the setup on another machine.
#[derive(Serialize, Deserialize)]
pub struct Bundle {
    pub app: String,
    pub version: u32,
    pub mounts: Vec<MountConfig>,
    pub rclone_conf: String,
}

pub struct Store {
    pub dir: PathBuf,
    pub portable: bool,
    settings: Mutex<Settings>,
}

impl Store {
    pub fn open(default_dir: PathBuf) -> Self {
        let portable_dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|d| d.join(PORTABLE_DIR)))
            .filter(|d| d.is_dir());
        let portable = portable_dir.is_some();
        let dir = portable_dir.unwrap_or(default_dir);
        let _ = fs::create_dir_all(dir.join("logs"));
        let settings = fs::read_to_string(dir.join("settings.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { dir, portable, settings: Mutex::new(settings) }
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn update(&self, f: impl FnOnce(&mut Settings)) -> Result<Settings, String> {
        let mut settings = self.settings.lock().unwrap();
        f(&mut settings);
        let json = serde_json::to_string_pretty(&*settings).map_err(|e| e.to_string())?;
        write_atomic(&self.dir.join("settings.json"), &json)?;
        Ok(settings.clone())
    }

    pub fn rclone_conf(&self) -> PathBuf {
        self.dir.join("rclone.conf")
    }

    pub fn read_rclone_conf(&self) -> String {
        fs::read_to_string(self.rclone_conf()).unwrap_or_default()
    }

    pub fn write_rclone_conf(&self, text: &str) -> Result<(), String> {
        write_atomic(&self.rclone_conf(), text)
    }

    pub fn log_file(&self, mount_id: &str) -> PathBuf {
        self.dir.join("logs").join(format!("{mount_id}.log"))
    }

    pub fn export(&self, path: &Path) -> Result<(), String> {
        let bundle = Bundle {
            app: "clonemount".into(),
            version: 1,
            mounts: self.settings().mounts,
            rclone_conf: self.read_rclone_conf(),
        };
        let json = serde_json::to_string_pretty(&bundle).map_err(|e| e.to_string())?;
        write_atomic(path, &json)
    }

    pub fn import(&self, path: &Path) -> Result<(), String> {
        let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
        let bundle: Bundle =
            serde_json::from_str(&text).map_err(|e| format!("Not a clonemount export: {e}"))?;
        if bundle.app != "clonemount" {
            return Err("Not a clonemount export".into());
        }
        self.write_rclone_conf(&bundle.rclone_conf)?;
        self.update(|s| s.mounts = bundle.mounts)?;
        Ok(())
    }
}

fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}
