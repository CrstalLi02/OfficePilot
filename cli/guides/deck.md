# 分阶段做 PPT

```bash
officepilot deck start deck --style style.json --outline outline.json   # 写风格表和大纲
officepilot deck page deck 1 pages/01.json    # 每页提交，检查通过才保留；不通过会说明原因
officepilot deck status deck                  # 哪些页还没写
officepilot deck build deck --out talk.pptx   # 组装并审查版面
officepilot deck replace deck 3 pages/03.json # 重做一页并重新组装
```

也可以一次生成：`officepilot create --type pptx --spec deck.json --out talk.pptx`，deck.json 为 `{"title","style","pages":[...]}`。
单页可先检查：`officepilot slides check page.json --style style.json --outline outline.json --page 3`。

## style.json
```json
{"size":"16:9","palette":{"background":"#FFFFFF","surface":"#F1F5F9","text":"#0F172A","muted":"#64748B","accent":"#0F766E","extra":["#F59E0B","#6366F1"]},
 "fonts":{"heading":"Microsoft YaHei","body":"Microsoft YaHei"},"title_size":32,"body_size":18}
```
只用配色表里的颜色（另加黑白），图表系列依次用 accent 和 extra。

## outline.json
```json
{"title":"Aurora X1 发布方案","pages":[{"title":"Aurora X1 发布方案","kind":"cover"},{"title":"市场机会","points":["规模","增长"]}]}
```
每页标题要和大纲一致。

## 页面规格（版式）
| layout | 需要的字段 |
|---|---|
| cover / closing / section | title, subtitle |
| bullets | title, subtitle?, bullets |
| two-column | title, left{heading,bullets}, right{heading,bullets} |
| metrics | title, metrics[{value,label,note?}]（最多 4 个）, bullets? |
| table | title, table{rows,header?,size?}, bullets? |
| chart | title, chart{type,categories,series[{name,values}],title?,labels?,number_format?}, bullets? |
| image-left / image-right | title, image{path,caption?,fit:cover\|contain}, bullets |
| quote | quote{text,by?} |
| blank | 只用 elements |

- bullets 里的字符串支持 **加粗**；`{"text":"…","level":1}` 是下一级。
- `notes` 是演讲者备注，`background` 改单页底色。
- `elements` 自由摆放（单位磅，16:9 页面 960×540）：
  `{"type":"text","x":48,"y":420,"w":400,"h":40,"text":"脚注","size":12,"color":"#64748B"}`、
  `{"type":"shape","shape":"round","x":..,"fill":"#F1F5F9"}`、`{"type":"image","path":"a.png","fit":"cover",…}`、
  `{"type":"table","x":..,"rows":[…]}`、`{"type":"chart","x":48,"y":120,"w":420,"h":300,"chart":{"type":"bar","categories":[…],"series":[…]}}`。

## 设计建议
- 一页一个观点；要点不超过 6 条、每条 40 字以内；正文字号不小于 14。
- 数据页优先用图表或 metrics，表格不超过 8 行。
- 检查给出“错误”的页必须改；“提示”酌情处理。
