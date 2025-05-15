use log::info;
use once_cell::sync::Lazy;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::Emitter;
use tauri::Runtime;

#[derive(Debug, Serialize, Clone)]
pub struct MpvProcess {
    id: u32,
    url: String,
    start_time: u64,
    last_known_time: Option<f64>,
    title: String,
    thumbnail: Option<String>,
}

static MPV_PROCESSES: Lazy<Mutex<HashMap<u32, MpvProcess>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[tauri::command]
pub async fn open_in_mpv<R: Runtime>(
    url: String,
    path: String,
    title: String,
    thumbnail: Option<String>,
    app_handle: tauri::AppHandle<R>,
) -> Result<u32, String> {
    use std::os::unix::net::UnixStream;
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::time::{SystemTime, UNIX_EPOCH};
    use std::fs;
    use std::path::Path;

    let ipc_socket = "/tmp/mpv-socket";

    // Try using an existing MPV instance
    if let Ok(mut stream) = UnixStream::connect(ipc_socket) {
        let command = format!(r#"{{"command": ["loadfile", "{}", "replace"]}}"#, url);
        writeln!(stream, "{}", command)
            .map_err(|e| format!("Failed to send command to MPV: {}", e))?;

        let process = MpvProcess {
            id: 0,
            url: url.clone(),
            start_time: 0,
            last_known_time: None,
            title,
            thumbnail,
        };

        let _ = app_handle.emit("mpv-process-reused", process);
        return Ok(0);
    }

    // No running MPV — try to find MPV binary
    let mpv_paths = if cfg!(target_os = "windows") {
        vec![
            r"C:\Program Files\mpv\mpv.exe",
            r"C:\Program Files (x86)\mpv\mpv.exe",
        ]
    } else if cfg!(target_os = "linux") {
        vec!["/usr/bin/mpv", "/usr/local/bin/mpv", "/snap/bin/mpv"]
    } else {
        vec![
            "/Applications/mpv.app/Contents/MacOS/mpv",
            "/opt/homebrew/bin/mpv",
            "/usr/local/bin/mpv",
        ]
    };

    let mpv_path = if !path.is_empty() && Path::new(&path).exists() {
        path
    } else {
        mpv_paths
            .iter()
            .find(|&p| Path::new(p).exists())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "mpv".to_string())
    };

    // Clean up any stale socket
    if Path::new(ipc_socket).exists() {
        let _ = fs::remove_file(ipc_socket);
    }

    // Start new MPV with fixed socket path
    let mut child = Command::new(&mpv_path)
        .arg("--idle=yes")
        .arg("--force-window=yes")
        .arg(format!("--input-ipc-server={}", ipc_socket))
        .arg(&url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Failed to launch MPV: {}", e))?;

    let process_id = child.id();

    // Spawn thread to track when MPV exits
    let app_handle_clone = app_handle.clone();
    thread::spawn(move || {
        let _ = child.wait();
        if let Some(p) = MPV_PROCESSES.lock().unwrap().remove(&process_id) {
            let _ = app_handle_clone.emit("mpv-process-removed", p);
        }
    });

    // Add to MPV_PROCESSES
    let process = MpvProcess {
        id: process_id,
        url: url.clone(),
        start_time: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        last_known_time: None,
        title,
        thumbnail,
    };

    MPV_PROCESSES
        .lock()
        .unwrap()
        .insert(process_id, process.clone());

    let _ = app_handle.emit("mpv-process-added", process.clone());

    Ok(process_id)
}

#[tauri::command]
pub async fn get_active_mpv_processes() -> Vec<MpvProcess> {
    MPV_PROCESSES.lock().unwrap().values().cloned().collect()
}

#[tauri::command]
pub async fn close_mpv_process<R: Runtime>(
    process_id: u32,
    app_handle: tauri::AppHandle<R>,
) -> Result<(), String> {
    #[cfg(windows)]
    return Err("Process termination is not supported on Windows".to_string());
    if let Some(process) = MPV_PROCESSES.lock().unwrap().remove(&process_id) {
        #[cfg(unix)]
        {
            unsafe {
                libc::kill(process_id as i32, libc::SIGTERM);
            }
        }

        // Updated event emission
        app_handle
            .emit("mpv-process-removed", process)
            .map_err(|e| e.to_string())?;

        Ok(())
    } else {
        Err("Process not found".to_string())
    }
}

#[tauri::command]
pub async fn open_in_vlc<R: Runtime>(
    url: String,
    path: String,
    user_agent: Option<String>,
    referer: Option<String>,
    origin: Option<String>,
    app_handle: tauri::AppHandle<R>,
) -> Result<(), String> {
    info!("Custom VLC path: {}", path);
    let vlc_paths = if cfg!(target_os = "windows") {
        vec![
            r"C:\Program Files\VideoLAN\VLC\vlc.exe",
            r"C:\Program Files (x86)\VideoLAN\VLC\vlc.exe",
        ]
    } else if cfg!(target_os = "linux") {
        vec!["/usr/bin/vlc", "/usr/local/bin/vlc", "/snap/bin/vlc"]
    } else {
        vec![
            "/Applications/VLC.app/Contents/MacOS/VLC",
            "/opt/homebrew/bin/vlc",
            "/usr/local/bin/vlc",
        ]
    };

    let vlc_path = if !path.is_empty() && Path::new(&path).exists() {
        path
    } else {
        vlc_paths
            .iter()
            .find(|&path| Path::new(path).exists())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "vlc".to_string())
    };

    info!("Using VLC player path: {}", vlc_path);

    let mut command = Command::new(&vlc_path);
    command.arg(&url);
    
    // Add headers if they are provided
    if let Some(ua) = user_agent {
        if !ua.is_empty() {
            command.arg(format!("--http-user-agent={}", ua));
        }
    }

    if let Some(ref_url) = referer {
        if !ref_url.is_empty() {
            command.arg(format!("--http-referrer={}", ref_url));
        }
    }

    // Log the complete command line
    let command_str = format!(
        "{} {}",
        vlc_path,
        command
            .get_args()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    );
    info!("Complete VLC command: {}", command_str);

    // Emit an event before attempting to spawn VLC
    app_handle.emit("player-launching", "VLC").map_err(|e| e.to_string())?;

    match command.spawn() {
        Ok(_) => {
            app_handle.emit("player-launched", "VLC").map_err(|e| e.to_string())?;
            Ok(())
        }
        Err(e) => {
            let error_msg = format!("Failed to launch VLC: {}", e);
            info!("{}", error_msg);
            app_handle.emit("player-error", error_msg.clone()).map_err(|e| e.to_string())?;
            Err(error_msg)
        }
    }
}

