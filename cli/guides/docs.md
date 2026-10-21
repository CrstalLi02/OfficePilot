# Word（.docx）操作

`officepilot read x.docx` 按块列出：`[序号] 类型 | 文字`，类型有 标题N / 段落 / 列表·N / 表格 / 内容控件。块序号都指这次读取时的文档：同一批里插入、删除不会让后面的序号错位。

```json
[
  {"op":"replace_text","block":3,"find":"2025 年","replace":"2026 年"},
  {"op":"replace_text","find":"甲方","replace":"委托方"},
  {"op":"replace_block","block":5,"text":"重写整段，支持 **加粗** *斜体* [链接](https://example.com)","style":"heading 2"},
  {"op":"insert","after":7,"markdown":"## 新章节\n正文\n- 要点\n1. 步骤\n| 列1 | 列2 |\n|---|---|\n| a | b |"},
  {"op":"insert","at":"end","markdown":"> 引用"},
  {"op":"delete","block":9},
  {"op":"delete","from":10,"to":12},
  {"op":"move","block":4,"after":10},
  {"op":"set_style","block":2,"style":"heading 1"},
  {"op":"set_cell","block":6,"row":1,"col":2,"text":"250"},
  {"op":"comment","block":3,"text":"请复核数字"}
]
```

- `replace_text`：find 要与原文逐字一致；不给 block 时全文替换；`"all":false` 只换第一处。替换沿用原格式。
- `replace_block`：重写段落文字，保留段落格式和首个文字片段的字体。表格请用 `set_cell`。
- `insert`：`after` / `before` 某块，或 `at:"start"|"end"`。支持标题、段落、无序 / 有序列表（缩进两格为下一级）、表格、引用、代码块。
- `set_style`：heading 1–6 / title / normal / quote，或文档里已有的样式名。
- 修订模式：`apply --track [--author 名字]`，改动写成 Word 修订，可在 Word 里逐条接受或拒绝（修订模式下不支持 move）。
- 带分节符（页面设置）的段落不能删除。
