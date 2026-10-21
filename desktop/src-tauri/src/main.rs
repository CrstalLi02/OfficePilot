//! OfficePilot 桌面版：在同一进程里启动 WaLiOffice 服务（只监听 127.0.0.1），窗口加载这个地址。
//!
//! - 数据放在系统的应用数据目录（macOS：~/Library/Application Support/com.officepilot.desktop）；
//! - 单用户自动登录：令牌通过窗口初始化脚本注入，不经过网络；
//! - 关闭窗口缩到托盘，后台任务、定时任务、飞书接收继续运行；从托盘退出；
//! - 桌面模式默认开启本地命令插件、允许插件连接本机地址。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

/// 优先使用的端口：固定端口让页面来源不变，浏览器本地存储里的偏好可以保留
const PREFERRED_PORT: u16 = 17860;

struct DesktopState {
    data_root: PathBuf,
}

/// JWT 密钥：第一次启动时生成并保存在数据目录
fn jwt_secret(root: &Path) -> anyhow::Result<String> {
    let file = root.join(".jwt-secret");
    if let Ok(secret) = std::fs::read_to_string(&file) {
        if secret.trim().len() >= 32 {
            return Ok(secret.trim().to_string());
        }
    }
    let secret = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    std::fs::write(&file, &secret)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
    }
    Ok(secret)
}

/// 从图形界面启动时 PATH 很短（macOS 只有 /usr/bin 等），本地命令插件常用的 npx / uvx 找不到。
/// 取登录 shell 的 PATH，再补上常见的安装目录。
fn desktop_path() -> String {
    let mut parts: Vec<String> = Vec::new();
    #[cfg(unix)]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let out = std::process::Command::new(shell).args(["-lc", "printf %s \"$PATH\""]).output();
            let _ = tx.send(out);
        });
        if let Ok(Ok(out)) = rx.recv_timeout(Duration::from_secs(3)) {
            parts.extend(String::from_utf8_lossy(&out.stdout).split(':').map(str::to_string));
        }
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            for dir in [".local/bin", ".cargo/bin", ".volta/bin", ".bun/bin"] {
                parts.push(home.join(dir).to_string_lossy().into_owned());
            }
        }
        parts.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(str::to_string));
    }
    if let Ok(current) = std::env::var("PATH") {
        parts.extend(std::env::split_paths(&current).map(|p| p.to_string_lossy().into_owned()));
    }
    let mut seen = std::collections::HashSet::new();
    parts.retain(|p| !p.is_empty() && seen.insert(p.clone()));
    std::env::join_paths(parts).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

/// 服务端从环境变量读取配置：在第一次读取配置之前设置好
fn configure_env(root: &Path, port: u16) -> anyhow::Result<()> {
    let data = root.join("data");
    std::fs::create_dir_all(&data)?;
    let db = data.join("walioffice.db");
    let vars: Vec<(&str, String)> = vec![
        ("WALIOFFICE_DESKTOP", "1".into()),
        ("AIPPT_APP_NAME", "OfficePilot".into()),
        ("AIPPT_HOST", "127.0.0.1".into()),
        ("AIPPT_PORT", port.to_string()),
        ("AIPPT_DATA_DIR", data.to_string_lossy().into_owned()),
        ("AIPPT_PROJECTS_DIR", data.join("projects").to_string_lossy().into_owned()),
        ("AIPPT_SESSIONS_DIR", data.join("sessions").to_string_lossy().into_owned()),
        ("AIPPT_RENDER_OUTPUT_DIR", root.join("outputs").to_string_lossy().into_owned()),
        // 路径里可能有空格（Application Support）
        ("DATABASE_URL", format!("sqlite://{}?mode=rwc", db.to_string_lossy().replace(' ', "%20"))),
        ("AIPPT_JWT_SECRET", jwt_secret(root)?),
        // 桌面版长时间运行：令牌有效期放长，每次启动重新签发
        ("AIPPT_JWT_EXPIRY_HOURS", "87600".into()),
        ("WALIOFFICE_MCP_STDIO", "1".into()),
        ("WALIOFFICE_MCP_ALLOW_PRIVATE", "1".into()),
        ("WALIOFFICE_LOCAL_FOLDERS", "1".into()),
        ("PATH", desktop_path()),
    ];
    // 随安装包附带的 officepilot 命令行（sidecar，与主程序在同一目录）
    if let Some(cli) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(if cfg!(windows) { "officepilot.exe" } else { "officepilot" }))).filter(|p| p.is_file()) {
        std::env::set_var("OFFICEPILOT_CLI", cli);
    }
    for (key, value) in vars {
        std::env::set_var(key, value);
    }
    Ok(())
}

