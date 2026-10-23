//! 桌面版的运行环境：数据目录、JWT 密钥、PATH 等都由程序设置，不读取 `.env`。
//!
//! 两种启动方式共用这里：
//! - 旧版 Tauri 桌面版在同一进程里调用 [`configure_env`]；
//! - OfficePilot 桌面版（编辑器套件外壳）把服务作为后台进程启动：`walioffice desktop --data-root <目录>
//!   --ready-file <文件>`。服务就绪后把端口和本机用户的令牌写进就绪文件（只有当前用户可读），
//!   外壳读取后连接；外壳退出（标准输入关闭）时服务随之退出。

use std::path::{Path, PathBuf};
use std::time::Duration;

/// 优先使用的端口：固定端口让页面来源不变，浏览器本地存储里的偏好可以保留
pub const PREFERRED_PORT: u16 = 17860;

/// JWT 密钥：第一次启动时生成并保存在数据目录
pub fn jwt_secret(root: &Path) -> anyhow::Result<String> {
    let file = root.join(".jwt-secret");
    if let Ok(secret) = std::fs::read_to_string(&file) {
        if secret.trim().len() >= 32 {
            return Ok(secret.trim().to_string());
        }
    }
    let secret = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    write_private(&file, secret.as_bytes())?;
    Ok(secret)
}

/// 写入只有当前用户可读的文件（先写临时文件再改名）
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)
}

/// 从图形界面启动时 PATH 很短（macOS 只有 /usr/bin 等），本地命令插件常用的 npx / uvx 找不到。
/// 取登录 shell 的 PATH，再补上常见的安装目录。
pub fn desktop_path() -> String {
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
pub fn configure_env(root: &Path, port: u16) -> anyhow::Result<()> {
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

struct Args {
    data_root: PathBuf,
    ready_file: PathBuf,
    exit_on_stdin_close: bool,
}

fn parse_args(args: &[String]) -> anyhow::Result<Args> {
    let mut data_root = None;
    let mut ready_file = None;
    let mut exit_on_stdin_close = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--data-root" => data_root = it.next().map(PathBuf::from),
            "--ready-file" => ready_file = it.next().map(PathBuf::from),
            "--exit-on-stdin-close" => exit_on_stdin_close = true,
            other => anyhow::bail!("不认识的参数：{other}"),
        }
    }
    Ok(Args {
        data_root: data_root.ok_or_else(|| anyhow::anyhow!("缺少 --data-root"))?,
        ready_file: ready_file.ok_or_else(|| anyhow::anyhow!("缺少 --ready-file"))?,
        exit_on_stdin_close,
    })
}

/// 就绪文件的内容
pub fn ready_json(port: u16, token: &str) -> serde_json::Value {
    serde_json::json!({
        "port": port,
        "url": format!("http://127.0.0.1:{port}/"),
        "token": token,
        "pid": std::process::id(),
        "version": env!("CARGO_PKG_VERSION"),
    })
}

/// `walioffice desktop …`：作为桌面版的后台服务运行
pub async fn run(args: &[String]) -> anyhow::Result<()> {
    let args = parse_args(args)?;
    std::fs::create_dir_all(&args.data_root)?;
    // 上次留下的就绪文件作废，外壳只认这次写入的
    let _ = std::fs::remove_file(&args.ready_file);
    crate::init_logging_to_file(&args.data_root.join("logs").join("officepilot.log"))?;
    let listener = bind_listener()?;
    let port = listener.local_addr()?.port();
    configure_env(&args.data_root, port)?;
    let router = crate::prepare().await?;
    let token = crate::desktop_token().await?;
    if args.exit_on_stdin_close {
        // 外壳退出（包括崩溃）时标准输入关闭，服务随之退出，不留下占着端口的进程
        std::thread::spawn(|| {
            use std::io::Read;
            let mut buf = [0u8; 64];
            let mut stdin = std::io::stdin();
            while matches!(stdin.read(&mut buf), Ok(n) if n > 0) {}
            tracing::info!("桌面外壳已退出，服务停止");
            std::process::exit(0);
        });
    }
    if let Some(dir) = args.ready_file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_private(&args.ready_file, serde_json::to_vec(&ready_json(port, &token))?.as_slice())?;
    tracing::info!("OfficePilot 桌面服务：http://127.0.0.1:{port}，数据目录 {}", args.data_root.display());
    crate::serve(tokio::net::TcpListener::from_std(listener)?, router).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_and_ready_file() {
        let a = parse_args(&["--data-root".into(), "/d".into(), "--ready-file".into(), "/r.json".into(), "--exit-on-stdin-close".into()]).unwrap();
        assert_eq!((a.data_root, a.ready_file, a.exit_on_stdin_close), (PathBuf::from("/d"), PathBuf::from("/r.json"), true));
        assert!(parse_args(&["--data-root".into(), "/d".into()]).is_err());
        assert!(parse_args(&["--nope".into()]).is_err());
        let v = ready_json(17860, "tok");
        assert_eq!((v["port"].as_u64(), v["url"].as_str(), v["token"].as_str()), (Some(17860), Some("http://127.0.0.1:17860/"), Some("tok")));
    }

    #[test]
    fn private_file_and_stable_secret() {
        let dir = std::env::temp_dir().join(format!("officepilot-desktop-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let s1 = jwt_secret(&dir).unwrap();
        assert_eq!(s1, jwt_secret(&dir).unwrap(), "第二次启动沿用同一个密钥");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(".jwt-secret")).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
