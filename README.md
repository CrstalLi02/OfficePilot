# OfficePilot

> Web 端 AI Agent 智能办公平台 — 通过自然语言对话，一键生成 PPT、文档、表格、流程图、图表、图片和视频。

本项目基于开源项目 [fuzhengwei/WaLiOffice](https://github.com/fuzhengwei/WaLiOffice)（导入自 commit `50ad355`）二次开发，重构了 Agent 内核。原项目的功能介绍见下文。

## 🧭 本仓库新增

| 模块 | 内容 |
|---|---|
| 流式 + 并行工具调用 | 回复逐 token 推送；同一轮的多个工具调用按依赖关系分段执行，可并行的工具并发运行（并发上限可配置），每个工具在独立任务中运行，带超时和 panic 隔离，结果按原顺序写回上下文 |
| 可取消执行 | `POST /api/chat/cancel` 中断正在生成的 LLM 流和执行中的工具，已生成的产物照常保存；刷新页面不会中断任务 |
| Craft / Plan / Ask 三种模式 | 删除关键词规则式意图识别，改由模型根据工具描述选择工具；Plan 模式先提交计划（`propose_plan`），用户可勾选步骤、补充意见，确认后从中断处恢复执行，刷新页面后仍可确认 |
| 产物局部编辑 | `artifact_edit`：把产物展开成带编号的视图，模型返回语义化操作（按章节 / 页 / 单元格 / 列，图表用 JSON Merge Patch），服务端校验后应用，只重写被改动的部分；非法操作整体回滚 |
| 产物版本 | 每次生成、AI 编辑、手动修改、恢复都会记录一个版本；支持逐行对比任意版本、一键恢复 |
| 工作区文件工具 | Agent 可以列目录、全文搜索（带提取文本缓存）、读取 Word / PDF / PPT / Excel，新建文本文件、把产物导出到指定目录，批量建目录 / 移动 / 重命名 / 转换为 Markdown；每次写入和整理都记录逆操作，可一键撤销；不提供删除。输入框 `@` 可直接引用文件 |
| 数据分析 | 进程内 SQL 引擎（Apache DataFusion）：`data_profile` 查看数据结构与概况，`data_query` 对 csv / xlsx / json 执行只读 SQL，图表和表格直接由查询结果生成，不经过模型；SQL 文本校验 + DataFusion `SQLOptions` 禁止 DDL / DML / SET，限制内存、超时和结果行数；支持 GBK 编码 CSV，Excel 可按 sheet 导出为 csv |
| 技能系统 | 兼容开放的 Agent Skills（SKILL.md）规范与 OpenClaw 扩展字段：内置周报、会议纪要、PRD、竞品分析、数据分析报告、汇报 PPT、公文写作、简历优化 8 个技能；渐进式加载（提示词只列名称和简介 → `use_skill` 加载正文 → `read_skill_file` 读模板）；输入框 `/` 显式调用；支持导入 zip / SKILL.md（防路径穿越、限制大小）、启用停用、从对话生成技能；技能只作参考说明，`allowed-tools` 只能缩小工具范围 |
| 后台任务 | Agent 运行与网页连接解耦：每次运行落库为任务（排队 / 运行 / 完成 / 失败 / 停止 / 待确认 / 中断），每个用户同时最多运行 3 个，超出排队；事件写入回放缓冲，刷新页面或换设备打开会话会自动接上进度（SSE 支持断点续传）；关掉页面任务继续运行，完成后发通知；服务重启时未完成的任务标记为中断，可一键重试；顶栏任务面板查看与管理 |
| 定时任务 | 用 5 段 cron 设定周期任务（每天 / 工作日 / 每周 / 每月 / 每小时 / 自定义，最小间隔 10 分钟），按用户浏览器时区触发；可在对话里直接说“每周五下午五点帮我写周报”由 Agent 调用 `schedule_create` 创建，也可在任务面板里新建、编辑、停用、立即运行，并实时预览接下来的触发时间；到点以后台任务运行（执行或问答模式，每次新会话或固定会话），上一次还没结束时自动跳过，服务停机期间错过的只补跑一次；每次触发留有运行记录 |
| IM 远程控制 | 在飞书里给机器人发消息就能让 Agent 干活：默认用长连接接收事件（不需要公网地址，本机运行也能用），也支持事件回调 URL（验签 + 解密）；网页上生成 6 位绑定码，在飞书里发「绑定 123456」后才能使用，群聊需要 @ 机器人；每个飞书会话对应一个系统会话，支持 `/new`、`/mode 执行|规划|问答`、`/stop`、`/status`；运行中用卡片实时显示步骤，完成后更新为结论并把生成的 Word / Excel / PPT / 图片作为文件发回；规划模式下回复「确认」「取消」或修改意见即可处理计划；计划卡片带「确认执行」「取消」按钮（卡片回调），也可以直接回复；发图片会作为视觉输入，发文件会存进“我的文件”并交给 Agent 读取或分析（单独发来的先暂存，等下一条消息说明要做什么）；在飞书里创建的定时任务结果自动推回该会话，网页上也可以为定时任务选择推送到哪个飞书私聊或群；App Secret 等密钥加密存储，事件去重，每个账号每分钟最多 10 条 |
| MCP 插件 | 在「设置 → 插件（MCP）」接入任意 MCP 服务器，支持 Streamable HTTP、旧版 SSE 和本地命令（stdio，需服务端开启），也可以粘贴 Claude Desktop / Cursor 的 `mcpServers` JSON 一键导入；工具以 `mcp__标识__工具名` 自动加入 Agent 的工具箱，网页、飞书、定时任务里都能用；只读工具在规划 / 问答模式可用，可能修改数据的工具只在执行模式开放，模型调用未开放的工具会被拒绝；请求头与环境变量加密存储，本地命令不继承服务端环境变量，远程地址默认禁止内网 / 本机（防 SSRF），工具结果标注为外部数据以防提示注入；连接复用、工具列表缓存、会话失效自动重连，每个工具可单独开关 |
| 插件增强 | **调用前确认**：插件的写操作（或设为“每次确认”的插件）执行前运行原地暂停，网页弹出确认卡片（参数预览、倒计时），飞书发带按钮的卡片，可选“本次对话都允许”；10 分钟未处理自动拒绝，停止运行立即结束等待，定时任务没人在场直接拒绝；关掉页面时发通知。**插件目录**：内置按官方文档核实过的 DeepWiki、Context7、高德地图、GitHub、Fetch、Time，填写所需密钥即可添加（放在地址参数里的密钥也加密保存，不出现在界面和错误信息里）。**资源与提示词**：插件资源出现在 @ 菜单，引用后读取内容交给 Agent；插件提示词出现在 / 菜单，有参数时先填写，渲染后填进输入框。另外：系统提示要求执行类请求每次都必须实际调用工具，防止模型照着历史回复编造“已完成” |
| 桌面版 | Tauri 2 打包的 OfficePilot 桌面应用（`desktop/`）：同一进程内启动服务、只监听本机，自动以本机用户登录；首次启动引导配置模型服务（带连接测试）；关窗缩到托盘，定时任务和飞书接收继续运行；默认开启本地命令插件；文件可“用默认应用打开 / 在访达中显示”。详见 [desktop/README.md](desktop/README.md) |
| 路由评测 | `walioffice eval`：72 条用例（其中 5 条需要 `--mcp` 接入插件），复用线上的路由与请求组装逻辑，评测模型的工具选择，按模式输出报告 |
| 修复 | SQLite 下会话历史读取失败，导致 Agent 每轮都拿不到上下文；同一次运行中后续工具读不到刚生成的产物 |

架构说明与开发计划见各里程碑的提交记录。

WaLiOffice 将 AI Agent 能力与办公文档生成深度结合：用户在对话中描述需求，Agent 自动拆解任务、调用工具、生成产物，全程 SSE 流式响应，实时展示思考和生成过程。生产部署只需一个 Rust 二进制（前端静态资源内嵌），开箱即用。

你也可以选择安装 Deepseek Harness Plugin [https://github.com/fuzhengwei/walioffice-dsh-plugin](https://github.com/fuzhengwei/walioffice-dsh-plugin)

## ✨ 功能特性

- **AI 对话驱动**：基于 ReAct 循环的 Agent 引擎，支持多轮对话、上下文压缩、自动工具编排
- **PPT 生成**：大纲规划 → 内容填充 → 视觉设计，生成可预览的幻灯片
- **文档生成**：支持报告 / PRD / 计划等多种文档类型，可导出 `.docx`
- **表格生成**：结构化数据表格，可导出 `.xlsx`
- **图表生成**：基于数据自动生成可视化图表
- **流程图生成**：输出 draw.io 格式 XML，支持在线编辑
- **图片生成**：调用文生图模型，生成高质量图片
- **视频生成**：调用文生视频模型，远程不可用时本地 `ffmpeg` 兜底合成 MP4
- **联网搜索**：支持 SearXNG / DuckDuckGo 多搜索源，Agent 可主动检索实时信息
- **文件解析**：支持上传文件内容提取与 OCR 识别，作为对话上下文
- **多模型切换**：文本 / 图片 / 视频各自独立配置模型列表，前端设置页可实时切换
- **用户认证**：JWT 认证，登录页支持账号密码登录与注册
- **单一二进制部署**：Rust + rust-embed，前端嵌入后端，一个二进制即可启动全部服务
- **Docker 一键部署**：多阶段构建，内置国内镜像加速

## 🏗 技术栈

| 层 | 技术 | 说明 |
|---|---|---|
| 后端框架 | Rust + axum + tokio | 高性能异步 Web 框架 |
| 前端框架 | React 18 + TypeScript + Vite | SPA，构建后嵌入 Rust 二进制 |
| 样式 | TailwindCSS | 原子化 CSS |
| 状态管理 | Zustand | 轻量级状态管理 |
| 数据库 | SQLite（嵌入式） | 零配置，可选 MySQL |
| LLM 接口 | OpenAI 兼容 | 支持任何兼容端点，流式 SSE |
| 文档渲染 | docx-rs + rust_xlsxwriter | 纯 Rust 实现，无外部依赖 |
| 认证 | JWT + bcrypt | 无状态认证 |
| 静态资源 | rust-embed | 编译时嵌入前端产物 |
| 部署 | Docker / 单一二进制 | 多阶段构建；release 二进制约 69MB（内嵌前端与 DataFusion） |

## 📐 架构概览

```
┌─────────────────────────────────────────────────────────┐
│                    浏览器 / React UI                      │
│  Studio · Login · Files · Settings                       │
└────────────────────────┬────────────────────────────────┘
                         │ HTTP / SSE
┌────────────────────────▼────────────────────────────────┐
│                  Rust Axum Server                         │
│  ┌─────────┐ ┌──────────┐ ┌─────────┐ ┌──────────────┐ │
│  │  Auth   │ │  Routes  │ │  Agent  │ │   Render     │ │
│  │ (JWT)   │ │ (REST+   │ │ (ReAct) │ │ (DOCX/XLSX)  │ │
│  │         │ │  SSE)    │ │         │ │              │ │
│  └─────────┘ └──────────┘ └────┬───┘ └──────────────┘ │
│                               │                         │
│              ┌────────────────┼─────────────┐          │
│              ▼                ▼             ▼          │
│        ┌──────────┐  ┌──────────────┐ ┌──────────┐    │
│        │   LLM    │  │    Tools     │ │  SQLite  │    │
│        │  Client  │  │  Registry    │ │          │    │
│        └──────────┘  └──────┬───────┘ └──────────┘    │
│                             │                          │
│    ┌──────┬──────┬─────────┼────────┬──────┬──────┐  │
│    ▼      ▼      ▼         ▼        ▼      ▼      ▼  │
│   PPT    DOC   Sheet    DrawIO   Image  Video  Search │
└─────────────────────────────────────────────────────────┘
```

## 📁 项目结构

```
WaLiOffice/
├── server/                   # Rust 服务端（~14000 行）
│   └── src/
│       ├── main.rs           # 启动入口
│       ├── config.rs         # 环境变量配置
│       ├── state.rs          # 应用状态
│       ├── error.rs          # 统一错误处理
│       ├── file_extract.rs   # 文件内容提取
│       ├── image_ocr.rs      # OCR 图片识别
│       ├── agent/            # Agent 引擎
│       │   ├── mod.rs        # ReAct 循环 + 上下文管理
│       │   └── tools/        # 内置工具（13 个）
│       │       ├── ppt_plan.rs        # PPT 大纲规划
│       │       ├── ppt_generate.rs    # PPT 生成
│       │       ├── doc_generate.rs    # 文档生成
│       │       ├── sheet_generate.rs  # 表格生成
│       │       ├── chart_generate.rs  # 图表生成
│       │       ├── drawio_generate.rs # 流程图生成
│       │       ├── md_generate.rs     # Markdown 生成
│       │       ├── image_prompt.rs    # 图片提示词 + 生成
│       │       ├── video_generate.rs  # 视频生成（远程）
│       │       ├── local_video.rs     # 视频本地兜底
│       │       ├── agnes_media.rs     # Agnes 图像/视频服务
│       │       ├── web_search.rs      # 联网搜索
│       │       └── mod.rs             # 工具注册表
│       ├── llm/              # OpenAI 兼容 LLM Client（流式 + 非流式）
│       ├── auth/             # JWT 认证中间件
│       ├── db/               # SQLite 连接池 + migration + repository
│       ├── models/           # 数据模型
│       ├── render/           # DOCX / XLSX 纯 Rust 渲染
│       └── routes/           # API 路由（12 个模块）
│           ├── chat.rs       # SSE 流式 Agent 对话
│           ├── auth.rs       # 登录 / 注册 / 验证码
│           ├── session.rs    # 会话管理
│           ├── project.rs    # 项目管理
│           ├── file.rs       # 文件上传 / 下载
│           ├── settings.rs   # 模型切换 / 系统配置
│           ├── dashboard.rs  # 统计面板
│           ├── doc_export.rs # 文档导出
│           ├── notification.rs # 通知
│           ├── embed.rs      # 嵌入式静态资源
│           └── health.rs     # 健康检查
├── frontend/                 # React 前端（~9000 行）
│   └── src/
│       ├── pages/            # Studio / Login / Files
│       ├── components/       # 对话 / 产物展示 / PPT 预览 / 工具栏 / 设置
│       ├── api/              # 统一 API 封装（SSE 解析）
│       ├── stores/           # Zustand 状态管理
│       ├── lib/              # 工具函数
│       ├── types/            # TypeScript 类型定义
│       └── config/           # 前端配置
├── migrations/               # SQL 迁移脚本
├── Dockerfile                # 三阶段构建（前端 → Rust → 运行时）
├── docker-compose-walioffice.yml  # Docker Compose 部署配置
├── build.sh                  # 构建并推送镜像
├── .env.example              # 环境变量模板
└── ARCHITECTURE.md           # 架构设计文档
```

## 🚀 快速开始

### 前置要求

- **Rust** 1.88+（含 cargo）
- **Node.js** 20+（含 npm）
- **ffmpeg**（可选，仅视频本地兜底需要）

### 1. 配置环境变量

```bash
cp .env.example .env
```

编辑 `.env`，填写以下必要配置：

```bash
# ── 安全配置 ──
AIPPT_JWT_SECRET=your-random-secret-at-least-32-chars

# ── 文本 LLM（必填）──
LLM_TEXT_BASE_URL=http://127.0.0.1:8777/v1
LLM_TEXT_API_KEY=your-api-key
LLM_TEXT_MODELS=glm_for_coding

# ── 图片模型（必填）──
LLM_IMAGE_BASE_URL=https://apihub.agnes-ai.com
LLM_IMAGE_API_KEY=your-api-key
LLM_IMAGE_MODELS=agnes-image-2.1-flash

# ── 视频模型（必填）──
LLM_VIDEO_BASE_URL=https://apihub.agnes-ai.com
LLM_VIDEO_API_KEY=your-api-key
LLM_VIDEO_MODELS=agnes-video-v2.0
```

> 完整配置项见 `.env.example`，包括搜索源、数据目录、CORS 等。

### 2. 本地开发

```bash
# 方式一：同时启动前后端（推荐）
npm install
npm run dev
# 前端: http://localhost:5173（热更新）
# 后端: http://localhost:8000

# 方式二：分别启动
cd server && cargo run          # 终端 1：Rust 服务端
cd frontend && npm install && npm run dev  # 终端 2：前端 dev server
```

### 3. 生产构建（单一二进制）

```bash
# 构建前端 → 构建后端（前端产物自动嵌入二进制）
npm run build

# 运行
./server/target/release/walioffice
# 访问 http://localhost:8000
```

### 4. Docker 部署

```bash
# 使用预构建镜像
docker compose -f docker-compose-walioffice.yml up -d

# 或从源码构建
docker build -t walioffice .
docker run -p 8000:8000 -v ./data:/app/data walioffice
```

> 云服务器完整部署流程（含 Nginx + HTTPS）见 [DEPLOY.md](DEPLOY.md)。

## 🔧 Agent 工具

Agent 通过 ReAct 循环自动编排以下工具，用户无需手动选择：

| 工具 | 功能 | 输出格式 |
|------|------|----------|
| `ppt_plan` | 规划 PPT 大纲和结构 | JSON |
| `ppt_generate` | 生成完整 PPT（含视觉设计） | 幻灯片预览 |
| `doc_generate` | 生成文档（报告 / PRD / 计划等） | 可导出 `.docx` |
| `md_generate` | 生成 Markdown 文档 | Markdown |
| `sheet_generate` | 生成结构化表格 | 可导出 `.xlsx` |
| `chart_generate` | 基于数据生成可视化图表 | 图表 |
| `drawio_generate` | 生成流程图 / 架构图 | draw.io XML |
| `image_prompt` | 生成图片提示词并调用文生图 | 图片 |
| `video_generate` | 调用文生视频模型生成视频 | MP4 |
| `web_search` | 联网搜索实时信息 | 搜索结果摘要 |

## 📡 API 接口

| 路由 | 方法 | 说明 |
|------|------|------|
| `/api/auth/login` | POST | 账号密码登录 |
| `/api/auth/register` | POST | 注册 |
| `/api/auth/me` | GET | 获取当前用户信息 |
| `/api/chat/stream` | POST | SSE 流式 Agent 对话 |
| `/api/chat/sessions` | GET | 会话列表 |
| `/api/session/:id` | GET | 获取会话详情 |
| `/api/session/:id` | DELETE | 删除会话 |
| `/api/project/list` | GET | 项目列表 |
| `/api/project/:id` | GET | 获取项目详情 |
| `/api/file/upload` | POST | 文件上传 |
| `/api/file/download/:id` | GET | 文件下载 |
| `/api/doc/export` | POST | 导出 DOCX |
| `/api/excel/export` | POST | 导出 XLSX |
| `/api/settings/models` | GET | 获取可用模型列表 |
| `/api/settings/model` | PUT | 切换当前模型 |
| `/api/dashboard/stats` | GET | 统计数据 |
| `/api/notification/list` | GET | 通知列表 |
| `/api/health` | GET | 健康检查 |

## 🐳 Docker 构建说明

Dockerfile 采用三阶段构建：

1. **Stage 1 — 前端构建**：`node:20-slim`，npm ci + build
2. **Stage 2 — Rust 构建**：`rust:1.88-bookworm`，cargo build --release（使用 USTC crates 镜像加速）
3. **Stage 3 — 运行时**：`ubuntu:22.04`，仅包含二进制 + ffmpeg + 最小依赖

运行时镜像基于 `ubuntu:22.04` 并安装 ffmpeg，其中 walioffice 二进制（release，内嵌前端与 DataFusion 数据引擎）约 69MB，启动即用。release 构建（LTO + 单 codegen unit）首次约需 8–9 分钟。

## ⚙️ 环境变量参考

| 变量 | 必填 | 默认值 | 说明 |
|------|------|--------|------|
| `AIPPT_JWT_SECRET` | ✅ | — | JWT 签名密钥，生产环境必须修改 |
| `AIPPT_PORT` | ❌ | `8000` | 服务端口 |
| `AIPPT_HOST` | ❌ | `0.0.0.0` | 监听地址 |
| `LLM_TEXT_BASE_URL` | ✅ | — | 文本 LLM API 地址 |
| `LLM_TEXT_API_KEY` | ✅ | — | 文本 LLM API Key |
| `LLM_TEXT_MODELS` | ✅ | — | 可用文本模型列表（逗号分隔） |
| `LLM_IMAGE_BASE_URL` | ✅ | — | 图片模型 API 地址 |
| `LLM_IMAGE_API_KEY` | ✅ | — | 图片模型 API Key |
| `LLM_IMAGE_MODELS` | ✅ | — | 可用图片模型列表 |
| `LLM_VIDEO_BASE_URL` | ✅ | — | 视频模型 API 地址 |
| `LLM_VIDEO_API_KEY` | ✅ | — | 视频模型 API Key |
| `LLM_VIDEO_MODELS` | ✅ | — | 可用视频模型列表 |
| `AIPPT_WEB_SEARCH_PROVIDER` | ❌ | `auto` | 搜索源：`auto` / `searxng` / `duckduckgo` |
| `AIPPT_CORS_ORIGINS` | ❌ | — | CORS 允许来源（逗号分隔） |
| `AIPPT_DATA_DIR` | ❌ | `data` | 数据存储目录 |

> 完整配置见 [`.env.example`](.env.example)。

## 📌 登录说明

- 登录页（`/login`）使用**账号密码登录**，没有账号可以直接在页面上注册（用户名至少 3 个字符，密码至少 6 位）
- 旧的验证码登录接口 `/api/auth/verification-login` 仍保留在后端，需要时配置 `WALIOFFICE_X_API_AUTH_LOGIN_URL` 指向验证服务；登录页不再使用
- 管理后台已暂时移除，启动时不再创建默认管理员账号

## 📄 相关文档

- [架构设计](ARCHITECTURE.md) — 技术分层、模块职责、数据流
- [部署指南](DEPLOY.md) — 云服务器完整部署流程（Docker + Nginx + HTTPS）

## 📜 License

MIT
