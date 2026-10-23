//! WaLiOffice 服务端：命令行（`walioffice`、`walioffice eval`）与桌面版（同进程启动）共用。

mod agent;
mod auth;
mod config;
mod data_engine;
mod db;
mod editor;
mod error;
mod eval;
mod file_extract;
mod file_store;
mod image_ocr;
mod llm;
mod models;
mod render;
mod routes;
mod schedules;
mod im;
mod mcp;
mod secrets;
mod skills;
mod state;
mod suite_ai;
mod suite_cli;
mod tasks;
mod workspace;


use std::net::SocketAddr;
use tracing::info;

/// 初始化日志（只调用一次）
pub fn init_logging() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,walioffice=debug".into()),
        )
        .try_init();
}

/// 日志写入文件（桌面版：没有终端可看）
pub fn init_logging_to_file(path: &std::path::Path) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // 超过 10MB 时从头开始，避免日志无限增长
    if std::fs::metadata(path).is_ok_and(|m| m.len() > 10 * 1024 * 1024) {
        let _ = std::fs::rename(path, path.with_extension("log.old"));
    }
    let file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,walioffice=info".into()),
        )
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(file))
        .try_init();
    Ok(())
}

/// 准备运行环境：配置、数据库、后台调度、飞书连接、插件、Agent 工具；返回路由
pub async fn prepare() -> anyhow::Result<axum::Router> {
    let cfg = config::config();
    cfg.ensure_dirs()?;

    let pool = state::init_db_pool().await;
    state::set_db_pool(pool);

    // 上次未完成的后台任务无法从中途恢复，标记为已中断（可重试）
    match tasks::mark_interrupted(&state::db_pool()).await {
        Ok(0) => {}
        Ok(n) => info!("⏹️ {n} 个未完成的后台任务已标记为中断"),
        Err(e) => tracing::warn!("标记中断任务失败: {e:?}"),
    }

    schedules::spawn_scheduler();
    tokio::spawn(im::manager::start_all());
    mcp::pool::spawn_reaper();
    agent::tools::register_all_tools().await;
    // 桌面版：用编辑器套件打开的文件，保存后同步回工作区
    if editor::enabled() {
        editor::spawn_sync();
    }

    Ok(routes::build_router())
}

/// 在给定的监听端口上提供服务（直到进程退出）
pub async fn serve(listener: tokio::net::TcpListener, app: axum::Router) -> anyhow::Result<()> {
    axum::serve(listener, app).await?;
    Ok(())
}

/// 桌面版：本机唯一用户，返回它的登录令牌（不存在时创建）
pub async fn desktop_token() -> anyhow::Result<String> {
    let user = db::user_repo::find_or_create_external(&state::db_pool(), "desktop").await?;
    Ok(auth::create_token(&user)?)
}

/// 命令行入口：`walioffice` 启动服务，`walioffice eval …` 离线评测
pub async fn cli_main() -> anyhow::Result<()> {
    init_logging();

    // 离线评测：walioffice eval [options]
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("eval") {
        return eval::run(&args[2..]).await;
    }

    let app = prepare().await?;
    let cfg = config::config();
    let addr: SocketAddr = format!("{}:{}", cfg.host, cfg.port).parse()?;
    info!("🚀 {} running at http://{}", cfg.app_name, addr);
    info!("📝 LLM: {} @ {}", cfg.llm_model, cfg.llm_base_url);
    info!("📂 Projects: {}", cfg.projects_dir);
    info!("🗄️ Database: {}", if cfg.is_mysql() { "MySQL" } else { "SQLite" });
    serve(tokio::net::TcpListener::bind(addr).await?, app).await
}
