# officepilot 总览

officepilot 在本机创建、读取、修改、转换真实的 .docx / .xlsx / .pptx。编辑只改写涉及的部分，其余内容逐字节保留；一批操作要么全部生效，要么文件不变。命令里不调用任何大模型。

## 工作流程
1. 看结构：`officepilot info <文件>`，再 `officepilot read <文件> [--from/--to | --sheet/--range]`。
2. 写操作：把一批操作写成 JSON，`officepilot apply <文件> --ops ops.json --dry-run` 先预览，确认后去掉 `--dry-run`。
3. 自查：再 `read` 一遍；PPT 用 `officepilot slides audit <文件>`。
4. 交给用户：`officepilot open <文件>`。

## 常用命令
- `info / read / apply`：查看与原格式编辑（Word 修订用 `--track`，另存用 `--out`，防止并发修改传 `--revision`）
- `create --type docx --from notes.md --out x.docx`：Markdown → Word
- `create --type xlsx --from table.json --out x.xlsx`：JSON / CSV → Excel（`=` 开头的保持为实时公式）
- `deck start / page / build / replace`：分阶段做 PPT，每页先检查再保留（见 `guide deck`）
- `convert <文件> --to md|txt|html|csv|docx|xlsx`
- 所有命令加 `--json` 输出一个 JSON 对象；失败时退出码为 1，`error` 说明原因，`hint` 给出修正方法。

## 原则
- 先读再改，不要凭记忆写块序号、单元格或页码。
- 改已有文件用 apply，不要重新生成一份。
- 数字来自数据：汇总用公式，不要写死计算结果。

更多：`officepilot guide docs | sheet | slides | deck | formulas`
