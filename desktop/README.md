# OfficePilot 桌面版

在本机运行的 AI 办公助手：服务端（WaLiOffice）和界面打包在同一个应用里，数据只保存在本机。

## 和网页版的区别

- 启动即用：自动以本机用户登录，没有登录页；首次启动引导配置模型服务（DeepSeek、通义千问或任意 OpenAI 兼容服务），密钥只保存在本机。
- 服务只监听 `127.0.0.1`（优先端口 17860），只允许运行一个实例。
- 关闭窗口会缩到托盘，后台任务、定时任务、飞书长连接继续运行；从托盘菜单退出。
- 默认开启本地命令（stdio）插件，并允许插件连接本机地址；会从登录 shell 读取 PATH，`npx` / `uvx` 等命令可以直接使用。
- 文件预览里可以“用默认应用打开”或“在访达中显示”（只允许数据目录里的文件）。
- 不读取启动目录里的 `.env`，所有配置由应用设置。
- 文件页和文件预览里有“用编辑器打开”：用随安装包附带的编辑器套件（OfficePilot Suite，位于 `OfficePilot.app/Contents/Resources/suite/`）编辑 Word / Excel / PPT / PDF / Markdown / HTML。文件先复制到数据目录的 `open/<文件 id>/<原文件名>`，编辑器里保存后自动同步回“我的文件”，每次同步都能在整理记录里撤销；编辑期间 AI 也改了同一个文件时两边都保留，编辑器的版本另存为“原名（编辑器版本）”。会话里生成的文档、表格、PPT 可以一步“保存到我的文件并用编辑器打开”。
- 附带 `officepilot` 命令行（sidecar，编译时由 build.rs 自动构建）：设置 → 集成里可以把 skill 装进 Claude Code / Codex / Cursor / Gemini CLI、复制 MCP 接入配置，或把命令行安装到 `~/.local/bin`。

数据目录：macOS 为 `~/Library/Application Support/com.officepilot.desktop`（数据库、上传和生成的文件、JWT 密钥、`logs/officepilot.log` 日志）。

## 开发与打包

需要 Rust、Node.js，以及 Tauri CLI（`cargo install tauri-cli --version "^2" --locked`）。

```bash
cd frontend && npm install && npm run build      # 界面会编译进应用
cd ../desktop/src-tauri
cargo build                                       # 开发版：./target/debug/officepilot-desktop
cargo tauri build                                 # 安装包：target/release/bundle/{macos,dmg}（不含编辑器套件）
```

带编辑器套件的完整安装包（macOS arm64，需要 Node.js 22）：

```bash
scripts/build-suite.sh                     # 构建 suite/ 子模块里的 OfficePilot Suite.app
SKIP_SUITE_BUILD=1 scripts/build-desktop.sh   # 打包 OfficePilot.app 并放入套件，再生成 dmg
```

开发时不需要打包：服务端会自动使用仓库里 `suite/apps/shell/release/mac-arm64/OfficePilot Suite.app`，也可以用环境变量 `OFFICEPILOT_SUITE_APP` 指定。用网页开发服务器调试这个功能时设置 `WALIOFFICE_LOCAL_EDITOR=1`。

未签名的安装包第一次打开时，macOS 会提示“无法验证开发者”，在访达里右键应用选择“打开”即可；第一次用编辑器打开文件时，编辑器套件可能同样需要确认一次。签名与公证需要 Apple 开发者账号；Windows / Linux 安装包需要在对应系统上构建。
