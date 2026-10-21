# PPT（.pptx）操作

`officepilot read x.pptx [--from 2 --to 5]`：每页列出形状 `[id] 类型「名称」 | 文字` 和备注。页码从 1 开始，同一批里都指读取时的页码。

```json
[
  {"op":"replace_text","find":"2025","replace":"2026"},
  {"op":"replace_text","slide":3,"find":"旧","replace":"新","all":false},
  {"op":"set_text","slide":2,"shape":"body","text":"第一点\n  子要点\n第二点 **重点**"},
  {"op":"set_text","slide":2,"shape":"title","text":"新标题"},
  {"op":"set_cell","slide":3,"shape":"5","row":1,"col":1,"text":"3.5 亿"},
  {"op":"set_notes","slide":2,"text":"讲稿"},
  {"op":"duplicate_slide","slide":2},
  {"op":"delete_slide","slide":4},
  {"op":"move_slide","slide":5,"to":2}
]
```

- `shape`：形状 id、名称，或 title / subtitle / body。`set_text` 每行一段，行首每两个空格缩进一级，沿用原字体格式。
- 含图表或 SmartArt 的页暂不支持复制。
- 检查版面：`officepilot slides audit x.pptx`（文字溢出、重叠、超出页面、字号过小）。
- 从零做一套 PPT 用 `officepilot guide deck`。
