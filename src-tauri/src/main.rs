mod hosts;
mod sftp;
mod ssh;

use hosts::{Host, HostStore};
use serde::Serialize;
use sftp::{ListResult, SftpManager};
use ssh::{SshCommand, SshManager};
use std::path::PathBuf;
use tauri::{AppHandle, State};

struct AppState {
    store: HostStore,
    ssh: SshManager,
    sftp: SftpManager,
    known_hosts: PathBuf,
}

#[derive(Serialize)]
struct ConnectResult {
    session_id: String,
    notice: Option<String>,
}

#[derive(Serialize)]
struct SftpOpenResult {
    session_id: String,
    notice: Option<String>,
    home: String,
}

#[tauri::command]
fn list_hosts(state: State<AppState>) -> Vec<Host> {
    state.store.list()
}

#[tauri::command]
fn save_host(state: State<AppState>, host: Host) -> Result<Host, String> {
    if host.name.trim().is_empty() || host.host.trim().is_empty() || host.username.trim().is_empty()
    {
        return Err("name, host and username are required".into());
    }
    if host.port == 0 {
        return Err("invalid port".into());
    }
    state.store.upsert(host)
}

#[tauri::command]
fn delete_host(state: State<AppState>, id: String) -> Result<(), String> {
    state.ssh.disconnect_host(&id);
    state.sftp.disconnect_host(&id);
    state.store.remove(&id)
}

#[tauri::command]
fn reset_host_key(state: State<AppState>, id: String) -> Result<usize, String> {
    let host = state.store.get(&id).ok_or("host not found")?;
    ssh::reset_host_key(&state.known_hosts, &host.host, host.port)
}

#[tauri::command]
async fn connect_host(
    app: AppHandle,
    state: State<'_, AppState>,
    host_id: String,
    secret: Option<String>,
    cols: u32,
    rows: u32,
) -> Result<ConnectResult, String> {
    let host = state.store.get(&host_id).ok_or("host not found")?;
    let mgr = state.ssh.clone();
    let known_hosts = state.known_hosts.clone();
    let (session_id, notice) = tauri::async_runtime::spawn_blocking(move || {
        ssh::connect(app, mgr, host, secret, cols, rows, &known_hosts)
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(ConnectResult { session_id, notice })
}

#[tauri::command]
fn ssh_write(state: State<AppState>, session_id: String, data: Vec<u8>) -> Result<(), String> {
    state.ssh.send(&session_id, SshCommand::Data(data))
}

#[tauri::command]
fn ssh_resize(
    state: State<AppState>,
    session_id: String,
    cols: u32,
    rows: u32,
) -> Result<(), String> {
    state.ssh.send(&session_id, SshCommand::Resize { cols, rows })
}

#[tauri::command]
fn ssh_close(state: State<AppState>, session_id: String) -> Result<(), String> {
    state.ssh.send(&session_id, SshCommand::Disconnect)
}

#[tauri::command]
async fn sftp_open(
    app: AppHandle,
    state: State<'_, AppState>,
    host_id: String,
    secret: Option<String>,
) -> Result<SftpOpenResult, String> {
    let host = state.store.get(&host_id).ok_or("host not found")?;
    let mgr = state.sftp.clone();
    let known_hosts = state.known_hosts.clone();
    let (session_id, notice, home) = tauri::async_runtime::spawn_blocking(move || {
        sftp::open(app, mgr, host, secret, &known_hosts)
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(SftpOpenResult {
        session_id,
        notice,
        home,
    })
}

#[tauri::command]
async fn sftp_list(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
) -> Result<ListResult, String> {
    let mgr = state.sftp.clone();
    tauri::async_runtime::spawn_blocking(move || mgr.list(&session_id, &path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn sftp_transfer(
    state: State<AppState>,
    session_id: String,
    id: String,
    upload: bool,
    src: String,
    dst: String,
) -> Result<(), String> {
    state.sftp.transfer(&session_id, id, upload, src, dst)
}

#[tauri::command]
fn sftp_close(state: State<AppState>, session_id: String) -> Result<(), String> {
    state.sftp.close(&session_id);
    Ok(())
}

#[tauri::command]
async fn sftp_mkdir(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
) -> Result<(), String> {
    let mgr = state.sftp.clone();
    tauri::async_runtime::spawn_blocking(move || mgr.mkdir(&session_id, &path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn sftp_delete(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
) -> Result<(), String> {
    let mgr = state.sftp.clone();
    tauri::async_runtime::spawn_blocking(move || mgr.delete(&session_id, &path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn local_list(path: String) -> Result<ListResult, String> {
    sftp::local_list(&path)
}

#[tauri::command]
fn local_delete(path: String) -> Result<(), String> {
    sftp::local_delete(&path)
}

#[tauri::command]
fn local_mkdir(path: String) -> Result<(), String> {
    sftp::local_mkdir(&path)
}

/// Called once by the frontend on startup. If the webview was reloaded, any
/// sessions still in the managers are unreachable orphans — kill them.
#[tauri::command]
fn reset_sessions(state: State<AppState>) {
    state.ssh.disconnect_all();
    state.sftp.disconnect_all();
}

fn main() {
    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ssh-workspace");
    let known_hosts = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ssh")
        .join("known_hosts");
    let state = AppState {
        store: HostStore::load(config_dir.join("hosts.json")),
        ssh: SshManager::new(),
        sftp: SftpManager::new(),
        known_hosts,
    };

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            list_hosts,
            save_host,
            delete_host,
            reset_host_key,
            connect_host,
            ssh_write,
            ssh_resize,
            ssh_close,
            sftp_open,
            sftp_list,
            sftp_transfer,
            sftp_close,
            sftp_mkdir,
            sftp_delete,
            local_list,
            local_mkdir,
            local_delete,
            reset_sessions,
        ])
        .run(tauri::generate_context!())
        .expect("error while running SSH Workspace");
}
