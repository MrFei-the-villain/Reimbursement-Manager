use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconEvent},
    Manager, RunEvent, WindowEvent,
};

struct AppState {
    port: u16,
}

struct BackendProcess(Mutex<Option<Child>>);

#[tauri::command]
fn get_backend_port(state: tauri::State<AppState>) -> u16 {
    state.port
}

fn pick_free_port() -> Option<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).ok()?;
    Some(listener.local_addr().ok()?.port())
}

/// Minimal HTTP/1.0 GET used for health checks (no HTTP client dependency).
fn http_get_ok(port: u16, path: &str) -> bool {
    use std::io::{Read, Write};
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let request = format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut buf = [0u8; 512];
    match stream.read(&mut buf) {
        Ok(n) if n > 0 => String::from_utf8_lossy(&buf[..n]).contains(" 200 "),
        _ => false,
    }
}

/// Mirror of the Python `app.config._resolve_base_dir` data-dir rules.
fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("REIMBURSE_DATA_DIR") {
        return PathBuf::from(dir);
    }
    if cfg!(debug_assertions) {
        // dev: the project root (parent of src-tauri, known at compile time)
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    } else {
        let appdata = std::env::var("APPDATA").unwrap_or_default();
        PathBuf::from(appdata).join("ReimbursementManager")
    }
}

fn spawn_backend(app: &tauri::App, port: u16) -> Result<Child, Box<dyn std::error::Error>> {
    let mut command = if cfg!(debug_assertions) {
        // dev: run straight from the project virtualenv
        let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let mut cmd = Command::new(project.join(".venv/Scripts/python.exe"));
        cmd.arg(project.join("backend_main.py"))
            .arg("--port")
            .arg(port.to_string())
            .current_dir(project);
        cmd
    } else {
        // release: PyInstaller onedir bundle shipped as a Tauri resource
        let resource_dir = app.path().resource_dir()?;
        let mut cmd = Command::new(resource_dir.join("backend").join("backend.exe"));
        cmd.arg("--port").arg(port.to_string());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        cmd
    };
    Ok(command.spawn()?)
}

fn kill_backend(app: &tauri::AppHandle) {
    let binding = app.state::<BackendProcess>();
    let mut guard = binding.0.lock().unwrap();
    if let Some(child) = guard.as_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
    *guard = None;
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![get_backend_port])
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .manage(BackendProcess(Mutex::new(None)))
        .setup(|app| {
            let port = pick_free_port().expect("could not find a free port");
            app.manage(AppState { port });

            let child = spawn_backend(app, port)?;
            *app.state::<BackendProcess>().0.lock().unwrap() = Some(child);

            // Wait for the backend health endpoint (OCR model load takes seconds)
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                if http_get_ok(port, "/api/health") {
                    break;
                }
                let exited = {
                    let binding = app.state::<BackendProcess>();
                    let mut guard = binding.0.lock().unwrap();
                    match guard.as_mut() {
                        Some(child) => matches!(child.try_wait(), Ok(Some(_))),
                        None => true,
                    }
                };
                if exited || Instant::now() > deadline {
                    kill_backend(app.handle());
                    return Err(format!("backend did not start on port {port}").into());
                }
                std::thread::sleep(Duration::from_millis(250));
            }

            // System tray
            let show = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
            let open_folder =
                MenuItem::with_id(app, "open_folder", "Open Data Folder", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &open_folder, &quit])?;

            tauri::tray::TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Reimbursement Manager")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main_window(app),
                    "open_folder" => {
                        let _ = Command::new("explorer").arg(data_dir()).spawn();
                    }
                    "quit" => {
                        kill_backend(app);
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                })
                .build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            // Close-to-tray: hide the window, the backend watcher keeps running
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                kill_backend(app);
            }
        });
}
