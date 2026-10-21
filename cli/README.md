# officepilot 命令行

在本机创建、读取、修改、转换真实的 Word / Excel / PPT 文件。编辑只改写涉及的部分，其余内容逐字节保留；一批操作要么全部生效，要么文件不变。命令行不调用任何大模型或网络服务。

## 安装

```bash
cargo install --path cli
```

## 快速上手

```bash
officepilot info 报告.docx                                   # 概况
officepilot read 报告.docx --from 0 --to 30                  # 按块读取
officepilot apply 报告.docx --ops ops.json --dry-run         # 预览改动
officepilot apply 报告.docx --ops ops.json --track           # 写成 Word 修订
officepilot create --type docx --from notes.md --out 周报.docx
officepilot create --type xlsx --from data.json --out 销售.xlsx
officepilot deck start deck --style style.json --outline outline.json
officepilot deck page deck 1 pages/01.json
officepilot deck build deck --out 汇报.pptx
officepilot slides audit 汇报.pptx
officepilot convert 销售.xlsx --to csv
officepilot guide deck                                       # 内置说明
```

所有命令加 `--json` 输出一个 JSON 对象，失败时退出码为 1，`error` 说明原因、`hint` 给出修正方法。

## 给编程 Agent 用

把 `cli/skill/officepilot/` 复制到 Agent 的 skills 目录（例如 Claude Code 的 `~/.claude/skills/officepilot/`），Agent 就会在需要 Office 文件时调用 officepilot。

## MCP 服务端

所有命令也以 MCP 工具提供（19 个工具，外加 `officepilot://guide/*` 资源）：

```bash
claude mcp add --transport stdio officepilot -- officepilot mcp
```

Cursor / Claude Desktop 等客户端：

```json
{ "mcpServers": { "officepilot": { "command": "officepilot", "args": ["mcp"] } } }
```

给其它机器或容器用（Streamable HTTP，兼容旧版 SSE）：

```bash
officepilot mcp --http 3093 --host 0.0.0.0 --token "$OFFICEPILOT_MCP_TOKEN"
curl -T 报告.docx -H "Authorization: Bearer $OFFICEPILOT_MCP_TOKEN" http://服务器:3093/files/报告.docx   # 返回 url，工具的 file 参数可直接用
```

- stdio 默认与命令行权限相同；设置 `OFFICEPILOT_ALLOWED_ROOTS`（多个目录用 `:` 分隔）后只能访问这些目录。
- HTTP 默认只监听本机；监听其它地址必须设置令牌。每个会话只能访问自己的临时目录和上传的文件，不会去下载外部链接；空闲 1 小时后清理。
- 写出的文件返回下载地址，1 MB 以内的同时内嵌在结果里。
