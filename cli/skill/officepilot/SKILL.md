---
name: officepilot
description: 在本机创建、读取、修改、转换真实的 Word（.docx）、Excel（.xlsx）、PowerPoint（.pptx）文件。用户要你写报告 / 方案 / 周报成 Word，做表格、汇总、图表成 Excel，做汇报 / 路演 PPT，或修改、审阅、转换已有的 Office 文件时使用。不要用 Markdown 代替 Office 文件交付。
---

# officepilot：真实的 Office 文件

`officepilot` 是一个命令行工具，在本机直接读写 .docx / .xlsx / .pptx。编辑只改写涉及的部分，原排版、样式、图片原样保留；一批操作要么全部生效，要么文件不变。它不调用大模型：内容由你来想，它负责构建和检查。

开始前运行 `officepilot capabilities` 确认已安装；不熟悉的操作先看 `officepilot guide <主题>`（overview / docs / sheet / slides / deck / formulas）。所有命令都可以加 `--json`。

## 改已有文件
1. `officepilot info <文件>` 看概况，`officepilot read <文件>` 看结构（Word 的块序号、Excel 的单元格、PPT 的页码和形状 id）。不要凭猜测写序号。
2. 把改动写成一个 JSON 数组，先 `officepilot apply <文件> --ops ops.json --dry-run` 预览，再去掉 `--dry-run` 执行。
   - Word 需要留痕审阅时加 `--track`。
   - 不想改原文件用 `--out 新文件`。
3. 改完再 `read` 确认；PPT 再跑 `officepilot slides audit <文件>`。
4. 出错时读 `error` 和 `hint`：错误信息里会给出原文、可用的样式或形状，照着修正后重试。

## 新建
- Word：先写 Markdown（标题、列表、表格、引用、链接），`officepilot create --type docx --from notes.md --title "标题" --out 报告.docx`。
- Excel：写 JSON（每个工作表的 rows、formats、charts），汇总和统计写成公式（`=SUM(...)`、`=SUMIF(...)`），不要写死结果；`officepilot create --type xlsx --from data.json --out 表.xlsx`。
- PPT：按 `officepilot guide deck` 的分阶段流程：
  1. `deck start` 写风格表（配色、字体）和大纲；
  2. 每页写一个页面规格，`deck page` 提交，检查不通过就按提示修改后重交；
  3. `deck build` 组装；不满意的页用 `deck replace` 重做。
  一页一个观点，要点不超过 6 条，数据页用图表或 metrics。

## 转换
`officepilot convert <文件> --to md|txt|html|csv|docx|xlsx`，全部在本机完成。转 PDF 和渲染图片暂不支持。

## 交付
完成后告诉用户文件路径和做了哪些改动；需要时 `officepilot open <文件>` 打开。
