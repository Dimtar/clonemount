mod config;
mod mounts;
mod rclone;

use config::{MountConfig, Store};
use mounts::Manager;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager as _, RunEvent, State, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_opener::OpenerExt;

/// Passed by the autostart entry so a login launch stays in the tray.
const HIDDEN_ARG: &str = "--hidden";

struct AppState {
    store: Arc<Store>,
    mounts: Arc<Manager>,
}

type Res<T> = Result<T, String>;

#[derive(Serialize)]
struct Overview {
    mounts: Vec<MountConfig>,
    statuses: Vec<mounts::MountStatus>,
    rclone_path: String,
    rclone_found: Option<String>,
    rclone_version: Option<String>,
    data_dir: String,
    portable: bool,
    missing: Vec<mounts::Requirement>,
    system_conf: Option<String>,
}

#[derive(Serialize)]
struct Remote {
    name: String,
    kind: String,
}

fn rclone_bin(state: &AppState) -> Res<PathBuf> {
    rclone::find(&state.store.settings().rclone_path)
        .ok_or_else(|| "rclone not found. Set its location in Settings.".into())
}

#[tauri::command]
fn overview(state: State<AppState>) -> Overview {
    let settings = state.store.settings();
    let found = rclone::find(&settings.rclone_path);
    // Offer to import the user's existing rclone config until ours has remotes.
    let system_conf = if state.store.read_rclone_conf().trim().is_empty() {
        found.as_ref().and_then(|bin| system_rclone_conf(bin)).map(|p| p.to_string_lossy().into_owned())
    } else {
        None
    };
    Overview {
        statuses: state.mounts.statuses(),
        mounts: settings.mounts,
        rclone_path: settings.rclone_path,
        rclone_version: found.as_deref().and_then(rclone::version),
        rclone_found: found.map(|p| p.to_string_lossy().into_owned()),
        data_dir: state.store.dir.to_string_lossy().into_owned(),
        portable: state.store.portable,
        missing: mounts::missing_requirements(),
        system_conf,
    }
}

#[tauri::command]
fn statuses(state: State<AppState>) -> Vec<mounts::MountStatus> {
    state.mounts.statuses()
}

#[tauri::command]
fn save_mount(state: State<AppState>, mut mount: MountConfig) -> Res<MountConfig> {
    if mount.remote.trim().is_empty() {
        return Err("Choose a remote".into());
    }
    if mount.id.is_empty() {
        mount.id = uuid::Uuid::new_v4().to_string();
    }
    let saved = mount.clone();
    state.store.update(|s| match s.mounts.iter_mut().find(|m| m.id == mount.id) {
        Some(existing) => *existing = mount,
        None => s.mounts.push(mount),
    })?;
    Ok(saved)
}

#[tauri::command]
fn delete_mount(state: State<AppState>, id: String) -> Res<()> {
    state.store.update(|s| s.mounts.retain(|m| m.id != id))?;
    Ok(())
}

#[tauri::command]
fn set_mounted(state: State<AppState>, id: String, mounted: bool) {
    state.mounts.set_wanted(&id, mounted);
}

