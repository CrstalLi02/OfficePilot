//! officepilot：在本机创建、读取、修改、转换真实的 Word / Excel / PPT 文件。
//!
//! 每个命令默认输出一行摘要，加 --json 输出一个 JSON 对象（给脚本和编程 Agent 用）。
//! 编辑是原子的：一批操作有任何一个不合法，文件保持原样，错误信息说明如何修正。
//! 命令行里不调用任何大模型：思考交给 Agent，命令行只负责构建和检查。

use officepilot_cli::{convert, deck_cmd, guides, io, mcp, office, Out};

use clap::{Parser, Subcommand};
use serde_json::json;

#[derive(Parser)]
#[command(name = "officepilot", version, about = "创建、读取、修改、转换 Word / Excel / PPT 文件（原格式，只改动涉及的部分）")]
struct Cli {
    /// 输出一个 JSON 对象
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 概况：Word 的标题大纲、Excel 的工作表、PPT 的页数与标题
    Info { file: String },
    /// 结构化读取：Word 按块、Excel 按区域、PPT 按页
    Read {
        file: String,
        /// Word：起始块序号；PPT：起始页码
        #[arg(long)]
        from: Option<usize>,
        /// Word：结束块序号（含）；PPT：结束页码（含）
        #[arg(long)]
        to: Option<usize>,
        /// Excel：工作表名
        #[arg(long)]
        sheet: Option<String>,
        /// Excel：区域，如 A1:H40
        #[arg(long)]
        range: Option<String>,
        /// Excel：最多读取行数
        #[arg(long, default_value_t = 200)]
        max_rows: u32,
    },
    /// 执行一批编辑操作（原子）。操作格式见 `officepilot guide docs|sheet|slides`
    Apply {
        file: String,
        /// 操作：JSON 文件路径、- 表示标准输入，或直接写 JSON 数组
        #[arg(long)]
        ops: String,
        /// Word：写成修订（可在 Word 里逐条接受 / 拒绝）
        #[arg(long)]
        track: bool,
        /// 修订作者
        #[arg(long, default_value = "OfficePilot AI")]
        author: String,
        /// 另存到这个路径（默认原地保存）
        #[arg(long)]
        out: Option<String>,
        /// 只检查并报告改动，不写文件
        #[arg(long)]
        dry_run: bool,
        /// 读取时得到的 revision：文件之后被改过则拒绝
        #[arg(long)]
        revision: Option<String>,
    },
    /// 新建文件：docx 来自 Markdown，xlsx 来自 JSON / CSV，pptx 来自页面规格
    Create {
        #[arg(long = "type", value_parser = ["docx", "xlsx", "pptx"])]
        kind: String,
        /// 内容来源：.md（docx）、.json / .csv（xlsx）、整套规格 .json（pptx）
        #[arg(long)]
        from: Option<String>,
        /// pptx：页面规格文件或目录（每页一个 .json）
        #[arg(long)]
        spec: Option<String>,
        /// pptx：大纲文件（配合 --spec 目录）
        #[arg(long)]
        outline: Option<String>,
        /// pptx：风格表文件（配合 --spec 目录）
        #[arg(long)]
        style: Option<String>,
        /// docx：文档标题
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        out: String,
    },
    /// 本地转换：docx/pptx → md/txt/html，xlsx → csv/md/html，md → docx/html，csv → xlsx
    Convert {
        file: String,
        #[arg(long)]
        to: String,
        /// 输出路径（默认同目录同名换扩展名）
        #[arg(long)]
        out: Option<String>,
    },
    /// PPT 检查
    Slides {
        #[command(subcommand)]
        action: SlidesCmd,
    },
    /// 分阶段做 PPT：start 写风格与大纲 → page 每页检查后保留 → build 组装 → replace 重做某页
    Deck {
        #[command(subcommand)]
        action: DeckCmd,
    },
    /// 打印使用说明：overview / docs / sheet / slides / deck / formulas
    Guide { topic: Option<String> },
    /// 列出本机可用的能力
    Capabilities,
    /// 交给 OfficePilot 桌面版或系统默认程序打开
    Open { file: String },
    /// 作为 MCP 服务端运行（默认 stdio；--http 端口 提供 Streamable HTTP）
    Mcp {
        /// 用 HTTP 提供服务的端口
        #[arg(long)]
        http: Option<u16>,
        /// HTTP 监听地址；不是本机地址时必须设置 --token
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// HTTP 访问令牌（Authorization: Bearer …）；也可用环境变量 OFFICEPILOT_MCP_TOKEN
        #[arg(long)]
        token: Option<String>,
        /// 工作目录（stdio：相对路径的基准；HTTP：文件仓库位置）
        #[arg(long)]
        root: Option<String>,
        /// 对外地址（反向代理后面时设置），默认 http://host:端口
        #[arg(long)]
        public_url: Option<String>,
    },
}