fn bind_listener() -> anyhow::Result<std::net::TcpListener> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", PREFERRED_PORT)).or_else(|_| std::net::TcpListener::bind(("127.0.0.1", 0)))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// 只允许打开数据目录里的文件（生成的产物、上传的文件）
fn allowed_path(state: &DesktopState, path: &str) -> Result<PathBuf, String> {
    let target = std::fs::canonicalize(path).map_err(|_| "文件不存在".to_string())?;
    let root = std::fs::canonicalize(&state.data_root).map_err(|e| e.to_string())?;
    if !target.starts_with(&root) {
        return Err("只能打开 OfficePilot 数据目录里的文件".into());
    }
    Ok(target)
}

/// 在访达 / 资源管理器中显示
#[tauri::command]
fn reveal_path(app: AppHandle, state: tauri::State<'_, DesktopState>, path: String) -> Result<(), String> {
    let target = allowed_path(&state, &path)?;
    app.opener().reveal_item_in_dir(target).map_err(|e| e.to_string())
}

/// 用默认应用打开
#[tauri::command]
fn open_path(app: AppHandle, state: tauri::State<'_, DesktopState>, path: String) -> Result<(), String> {
    let target = allowed_path(&state, &path)?;
    app.opener().open_path(target.to_string_lossy(), None::<&str>).map_err(|e| e.to_string())
}

/// 选择文件夹（本地文件夹工作区用）
#[tauri::command]
async fn pick_folder(app: AppHandle) -> Option<String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |picked| {
        let _ = tx.send(picked.map(|p| p.to_string()));
    });
    rx.await.ok().flatten()
}

fn setup(app: &mut tauri::App) -> anyhow::Result<()> {
    let root = app.path().app_data_dir()?;
    std::fs::create_dir_all(&root)?;
    // 安装版的日志写到数据目录（开发版仍输出到终端）
    if cfg!(debug_assertions) {
        walioffice::init_logging();
    } else {
        walioffice::init_logging_to_file(&root.join("logs").join("officepilot.log"))?;
    }
    let listener = bind_listener()?;
    let port = listener.local_addr()?.port();
    configure_env(&root, port)?;

    // 初始化数据库、调度与工具，签发本机用户的令牌
    let (router, token) = tauri::async_runtime::block_on(async {
        let router = walioffice::prepare().await?;
        let token = walioffice::desktop_token().await?;
        Ok::<_, anyhow::Error>((router, token))
    })?;
    tauri::async_runtime::spawn(async move {
        match tokio::net::TcpListener::from_std(listener) {
            Ok(listener) => {
                if let Err(e) = walioffice::serve(listener, router).await {
                    tracing::error!("服务停止：{e:?}");
                }
            }
            Err(e) => tracing::error!("监听失败：{e:?}"),
        }
    });
    tracing::info!("OfficePilot 桌面版：http://127.0.0.1:{port}，数据目录 {}", root.display());
    app.manage(DesktopState { data_root: root.clone() });

    let script = format!(
        "window.__OFFICEPILOT_DESKTOP__ = {};",
        json!({ "token": token, "version": env!("CARGO_PKG_VERSION"), "dataDir": root })
    );
    WebviewWindowBuilder::new(app, "main", WebviewUrl::External(format!("http://127.0.0.1:{port}/").parse()?))
        .title("OfficePilot")
        .inner_size(1360.0, 880.0)
        .min_inner_size(900.0, 600.0)
        .initialization_script(&script)
        .build()?;

    let show = MenuItem::with_id(app, "show", "显示 OfficePilot", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 OfficePilot", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let mut tray = TrayIconBuilder::with_id("main").tooltip("OfficePilot").menu(&menu).on_menu_event(|app, event| match event.id().as_ref() {
        "show" => show_main(app),
        "quit" => app.exit(0),
        _ => {}
    });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

fn main() {
    tauri::Builder::default()
        // 只允许一个实例：两个实例会同时写同一个数据库
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![reveal_path, open_path, pick_folder])
        .setup(|app| setup(app).map_err(|e| e.into()))
        .on_window_event(|window, event| {
            // 关闭窗口时缩到托盘，后台任务继续
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("启动 OfficePilot 失败")
        .run(|app, event| {
            #[cfg(target_os = "macos")]
            if let RunEvent::Reopen { .. } = event {
                show_main(app);
            }
            let _ = (app, event);
        });
}
