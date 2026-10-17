# 路由评测报告 · current

- 时间：2026-10-16 20:13:12
- 模型：`deepseek-v4-pro`
- 模式：craft
- 用例：63 条 × 3 次

| 指标 | 值 |
|---|---|
| 通过率 | **89.4%** (169/189) |
| 调用失败 | 0 |
| 调用工具时平均 tool_call 数 | 1.19 |
| 单轮多工具比例 | 17.4% |
| 延迟 p50 / p95 | 2615 ms / 13301 ms |

## 分类

| 分类 | 通过 |
|---|---|
| attachment | 6/6 |
| chart | 12/12 |
| chat | 9/9 |
| compound | 6/9 |
| data | 8/15 |
| doc | 6/9 |
| drawio | 9/9 |
| image | 9/9 |
| image_understanding | 6/6 |
| markdown | 9/9 |
| ppt | 4/9 |
| reference | 6/6 |
| search | 9/9 |
| sheet | 9/9 |
| skill | 12/12 |
| text_first | 12/12 |
| ui_mode | 3/3 |
| video | 11/12 |
| workspace | 23/24 |

## 用例明细

| 用例 | 通过率 | 选择的工具 | 问题 |
|---|---|---|---|
| `ppt-basic` | 0% | use_skill / use_skill / use_skill | 未调用 ["ppt_plan", "ppt_generate"] 中任何一个 |
| `ppt-en` | 33% | use_skill / use_skill / ppt_plan | 未调用 ["ppt_plan", "ppt_generate"] 中任何一个 |
| `ppt-training` | 100% | ppt_plan / ppt_plan / ppt_plan |  |
| `doc-prd` | 100% | use_skill / use_skill / use_skill |  |
| `doc-report` | 0% | web_search / web_search / web_search | 未调用 ["doc_generate", "md_generate", "use_skill"] 中任何一个 |
| `doc-plan` | 100% | doc_generate / doc_generate / doc_generate |  |
| `md-readme` | 100% | md_generate / md_generate / md_generate |  |
| `md-minutes` | 100% | use_skill / use_skill / use_skill |  |
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
| `video-cartoon` | 67% | video_storyboard / video_storyboard / （文本） | 未调用 ["video_generate", "video_storyboard"] 中任何一个 |
| `video-complex` | 100% | video_storyboard / video_storyboard / video_storyboard |  |
| `text-first-prompt` | 100% | md_generate / md_generate / md_generate |  |
| `text-first-script` | 100% | md_generate / md_generate / md_generate |  |
| `text-first-plan` | 100% | md_generate / md_generate / md_generate |  |
| `text-first-imgprompt` | 100% | （文本） / md_generate / md_generate |  |
| `understand-what` | 100% | （文本） / （文本） / （文本） |  |
| `understand-ocr` | 100% | （文本） / workspace_list / （文本） |  |
| `search-news` | 100% | web_search / web_search / web_search |  |
| `search-policy` | 100% | web_search / web_search / web_search |  |
| `search-then-md` | 100% | web_search / web_search / web_search |  |
| `chat-hello` | 100% | （文本） / （文本） / （文本） |  |
| `chat-qa` | 100% | （文本） / （文本） / （文本） |  |
| `chat-capability` | 100% | （文本） / （文本） / （文本） |  |
| `compound-outline-then-ppt` | 0% | use_skill / use_skill / use_skill | 未调用 ["md_generate", "ppt_plan", "doc_generate"] 中任何一个 |
| `multi-ppt-drawio-doc` | 100% | doc_generate+drawio_generate+use_skill / doc_generate+drawio_generate+use_skill / doc_generate+drawio_generate+use_skill |  |
| `multi-sheet-chart` | 100% | chart_generate+sheet_generate / chart_generate+sheet_generate / chart_generate+sheet_generate |  |
| `ref-image-to-video` | 100% | video_generate / video_generate / video_generate |  |
| `ref-modify-ppt` | 100% | workspace_list / workspace_list / artifact_edit |  |
| `attach-text-summarize` | 100% | ppt_plan / ppt_plan / ppt_plan |  |
| `attach-text-qa` | 100% | （文本） / （文本） / （文本） |  |
| `ui-image-mode-text-first` | 100% | （文本） / （文本） / （文本） |  |
| `ws-find` | 100% | workspace_search / workspace_search / workspace_search |  |
| `ws-list` | 100% | workspace_list / workspace_list / workspace_list |  |
| `ws-read-summarize` | 100% | workspace_search / workspace_search / workspace_search |  |
| `ws-organize` | 100% | workspace_list / workspace_list / workspace_list |  |
| `ws-convert` | 100% | workspace_list / workspace_list / workspace_list |  |
| `ws-save-note` | 67% | workspace_list / workspace_organize+workspace_write / workspace_organize+workspace_write | 未调用 ["workspace_write", "workspace_organize"] 中任何一个 |
| `ws-ppt-from-file` | 100% | workspace_list / workspace_search / workspace_list+workspace_search |  |
| `ws-ask-mode-read` | 100% | workspace_search / workspace_search / workspace_search |  |
| `data-profile` | 67% | data_profile / data_profile / workspace_list | 未调用 ["data_profile", "workspace_read"] 中任何一个 |
| `data-agg` | 0% | workspace_list / workspace_list / workspace_search | 未调用 ["data_profile", "data_query"] 中任何一个 |
| `data-chart` | 0% | workspace_search / workspace_list / workspace_search | 未调用 ["data_profile", "data_query"] 中任何一个 |
| `data-report` | 100% | use_skill+workspace_list / use_skill+workspace_list / use_skill+workspace_list |  |
| `data-csv-convert` | 100% | workspace_list / workspace_list / workspace_list |  |
| `chart-no-file` | 100% | chart_generate / chart_generate / chart_generate |  |
| `skill-weekly` | 100% | use_skill / use_skill / use_skill |  |
| `skill-official` | 100% | use_skill / use_skill / use_skill |  |
| `skill-resume` | 100% | use_skill+workspace_list / use_skill+workspace_list / use_skill+workspace_list |  |
| `skill-competitor` | 100% | use_skill / use_skill / use_skill |  |
