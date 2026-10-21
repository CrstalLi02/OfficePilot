# Excel（.xlsx）操作

`officepilot read x.xlsx --sheet 名称 --range A1:H40`：列出工作表，单元格显示为 `值〔=公式〕`。

```json
[
  {"op":"set","sheet":"订单","cell":"D2","value":"=B2*C2"},
  {"op":"set_range","sheet":"汇总","start":"A1","values":[["地区","金额"],["华东","=SUMIF(订单!A:A,A2,订单!C:C)"]]},
  {"op":"clear","sheet":"订单","range":"A10:F20"},
  {"op":"add_sheet","name":"汇总","values":[["地区","金额"]]},
  {"op":"rename_sheet","sheet":"Sheet1","name":"订单"},
  {"op":"format","sheet":"汇总","range":"A1:B1","bold":true,"fill":"#DBEAFE","align":"center"},
  {"op":"format","sheet":"汇总","range":"B2:B9","number_format":"#,##0.00"},
  {"op":"column_width","sheet":"汇总","columns":"A:B","width":16},
  {"op":"freeze","sheet":"汇总","cell":"A2"},
  {"op":"add_chart","sheet":"汇总","type":"column","title":"各地区金额","categories":"汇总!A2:A5","series":[{"name":"金额","values":"汇总!B2:B5"}],"anchor":"D2"}
]
```

- 值：数字、文字、true/false；以 `=` 开头的是公式，保持实时计算，同时写入算好的结果；`null` 清空。
- 依赖改动单元格的原有公式会一起更新结果；Excel / WPS 打开时也会重新计算。
- 数字格式：`0`、`0.00`、`#,##0`、`0%`、`0.0%`、`yyyy-mm-dd`，或 percent / currency / date / text。
- 图表类型：column / bar / line / pie；区域写成 `工作表!A2:A5`。
- 新建：`officepilot create --type xlsx --from data.json --out x.xlsx`，data.json 为
  `{"sheets":[{"name":"销售","rows":[["地区","金额"],["华东",100],["合计","=SUM(B2:B2)"]],"formats":[...],"charts":[...]}]}`，
  表头自动加粗、冻结首行、按内容设列宽。
- 公式支持见 `officepilot guide formulas`。
