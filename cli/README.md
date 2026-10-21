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
