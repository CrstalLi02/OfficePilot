# 路由评测报告 · baseline

- 时间：2026-10-16 20:13:12
- 模型：`deepseek-v4-pro`
- 用例：45 条 × 3 次

| 指标 | 值 |
|---|---|
| 通过率 | **96.3%** (130/135) |
| 调用失败 | 0 |
| 调用工具时平均 tool_call 数 | 1.18 |
| 单轮多工具比例 | 14.8% |
| 延迟 p50 / p95 | 2661 ms / 7336 ms |

## 分类

| 分类 | 通过 |
|---|---|
| attachment | 6/6 |
| chart | 9/9 |
| chat | 9/9 |
| compound | 9/9 |
| doc | 7/9 |
| drawio | 9/9 |
| image | 9/9 |
| image_understanding | 6/6 |
| markdown | 9/9 |
| ppt | 9/9 |
| reference | 6/6 |
| search | 9/9 |
| sheet | 9/9 |
| text_first | 12/12 |
| ui_mode | 3/3 |
| video | 9/12 |

## 用例明细

| 用例 | 通过率 | 选择的工具 | 问题 |
|---|---|---|---|
| `ppt-basic` | 100% | ppt_plan / ppt_plan / ppt_plan |  |
| `ppt-en` | 100% | ppt_plan / ppt_plan / ppt_plan |  |
| `ppt-training` | 100% | ppt_plan / ppt_plan / ppt_plan |  |
| `doc-prd` | 100% | doc_generate / doc_generate / doc_generate |  |
| `doc-report` | 33% | web_search / doc_generate / web_search | 未调用 ["doc_generate", "md_generate"] 中任何一个 |
| `doc-plan` | 100% | doc_generate / doc_generate / doc_generate |  |
| `md-readme` | 100% | md_generate / md_generate / md_generate |  |
| `md-minutes` | 100% | md_generate / md_generate / md_generate |  |
| `md-kb` | 100% | md_generate / md_generate / md_generate |  |
| `sheet-budget` | 100% | sheet_generate / sheet_generate / sheet_generate |  |
| `sheet-schedule` | 100% | sheet_generate / sheet_generate / sheet_generate |  |
| `sheet-xlsx-en` | 100% | sheet_generate / sheet_generate / sheet_generate |  |
| `chart-trend` | 100% | chart_generate / chart_generate / chart_generate |  |
| `chart-share` | 100% | web_search / web_search / web_search |  |
| `chart-funnel` | 100% | chart_generate / chart_generate / chart_generate |  |
| `drawio-flow` | 100% | drawio_generate / drawio_generate / drawio_generate |  |
| `drawio-arch` | 100% | drawio_generate / drawio_generate / drawio_generate |  |
| `drawio-er` | 100% | drawio_generate / drawio_generate / drawio_generate |  |
| `image-poster` | 100% | image_prompt / image_prompt / image_prompt |  |
| `image-logo` | 100% | image_prompt / image_prompt / image_prompt |  |
| `image-edit-attach` | 100% | image_prompt / image_prompt / image_prompt |  |
| `video-simple` | 100% | video_generate / video_generate / video_generate |  |
| `video-animate-attach` | 100% | video_generate / video_generate / video_generate |  |
| `video-cartoon` | 0% | （文本） / （文本） / （文本） | 未调用 ["video_generate", "video_storyboard"] 中任何一个 |
| `video-complex` | 100% | video_storyboard / video_storyboard / video_storyboard |  |
| `text-first-prompt` | 100% | md_generate / md_generate / md_generate |  |
| `text-first-script` | 100% | md_generate / md_generate / md_generate |  |
| `text-first-plan` | 100% | doc_generate / md_generate / doc_generate |  |
| `text-first-imgprompt` | 100% | （文本） / （文本） / （文本） |  |
| `understand-what` | 100% | （文本） / （文本） / （文本） |  |
| `understand-ocr` | 100% | （文本） / （文本） / （文本） |  |
| `search-news` | 100% | web_search / web_search / web_search |  |
| `search-policy` | 100% | web_search / web_search / web_search |  |
| `search-then-md` | 100% | web_search / web_search / web_search |  |
| `chat-hello` | 100% | （文本） / （文本） / （文本） |  |
| `chat-qa` | 100% | （文本） / （文本） / （文本） |  |
| `chat-capability` | 100% | （文本） / （文本） / （文本） |  |
| `compound-outline-then-ppt` | 100% | ppt_plan / ppt_plan / ppt_plan |  |
| `multi-ppt-drawio-doc` | 100% | doc_generate+drawio_generate+ppt_plan / doc_generate+drawio_generate+ppt_plan / doc_generate+drawio_generate+ppt_plan |  |
| `multi-sheet-chart` | 100% | chart_generate+sheet_generate / chart_generate+sheet_generate / chart_generate+sheet_generate |  |
| `ref-image-to-video` | 100% | video_generate / video_generate / video_generate |  |
| `ref-modify-ppt` | 100% | ppt_plan / ppt_plan / ppt_plan |  |
| `attach-text-summarize` | 100% | ppt_plan / ppt_plan / ppt_plan |  |
| `attach-text-qa` | 100% | （文本） / （文本） / （文本） |  |
| `ui-image-mode-text-first` | 100% | （文本） / （文本） / （文本） |  |
