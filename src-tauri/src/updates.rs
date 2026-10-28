//! Checks GitHub releases for a newer signed build, and installs it on request.
//!
//! Updates are never installed by themselves: restarting the app unmounts the
//! drives for a moment, which could interrupt open files.

use crate::AppState;
use serde::Serialize;
use std::sync::Mutex;
use std::time::Duration;
use tauri::menu::MenuItem;
use tauri::{AppHandle, Emitter, Manager, Wry};
use tauri_plugin_updater::{Update, UpdaterExt};

const FIRST_CHECK: Duration = Duration::from_secs(20);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Default)]
pub struct Updates {
    pending: Mutex<Option<Update>>,
    /// The tray menu entry, relabelled when an update is found.
    pub tray_item: Mutex<Option<MenuItem<Wry>>>,
}

#[derive(Serialize, Clone)]
pub struct UpdateInfo {
    pub current: String,
    pub available: Option<String>,
    pub notes: Option<String>,
}

pub fn start_background_checks(app: AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_CHECK);
        loop {
            let _ = tauri::async_runtime::block_on(check(&app));
            std::thread::sleep(CHECK_EVERY);
        }
    });
}

pub async fn check(app: &AppHandle) -> Result<UpdateInfo, String> {
    let update = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())?;
    let state = app.state::<AppState>();
    let info = UpdateInfo {
        current: app.package_info().version.to_string(),
        available: update.as_ref().map(|u| u.version.clone()),
        notes: update.as_ref().and_then(|u| u.body.clone()),
    };
    if let Some(item) = state.updates.tray_item.lock().unwrap().as_ref() {
        let label = match &info.available {
            Some(v) => format!("Install update {v}"),
            None => "Check for updates".into(),
        };
        let _ = item.set_text(label);
    }
    *state.updates.pending.lock().unwrap() = update;
    if info.available.is_some() {
        let _ = app.emit("update-available", info.clone());
    }
    Ok(info)
}

pub fn info(app: &AppHandle) -> UpdateInfo {
    let pending = app.state::<AppState>().updates.pending.lock().unwrap().clone();
    UpdateInfo {
        current: app.package_info().version.to_string(),
        available: pending.as_ref().map(|u| u.version.clone()),
        notes: pending.and_then(|u| u.body),
    }
}

/// Downloads the pending update, unmounts everything, installs and restarts.
pub async fn install(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let update = state.updates.pending.lock().unwrap().clone();
    let update = match update {
        Some(u) => u,
        None => {
            check(app).await?;
            state.updates.pending.lock().unwrap().clone().ok_or("clonemount is up to date")?
        }
    };
    let bytes = update.download(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
    // On Windows install() quits the app to run the installer, so unmount first.
    state.mounts.shutdown();
    update.install(bytes).map_err(|e| e.to_string())?;
    app.restart();
}

pub fn tray_clicked(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let has_update = app.state::<AppState>().updates.pending.lock().unwrap().is_some();
        let result = if has_update {
            install(&app).await
        } else {
            crate::show_window(&app);
            check(&app).await.map(|info| {
                let _ = app.emit("update-checked", info);
            })
        };
        if let Err(e) = result {
            let _ = app.emit("update-error", e);
        }
    });
}