#[tauri::command]
fn open_target(app: AppHandle, path: String) -> Res<()> {
    let path = if path.len() == 2 && path.ends_with(':') { format!("{path}\\") } else { path };
    app.opener().open_path(path, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
fn open_url(app: AppHandle, url: String) -> Res<()> {
    if !url.starts_with("https://") {
        return Err("Only https links can be opened".into());
    }
    app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())
}

/// A native OK/Cancel prompt; the webview's own confirm() isn't reliable everywhere.
#[tauri::command]
async fn ask(app: AppHandle, message: String) -> bool {
    app.dialog()
        .message(message)
        .title("clonemount")
        .buttons(MessageDialogButtons::OkCancel)
        .blocking_show()
}

#[tauri::command]
fn open_data_dir(app: AppHandle, state: State<AppState>) -> Res<()> {
    app.opener()
        .open_path(state.store.dir.to_string_lossy(), None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn open_log(app: AppHandle, state: State<AppState>, id: String) -> Res<()> {
    app.opener()
        .open_path(state.store.log_file(&id).to_string_lossy(), None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_remotes(state: State<'_, AppState>) -> Res<Vec<Remote>> {
    let bin = rclone_bin(&state)?;
    let remotes = rclone::list_remotes(&bin, &state.store.rclone_conf())?;
    Ok(remotes.into_iter().map(|(name, kind)| Remote { name, kind }).collect())
}

/// Backend types and their options, straight from `rclone config providers`.
#[tauri::command]
async fn providers(state: State<'_, AppState>) -> Res<serde_json::Value> {
    let bin = rclone_bin(&state)?;
    let out = rclone::run(&bin, &state.store.rclone_conf(), &["config", "providers"])?;
    serde_json::from_str(&out).map_err(|e| e.to_string())
}

/// Creates a remote. For OAuth backends rclone opens the browser to sign in
/// and this waits until that finishes.
#[tauri::command]
async fn create_remote(
    state: State<'_, AppState>,
    name: String,
    kind: String,
    options: BTreeMap<String, String>,
) -> Res<()> {
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || "-_. ".contains(c)) {
        return Err("Remote names can use letters, numbers, spaces, - _ and .".into());
    }
    let bin = rclone_bin(&state)?;
    let conf = state.store.rclone_conf();
    let mut args = vec!["config".to_string(), "create".into(), name.into(), kind];
    args.extend(options.into_iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| format!("{k}={v}")));
    args.push("--obscure".into());
    tauri::async_runtime::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        rclone::run(&bin, &conf, &args).map(|_| ())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn delete_remote(state: State<'_, AppState>, name: String) -> Res<()> {
    let bin = rclone_bin(&state)?;
    rclone::run(&bin, &state.store.rclone_conf(), &["config", "delete", &name]).map(|_| ())
}

#[tauri::command]
fn get_rclone_conf(state: State<AppState>) -> String {
    state.store.read_rclone_conf()
}

#[tauri::command]
fn save_rclone_conf(state: State<AppState>, text: String) -> Res<()> {
    state.store.write_rclone_conf(&text)
}

/// The command line for editing this app's remotes with rclone's own wizard.
#[tauri::command]
fn config_command(state: State<AppState>) -> String {
    let bin = rclone::find(&state.store.settings().rclone_path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "rclone".into());
    format!("\"{bin}\" config --config \"{}\"", state.store.rclone_conf().display())
}

fn system_rclone_conf(bin: &std::path::Path) -> Option<PathBuf> {
    let mut cmd = std::process::Command::new(bin);
    rclone::hide_console(&mut cmd);
    let out = cmd.args(["config", "file"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let path = PathBuf::from(text.lines().last()?.trim());
    std::fs::metadata(&path).ok().filter(|m| m.len() > 0).map(|_| path)
}

#[tauri::command]
fn import_system_conf(state: State<AppState>) -> Res<()> {
    let bin = rclone_bin(&state)?;
    let path = system_rclone_conf(&bin).ok_or("No existing rclone config found")?;
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    state.store.write_rclone_conf(&text)
}

#[tauri::command]
fn set_rclone_path(state: State<AppState>, path: String) -> Res<()> {
    state.store.update(|s| s.rclone_path = path.trim().to_string())?;
    Ok(())
}

#[tauri::command]
async fn export_config(app: AppHandle, state: State<'_, AppState>) -> Res<bool> {
    let Some(path) = app
        .dialog()
        .file()
        .set_file_name("clonemount-config.json")
        .add_filter("clonemount config", &["json"])
        .blocking_save_file()
    else {
        return Ok(false);
    };
    state.store.export(&path.into_path().map_err(|e| e.to_string())?)?;
    Ok(true)
}

#[tauri::command]
async fn import_config(app: AppHandle, state: State<'_, AppState>) -> Res<bool> {
    let Some(path) = app
        .dialog()
        .file()
        .add_filter("clonemount config", &["json"])
        .blocking_pick_file()
    else {
        return Ok(false);
    };
    state.mounts.set_all_wanted(false);
    state.store.import(&path.into_path().map_err(|e| e.to_string())?)?;
    for m in state.store.settings().mounts.iter().filter(|m| m.auto_mount) {
        state.mounts.set_wanted(&m.id, true);
    }
    Ok(true)
}

#[tauri::command]
fn get_autostart(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Res<()> {
    let launcher = app.autolaunch();
    if enabled { launcher.enable() } else { launcher.disable() }.map_err(|e| e.to_string())
}

fn show_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open clonemount", true, None::<&str>)?;
    let mount_all = MenuItem::with_id(app, "mount_all", "Mount all", true, None::<&str>)?;
    let unmount_all = MenuItem::with_id(app, "unmount_all", "Unmount all", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let sep = || PredefinedMenuItem::separator(app);
    let menu = Menu::with_items(app, &[&open, &sep()?, &mount_all, &unmount_all, &sep()?, &quit])?;

    TrayIconBuilder::with_id("tray")
        .icon(app.default_window_icon().cloned().expect("app icon"))
        .tooltip("clonemount")
        .menu(&menu)
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .on_menu_event(|app, event| {
            let state = app.state::<AppState>();
            match event.id().as_ref() {
                "open" => show_window(app),
                "mount_all" => state.mounts.set_all_wanted(true),
                "unmount_all" => state.mounts.set_all_wanted(false),
                "quit" => app.exit(0),
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_window(app)))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec![HIDDEN_ARG])))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let store = Arc::new(Store::open(app.path().app_config_dir()?));

            // Turn on start-at-login the first time the app runs; that's the point of it.
            if !store.settings().initialized {
                let _ = app.autolaunch().enable();
                store.update(|s| s.initialized = true)?;
            }

            let mounts = Manager::new(store.clone());
            app.manage(AppState { store, mounts });
            build_tray(app.handle())?;

            if !std::env::args().any(|a| a == HIDDEN_ARG) {
                show_window(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window keeps mounts running in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            overview,
            statuses,
            save_mount,
            delete_mount,
            set_mounted,
            open_target,
            open_data_dir,
            open_url,
            ask,
            open_log,
            list_remotes,
            providers,
            create_remote,
            delete_remote,
            get_rclone_conf,
            save_rclone_conf,
            config_command,
            import_system_conf,
            set_rclone_path,
            export_config,
            import_config,
            get_autostart,
            set_autostart,
        ])
        .build(tauri::generate_context!())
        .expect("error while building clonemount");

    app.run(|app, event| {
        if let RunEvent::Exit = event {
            if let Some(state) = app.try_state::<AppState>() {
                state.mounts.shutdown();
            }
        }
    });
}
