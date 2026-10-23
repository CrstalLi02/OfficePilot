# OfficePilot

> A web-based AI agent for office work: describe what you need in plain language and get slide decks, documents, spreadsheets, flowcharts, charts, images and videos in one step.

This project is built on the open-source [fuzhengwei/WaLiOffice](https://github.com/fuzhengwei/WaLiOffice) (imported at commit `50ad355`), with a rebuilt agent core. The original project's feature overview follows further below.

## 🧭 What this repository adds

| Module | What it does |
|---|---|
| Streaming + parallel tool calls | Replies stream token by token. Tool calls in the same turn are split into segments by dependency; tools that can run in parallel run concurrently (configurable limit). Each tool runs in its own task with a timeout and panic isolation, and results are written back to the context in the original order |
| Cancellable runs | `POST /api/chat/cancel` stops the LLM stream and any running tools; artifacts already produced are still saved. Refreshing the page does not interrupt a run |
| Craft / Plan / Ask modes | Keyword-based intent rules were removed; the model chooses tools from their descriptions. In Plan mode the agent first submits a plan (`propose_plan`); the user can tick steps and add comments, and the run resumes where it paused once confirmed — even after a page refresh |
| Partial artifact edits | `artifact_edit` expands an artifact into a numbered view; the model returns semantic operations (by section / slide / cell / column, JSON Merge Patch for charts). The server validates and applies them, rewriting only the changed parts; any invalid operation rolls back the whole batch |
| Artifact versions | Every generation, AI edit, manual edit and restore records a version; any two versions can be diffed line by line and restored in one click |
| Workspace file tools | The agent can list folders, full-text search (with an extracted-text cache), and read Word / PDF / PPT / Excel files; create text files, export artifacts to a chosen folder, and batch-create folders / move / rename / convert to Markdown. Every write and reorganisation records its inverse operation and can be undone in one click; there is no delete. Type `@` in the input box to reference a file |
| Data analysis | An in-process SQL engine (Apache DataFusion): `data_profile` shows a dataset's structure and summary, and `data_query` runs read-only SQL over csv / xlsx / json. Charts and tables are built straight from query results, not by the model. SQL text checks plus DataFusion `SQLOptions` forbid DDL / DML / SET, with limits on memory, time and result rows. GBK-encoded CSV is supported, and Excel sheets can be exported to csv |
| Skills | Compatible with the open Agent Skills (SKILL.md) spec and OpenClaw extension fields. Eight built-in skills: weekly report, meeting minutes, PRD, competitive analysis, data analysis report, presentation deck, official document writing, and resume polishing. Progressive loading (the prompt lists only names and summaries → `use_skill` loads the body → `read_skill_file` reads templates). Type `/` in the input box to invoke one explicitly. Import from zip / SKILL.md (path-traversal protected, size limited), enable or disable, or create a skill from a conversation. Skills are reference instructions only; `allowed-tools` can only narrow the tool set |
| Background tasks | Agent runs are decoupled from the web connection. Each run is stored as a task (queued / running / done / failed / stopped / awaiting confirmation / interrupted); each user can run up to 3 at once and the rest queue. Events go into a replay buffer, so refreshing the page or opening the session on another device picks the progress back up (SSE supports resuming). Closing the page keeps the task running and sends a notification when it finishes. Tasks left unfinished when the server restarts are marked interrupted and can be retried in one click. Manage them from the task panel in the top bar |
| Scheduled tasks | Set up recurring tasks with 5-field cron (daily / weekdays / weekly / monthly / hourly / custom, minimum interval 10 minutes), triggered in the user's browser time zone. Ask in chat — e.g. "write my weekly report every Friday at 5 pm" — and the agent creates it with `schedule_create`, or create, edit, pause and run tasks from the task panel with a live preview of upcoming runs. Each trigger runs as a background task (Craft or Ask mode, in a new or a fixed session). A run is skipped if the previous one is still going, and runs missed while the server was down are made up only once. Every trigger keeps a run record |
| IM remote control | Message the bot in Feishu (Lark) to put the agent to work. Events arrive over a long connection by default (no public address needed, works on a local machine), and an event callback URL (signature check + decryption) is also supported. Generate a 6-digit binding code on the web, then send `绑定 123456` ("bind 123456") in Feishu to start; group chats require @-mentioning the bot. Each Feishu conversation maps to one session and supports `/new`, `/mode 执行\|规划\|问答` (Craft / Plan / Ask), `/stop` and `/status`. While running, a card shows the steps live; when done it updates with the conclusion and sends back the generated Word / Excel / PPT / image files. In Plan mode, reply `确认` (confirm), `取消` (cancel) or your changes to handle the plan; plan cards also carry Confirm / Cancel buttons (card callbacks). Images are used as visual input; files are saved to My Files and handed to the agent to read or analyse (a file sent on its own is held until the next message says what to do with it). Results of scheduled tasks created in Feishu are pushed back to that conversation, and on the web you can choose which Feishu private chat or group a scheduled task reports to. App Secret and other keys are stored encrypted, events are de-duplicated, and each account is limited to 10 messages per minute |
| MCP plugins | Connect any MCP server under Settings → Plugins (MCP): Streamable HTTP, legacy SSE, or a local command (stdio, must be enabled on the server); you can also paste a Claude Desktop / Cursor `mcpServers` JSON to import in one step. Tools join the agent's toolbox as `mcp__<id>__<tool>` and work on the web, in Feishu and in scheduled tasks. Read-only tools are available in Plan / Ask mode, while tools that may change data are only offered in Craft mode; calls to tools that are not offered are rejected. Headers and environment variables are stored encrypted, local commands do not inherit the server's environment, remote addresses on private networks / localhost are blocked by default (SSRF protection), and tool results are marked as external data to guard against prompt injection. Connections are reused, tool lists are cached, expired sessions reconnect automatically, and each tool can be switched on or off |
| Plugin enhancements | **Confirm before calling**: a plugin's write operations (or every call, for plugins set to "always confirm") pause the run in place. The web shows a confirmation card (argument preview, countdown) and Feishu gets a card with buttons, with an option to "allow for this conversation". Requests left unanswered for 10 minutes are rejected, stopping the run ends the wait immediately, and scheduled tasks — with nobody there to answer — are rejected outright; a notification is sent if the page is closed. **Plugin catalog**: built-in entries verified against the official docs for DeepWiki, Context7, Amap, GitHub, Fetch and Time — fill in the required keys to add one (keys passed as URL parameters are also stored encrypted and never shown in the UI or error messages). **Resources and prompts**: plugin resources appear in the `@` menu and their content is passed to the agent when referenced; plugin prompts appear in the `/` menu, ask for any arguments first, and are rendered into the input box. In addition, the system prompt requires every action request to actually call a tool, so the model cannot claim something is "done" by copying earlier replies |
| Desktop app | The OfficePilot desktop app packaged with Tauri 2 (`desktop/`): it starts the server in the same process, listens only on localhost, and signs in as the local user automatically. First launch walks through configuring a model service (with a connection test). Closing the window minimises to the tray while scheduled tasks and Feishu messages keep running. Local-command plugins are on by default, and files can be opened with the default app or shown in Finder. See [desktop/README.md](desktop/README.md) |
| Native Office editing | `engine/` (`officepilot-engine`): reads and edits real `.docx` / `.xlsx` / `.pptx` files in place, rewriting only the XML fragments that change and copying every other zip entry byte for byte. Word: block-level read (paragraphs, headings, lists, tables), find/replace, rewrite, Markdown insert, delete, move, restyle, table cells and comments, optionally as tracked changes. Excel: values and live formulas (results computed and cached, dependent formulas recalculated), formats, column widths, frozen panes, sheets and native charts. PowerPoint: text, tables, speaker notes, duplicate / delete / reorder slides. A batch of operations either fully applies or leaves the file unchanged, and errors quote the original text. The agent uses it through `docs_*` / `sheet_*` / `slides_*` tools; edits save in place with a snapshot and one-click undo |
| Command line | `cli/` (`officepilot`, ~2.5 MB): `info`, `read`, `apply` (`--dry-run`, `--track`, `--revision`, `--out`), `create` (docx from Markdown, xlsx from JSON / CSV, pptx from page specs), `convert` (docx / pptx → md / txt / html, xlsx → csv / md / html, md → docx / html, csv → xlsx), `slides check` / `slides audit` (overflow, overlap, off-slide and font-size checks without rendering), and a staged deck workflow (`deck start` → `deck page` → `deck build` → `deck replace`) with 12 layouts, native tables, editable charts, images and speaker notes. Every command supports `--json`; no model or network calls. Ships with an agent skill (`cli/skill/officepilot/SKILL.md`) and built-in guides (`officepilot guide`) |
| MCP server and integrations | `officepilot mcp` exposes every command as an MCP tool (19 tools plus `officepilot://guide/*` resources) over stdio, or over Streamable HTTP with legacy SSE (`--http`), with Bearer tokens (required off localhost), Origin checks, per-session sandboxes, file upload / download and an allowed-roots policy. Settings → Integrations installs the skill into Claude Code, Codex, Cursor and Gemini CLI on click, copies MCP setup snippets, and (desktop) links the bundled command line into `~/.local/bin` |
| Editor suite | `suite/`: the OfficePilot Suite desktop editors (Docs, Sheets, Slides, PDF, Markdown, HTML), a modified fork of [GenOffice](https://github.com/genspark-ai/genoffice) (Apache-2.0) kept as a git submodule. The fork removes the Genspark account and proxy, usage analytics and the upstream update feed, and rebrands every user-visible surface; see the section at the end of this README |
| Routing evaluation | `walioffice eval`: 72 cases (5 of which need a plugin attached with `--mcp`). It reuses the production routing and request-building logic to evaluate the model's tool choices and writes a report per mode |
| Fixes | Session history failed to load on SQLite, so the agent lost its context every turn; later tools in the same run could not see artifacts generated earlier in that run |

The architecture and development plans are recorded in each milestone's commits.

WaLiOffice combines AI agent capabilities with office document generation: you describe what you need in a conversation, and the agent breaks the task down, calls tools and produces the artifacts, streaming everything over SSE so you can watch it think and build in real time. A production deployment is a single Rust binary (with the front-end assets embedded) that works out of the box.

You can also install the DeepSeek Harness Plugin: [https://github.com/fuzhengwei/walioffice-dsh-plugin](https://github.com/fuzhengwei/walioffice-dsh-plugin)

## ✨ Features

- **Conversation-driven AI**: an agent engine built on a ReAct loop, with multi-turn conversations, context compression and automatic tool orchestration
- **Slide decks**: outline planning → content → visual design, producing slides you can preview
- **Documents**: reports, PRDs, plans and other document types, exportable to `.docx`
- **Spreadsheets**: structured tables, exportable to `.xlsx`
- **Charts**: visual charts generated from data
- **Flowcharts**: draw.io XML output that can be edited online
- **Images**: high-quality images from text-to-image models
- **Videos**: text-to-video models, with a local `ffmpeg` fallback that assembles an MP4 when the remote service is unavailable
- **Web search**: multiple search sources (SearXNG / DuckDuckGo); the agent can look up current information on its own
- **File parsing**: content extraction and OCR for uploaded files, used as conversation context
- **Multiple models**: separate model lists for text / image / video, switchable live from the settings page
- **Authentication**: JWT; the login page supports username + password sign-in and registration
- **Single-binary deployment**: Rust + rust-embed embed the front end into the back end, so one binary starts everything
- **One-command Docker deployment**: multi-stage build with mirror acceleration for mainland China

## 🏗 Tech stack

| Layer | Technology | Notes |
|---|---|---|
| Back-end framework | Rust + axum + tokio | High-performance async web framework |
| Front-end framework | React 18 + TypeScript + Vite | SPA, embedded into the Rust binary after building |
| Styling | TailwindCSS | Utility-first CSS |
| State management | Zustand | Lightweight state management |
| Database | SQLite (embedded) | Zero configuration; MySQL optional |
| LLM interface | OpenAI-compatible | Works with any compatible endpoint, streaming SSE |
| Document rendering | docx-rs + rust_xlsxwriter | Pure Rust, no external dependencies |
| Authentication | JWT + bcrypt | Stateless authentication |
| Static assets | rust-embed | Front-end build embedded at compile time |
| Deployment | Docker / single binary | Multi-stage build; the release binary is about 69 MB (front end and DataFusion embedded) |

## 📐 Architecture overview

```
┌─────────────────────────────────────────────────────────┐
│                  Browser / React UI                       │
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

## 📁 Project structure

```
WaLiOffice/
├── server/                   # Rust server (~14,000 lines)
│   └── src/
│       ├── main.rs           # Entry point
│       ├── config.rs         # Environment-variable configuration
│       ├── state.rs          # Application state
│       ├── error.rs          # Unified error handling
│       ├── file_extract.rs   # File content extraction
│       ├── image_ocr.rs      # OCR for images
│       ├── agent/            # Agent engine
│       │   ├── mod.rs        # ReAct loop + context management
│       │   └── tools/        # Built-in tools (13)
│       │       ├── ppt_plan.rs        # Slide outline planning
│       │       ├── ppt_generate.rs    # Slide deck generation
│       │       ├── doc_generate.rs    # Document generation
│       │       ├── sheet_generate.rs  # Spreadsheet generation
│       │       ├── chart_generate.rs  # Chart generation
│       │       ├── drawio_generate.rs # Flowchart generation
│       │       ├── md_generate.rs     # Markdown generation
│       │       ├── image_prompt.rs    # Image prompt + generation
│       │       ├── video_generate.rs  # Video generation (remote)
│       │       ├── local_video.rs     # Local video fallback
│       │       ├── agnes_media.rs     # Agnes image / video service
│       │       ├── web_search.rs      # Web search
│       │       └── mod.rs             # Tool registry
│       ├── llm/              # OpenAI-compatible LLM client (streaming + non-streaming)
│       ├── auth/             # JWT authentication middleware
│       ├── db/               # SQLite pool + migrations + repositories
│       ├── models/           # Data models
│       ├── render/           # Pure-Rust DOCX / XLSX rendering
│       └── routes/           # API routes (12 modules)
│           ├── chat.rs       # Streaming agent conversation over SSE
│           ├── auth.rs       # Login / registration / verification code
│           ├── session.rs    # Session management
│           ├── project.rs    # Project management
│           ├── file.rs       # File upload / download
│           ├── settings.rs   # Model switching / system settings
│           ├── dashboard.rs  # Statistics dashboard
│           ├── doc_export.rs # Document export
│           ├── notification.rs # Notifications
│           ├── embed.rs      # Embedded static assets
│           └── health.rs     # Health check
├── frontend/                 # React front end (~9,000 lines)
│   └── src/
│       ├── pages/            # Studio / Login / Files
│       ├── components/       # Chat / artifact display / slide preview / toolbar / settings
│       ├── api/              # Unified API layer (SSE parsing)
│       ├── stores/           # Zustand state
│       ├── lib/              # Utilities
│       ├── types/            # TypeScript type definitions
│       └── config/           # Front-end configuration
├── migrations/               # SQL migration scripts
├── Dockerfile                # Three-stage build (front end → Rust → runtime)
├── docker-compose-walioffice.yml  # Docker Compose deployment
├── build.sh                  # Build and push the image
├── .env.example              # Environment-variable template
└── ARCHITECTURE.md           # Architecture design document
```

## 🚀 Quick start

### Prerequisites

- **Rust** 1.88+ (with cargo)
- **Node.js** 20+ (with npm)
- **ffmpeg** (optional; only needed for the local video fallback)

### 1. Configure environment variables

```bash
cp .env.example .env
```

Edit `.env` and fill in the required settings:

```bash
# ── Security ──
AIPPT_JWT_SECRET=your-random-secret-at-least-32-chars

# ── Text LLM (required) ──
LLM_TEXT_BASE_URL=http://127.0.0.1:8777/v1
LLM_TEXT_API_KEY=your-api-key
LLM_TEXT_MODELS=glm_for_coding

# ── Image model (required) ──
LLM_IMAGE_BASE_URL=https://apihub.agnes-ai.com
LLM_IMAGE_API_KEY=your-api-key
LLM_IMAGE_MODELS=agnes-image-2.1-flash

# ── Video model (required) ──
LLM_VIDEO_BASE_URL=https://apihub.agnes-ai.com
LLM_VIDEO_API_KEY=your-api-key
LLM_VIDEO_MODELS=agnes-video-v2.0
```

> See `.env.example` for every option, including search sources, data directories and CORS.

### 2. Local development

```bash
# Option 1: start front end and back end together (recommended)
npm install
npm run dev
# Front end: http://localhost:5173 (hot reload)
# Back end:  http://localhost:8000

# Option 2: start them separately
cd server && cargo run          # Terminal 1: Rust server
cd frontend && npm install && npm run dev  # Terminal 2: front-end dev server
```

### 3. Production build (single binary)

```bash
# Build the front end → build the back end (the front-end build is embedded automatically)
npm run build

# Run
./server/target/release/walioffice
# Open http://localhost:8000
```

### 4. Docker deployment

```bash
# Use the prebuilt image
docker compose -f docker-compose-walioffice.yml up -d

# Or build from source
docker build -t walioffice .
docker run -p 8000:8000 -v ./data:/app/data walioffice
```

> For a full cloud-server deployment (including Nginx + HTTPS), see [DEPLOY.md](DEPLOY.md).

## 🔧 Agent tools

The agent orchestrates these tools automatically through its ReAct loop; users never have to pick one:

| Tool | What it does | Output |
|------|------|----------|
| `ppt_plan` | Plans a slide deck's outline and structure | JSON |
| `ppt_generate` | Generates a complete deck (with visual design) | Slide preview |
| `doc_generate` | Generates documents (reports / PRDs / plans, etc.) | Exportable `.docx` |
| `md_generate` | Generates Markdown documents | Markdown |
| `sheet_generate` | Generates structured tables | Exportable `.xlsx` |
| `chart_generate` | Generates visual charts from data | Chart |
| `drawio_generate` | Generates flowcharts / architecture diagrams | draw.io XML |
| `image_prompt` | Writes an image prompt and calls text-to-image | Image |
| `video_generate` | Calls a text-to-video model | MP4 |
| `web_search` | Searches the web for current information | Search result summary |

## 📡 API

| Route | Method | Description |
|------|------|------|
| `/api/auth/login` | POST | Username + password login |
| `/api/auth/register` | POST | Register |
| `/api/auth/me` | GET | Current user |
| `/api/chat/stream` | POST | Streaming agent conversation over SSE |
| `/api/chat/sessions` | GET | Session list |
| `/api/session/:id` | GET | Session details |
| `/api/session/:id` | DELETE | Delete a session |
| `/api/project/list` | GET | Project list |
| `/api/project/:id` | GET | Project details |
| `/api/file/upload` | POST | Upload a file |
| `/api/file/download/:id` | GET | Download a file |
| `/api/doc/export` | POST | Export DOCX |
| `/api/excel/export` | POST | Export XLSX |
| `/api/settings/models` | GET | Available models |
| `/api/settings/model` | PUT | Switch the current model |
| `/api/dashboard/stats` | GET | Statistics |
| `/api/notification/list` | GET | Notifications |
| `/api/health` | GET | Health check |

## 🐳 Docker build

The Dockerfile uses a three-stage build:

1. **Stage 1 — front end**: `node:20-slim`, npm ci + build
2. **Stage 2 — Rust**: `rust:1.88-bookworm`, cargo build --release (using the USTC crates mirror)
3. **Stage 3 — runtime**: `ubuntu:22.04`, containing only the binary + ffmpeg + minimal dependencies

The runtime image is based on `ubuntu:22.04` with ffmpeg installed. The walioffice binary (release build, with the front end and the DataFusion data engine embedded) is about 69 MB and ready to run. A first release build (LTO + a single codegen unit) takes about 8–9 minutes.

## ⚙️ Environment variables

| Variable | Required | Default | Description |
|------|------|--------|------|
| `AIPPT_JWT_SECRET` | ✅ | — | JWT signing key; must be changed in production |
| `AIPPT_PORT` | ❌ | `8000` | Server port |
| `AIPPT_HOST` | ❌ | `0.0.0.0` | Listen address |
| `LLM_TEXT_BASE_URL` | ✅ | — | Text LLM API URL |
| `LLM_TEXT_API_KEY` | ✅ | — | Text LLM API key |
| `LLM_TEXT_MODELS` | ✅ | — | Available text models (comma-separated) |
| `LLM_IMAGE_BASE_URL` | ✅ | — | Image model API URL |
| `LLM_IMAGE_API_KEY` | ✅ | — | Image model API key |
| `LLM_IMAGE_MODELS` | ✅ | — | Available image models |
| `LLM_VIDEO_BASE_URL` | ✅ | — | Video model API URL |
| `LLM_VIDEO_API_KEY` | ✅ | — | Video model API key |
| `LLM_VIDEO_MODELS` | ✅ | — | Available video models |
| `AIPPT_WEB_SEARCH_PROVIDER` | ❌ | `auto` | Search source: `auto` / `searxng` / `duckduckgo` |
| `AIPPT_CORS_ORIGINS` | ❌ | — | Allowed CORS origins (comma-separated) |
| `AIPPT_DATA_DIR` | ❌ | `data` | Data storage directory |

> See [`.env.example`](.env.example) for the full configuration.

## 📌 Signing in

- The login page (`/login`) uses **username + password**; if you have no account you can register on the same page (username at least 3 characters, password at least 6)
- The old verification-code login endpoint `/api/auth/verification-login` is still in the back end; set `WALIOFFICE_X_API_AUTH_LOGIN_URL` to a verification service if you need it. The login page no longer uses it
- The admin console has been removed for now, and no default admin account is created at startup

## 📄 Related documents

- [Architecture](ARCHITECTURE.md) — technical layers, module responsibilities, data flow
- [Deployment guide](DEPLOY.md) — full cloud-server deployment (Docker + Nginx + HTTPS)

## 📜 License

MIT

## Editor suite (suite/)

`suite/` is a git submodule pointing to [CrstalLi02/officepilot-suite](https://github.com/CrstalLi02/officepilot-suite): a desktop editor suite (Docs, Sheets, Slides, PDF, Markdown, HTML) modified from [GenOffice](https://github.com/genspark-ai/genoffice) (Mainfunc, Inc., Apache-2.0). Most of the editor code comes from GenOffice; OfficePilot's changes are listed in `suite/OFFICEPILOT_CHANGES.md`. GenOffice and Genspark are trademarks of Mainfunc, Inc.; this project is not affiliated with Mainfunc, Inc.

```bash
git submodule update --init suite
cd suite && npm ci && npm run native:build -w @genoffice/sheets && npm run build:all
```

In the desktop app, every Word, Excel, PowerPoint, PDF, Markdown or HTML file in My Files has an **Open in editor** button. OfficePilot copies the file to a working copy under `<data dir>/open/`, opens it in OfficePilot Suite, and syncs every save back into the workspace as an undoable operation. If the AI changes the same file while it is open in the editor, both versions are kept: the original keeps the AI's change and the editor's version is saved next to it as `<name>（编辑器版本）`.

```bash
scripts/build-suite.sh      # suite/apps/shell/release/mac-arm64/OfficePilot Suite.app (also used in development)
scripts/build-desktop.sh    # OfficePilot.app with the suite inside Contents/Resources/suite/, plus a dmg
```