#[derive(Subcommand)]
enum SlidesCmd {
    /// 检查页面规格（不渲染）：溢出、重叠、超出页面、字号；可对照风格表和大纲
    Check {
        /// 页面规格 .json，或大纲 .json
        spec: String,
        #[arg(long)]
        style: Option<String>,
        #[arg(long)]
        outline: Option<String>,
        /// 对照大纲的第几页（从 1 开始）
        #[arg(long)]
        page: Option<usize>,
    },
    /// 检查已有 .pptx 的版面：文字溢出、重叠、超出页面、字号过小
    Audit { file: String },
    /// 读取 PPT（同 read）
    Read { file: String },
}

#[derive(Subcommand)]
enum DeckCmd {
    /// 新建工作目录，写入风格表和大纲（检查后才写入）
    Start {
        dir: String,
        #[arg(long)]
        style: String,
        #[arg(long)]
        outline: String,
    },
    /// 提交一页（从 1 开始），检查通过才保留
    Page { dir: String, n: usize, spec: String },
    /// 组装成 .pptx 并检查版面
    Build {
        dir: String,
        #[arg(long)]
        out: String,
        /// 缺页时也组装（缺的页跳过）
        #[arg(long)]
        allow_missing: bool,
    },
    /// 重做一页并重新组装上次 build 的文件
    Replace { dir: String, n: usize, spec: String },
    /// 工作目录状态：哪些页已提交
    Status { dir: String },
}



fn run(cli: Cli) -> Out {
    match cli.command {
        Command::Info { file } => office::info(&file),
        Command::Read { file, from, to, sheet, range, max_rows } => office::read(&file, from, to, sheet.as_deref(), range.as_deref(), max_rows),
        Command::Apply { file, ops, track, author, out, dry_run, revision } => office::apply(&file, &ops, track, &author, out.as_deref(), dry_run, revision.as_deref()),
        Command::Create { kind, from, spec, outline, style, title, out } => office::create(&kind, from.as_deref(), spec.as_deref(), outline.as_deref(), style.as_deref(), title.as_deref(), &out),
        Command::Convert { file, to, out } => convert::convert(&file, &to, out.as_deref()),
        Command::Slides { action } => match action {
            SlidesCmd::Check { spec, style, outline, page } => deck_cmd::check(&spec, style.as_deref(), outline.as_deref(), page),
            SlidesCmd::Audit { file } => deck_cmd::audit(&file),
            SlidesCmd::Read { file } => office::read(&file, None, None, None, None, 200),
        },
        Command::Deck { action } => match action {
            DeckCmd::Start { dir, style, outline } => deck_cmd::start(&dir, &style, &outline),
            DeckCmd::Page { dir, n, spec } => deck_cmd::page(&dir, n, &spec, false),
            DeckCmd::Build { dir, out, allow_missing } => deck_cmd::build(&dir, &out, allow_missing),
            DeckCmd::Replace { dir, n, spec } => deck_cmd::page(&dir, n, &spec, true),
            DeckCmd::Status { dir } => deck_cmd::status(&dir),
        },
        Command::Guide { topic } => guides::guide(topic.as_deref()),
        Command::Capabilities => guides::capabilities(),
        Command::Open { file } => io::open(&file),
        Command::Mcp { .. } => unreachable!(),
    }
}

fn main() {
    let cli = Cli::parse();
    if let Command::Mcp { http, host, token, root, public_url } = &cli.command {
        let root = root.clone().map(std::path::PathBuf::from).or_else(|| std::env::current_dir().ok()).unwrap_or_default();
        let result = match http {
            None => mcp::stdio::serve(mcp::Workspace::local(root)).map_err(|e| e.to_string()),
            Some(port) => {
                let token = token.clone().or_else(|| std::env::var("OFFICEPILOT_MCP_TOKEN").ok()).filter(|t| !t.trim().is_empty());
                mcp::http::serve(mcp::http::Options { host: host.clone(), port: *port, token, root, public_url: public_url.clone() })
            }
        };
        if let Err(e) = result {
            eprintln!("错误：{e}");
            std::process::exit(1);
        }
        return;
    }
    let json_mode = cli.json;
    let code = match run(cli) {
        Ok(done) => {
            if json_mode {
                let mut data = done.data;
                if let Some(obj) = data.as_object_mut() {
                    obj.insert("ok".into(), json!(!done.failed));
                    obj.entry("summary").or_insert(json!(done.summary));
                }
                println!("{}", serde_json::to_string(&data).unwrap());
            } else {
                println!("{}", done.summary);
            }
            if done.failed { 1 } else { 0 }
        }
        Err(fail) => {
            if json_mode {
                println!("{}", json!({ "ok": false, "error": fail.message, "hint": fail.hint }));
            } else {
                eprintln!("错误：{}", fail.message);
                if let Some(h) = &fail.hint {
                    eprintln!("提示：{h}");
                }
            }
            1
        }
    };
    std::process::exit(code);
}
