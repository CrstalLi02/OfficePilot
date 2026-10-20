# 路由评测报告 · e1-full

- 时间：2026-10-20 13:21:42
- 模型：`deepseek-v4-pro`
- 模式：craft
- 用例：75 条 × 1 次

| 指标 | 值 |
|---|---|
| 通过率 | **92.0%** (69/75) |
| 含准备步骤的通过率 | 97.3% (73/75)：第一轮只做了加载技能、找 / 读文件、搜索、提交计划，且未调用禁止的工具 |
| 调用失败 | 0 |
| 调用工具时平均 tool_call 数 | 1.14 |
| 单轮多工具比例 | 12.3% |
| 延迟 p50 / p95 | 2511 ms / 10185 ms |

## 分类

| 分类 | 通过 |
|---|---|
| attachment | 2/2 |
| chart | 4/4 |
| chat | 3/3 |
| compound | 2/3 |
| data | 4/5 |
| doc | 2/3 |
| drawio | 3/3 |
| image | 3/3 |
| image_understanding | 2/2 |
| markdown | 3/3 |
| office-edit | 8/8 |
| ppt | 2/3 |
| reference | 2/2 |
| schedule | 4/4 |
| search | 3/3 |
| sheet | 3/3 |
| skill | 4/4 |
| text_first | 4/4 |
| ui_mode | 1/1 |
| video | 2/4 |
| workspace | 8/8 |

## 用例明细

| 用例 | 通过率 | 选择的工具 | 问题 |
|---|---|---|---|
| `ppt-basic` | 0% | use_skill | （准备步骤）未调用 ["ppt_plan", "ppt_generate"] 中任何一个 |
| `ppt-en` | 100% | ppt_plan |  |
| `ppt-training` | 100% | ppt_plan |  |
| `doc-prd` | 100% | use_skill |  |
| `doc-report` | 0% | web_search | （准备步骤）未调用 ["doc_generate", "md_generate", "use_skill"] 中任何一个 |
| `doc-plan` | 100% | doc_generate |  |
| `md-readme` | 100% | md_generate |  |
| `md-minutes` | 100% | use_skill |  |
| `md-kb` | 100% | md_generate |  |
| `sheet-budget` | 100% | sheet_generate |  |
| `sheet-schedule` | 100% | sheet_generate |  |
| `sheet-xlsx-en` | 100% | sheet_generate |  |
| `chart-trend` | 100% | chart_generate |  |
| `chart-share` | 100% | web_search |  |
| `chart-funnel` | 100% | chart_generate |  |
| `drawio-flow` | 100% | drawio_generate |  |
| `drawio-arch` | 100% | drawio_generate |  |
| `drawio-er` | 100% | drawio_generate |  |
| `image-poster` | 100% | image_prompt |  |
| `image-logo` | 100% | image_prompt |  |
| `image-edit-attach` | 100% | image_prompt |  |
| `video-simple` | 100% | video_generate |  |
| `video-animate-attach` | 0% | （文本） | 缺少 ["video_generate"] |
| `video-cartoon` | 0% | （文本） | 未调用 ["video_generate", "video_storyboard"] 中任何一个 |
| `video-complex` | 100% | video_storyboard |  |
| `text-first-prompt` | 100% | md_generate |  |
| `text-first-script` | 100% | md_generate |  |
| `text-first-plan` | 100% | （文本） |  |
| `text-first-imgprompt` | 100% | （文本） |  |
| `understand-what` | 100% | （文本） |  |
| `understand-ocr` | 100% | （文本） |  |
| `search-news` | 100% | web_search |  |
| `search-policy` | 100% | web_search |  |
| `search-then-md` | 100% | web_search |  |
| `chat-hello` | 100% | （文本） |  |
| `chat-qa` | 100% | （文本） |  |
| `chat-capability` | 100% | （文本） |  |
| `compound-outline-then-ppt` | 0% | use_skill | （准备步骤）未调用 ["md_generate", "ppt_plan", "doc_generate"] 中任何一个 |
| `multi-ppt-drawio-doc` | 100% | doc_generate+drawio_generate+ppt_plan |  |
| `multi-sheet-chart` | 100% | chart_generate+sheet_generate |  |
| `ref-image-to-video` | 100% | video_generate |  |
| `ref-modify-ppt` | 100% | workspace_list |  |
| `attach-text-summarize` | 100% | ppt_plan |  |
| `attach-text-qa` | 100% | （文本） |  |
| `ui-image-mode-text-first` | 100% | image_prompt |  |
| `ws-find` | 100% | workspace_search |  |
| `ws-list` | 100% | workspace_list |  |
| `ws-read-summarize` | 100% | workspace_search |  |
| `ws-organize` | 100% | workspace_list |  |
| `ws-convert` | 100% | workspace_list |  |
| `ws-save-note` | 100% | workspace_organize+workspace_write |  |
| `ws-ppt-from-file` | 100% | workspace_search |  |
| `ws-ask-mode-read` | 100% | workspace_search |  |
| `data-profile` | 100% | data_profile |  |
| `data-agg` | 0% | workspace_search | （准备步骤）未调用 ["data_profile", "data_query"] 中任何一个 |
| `data-chart` | 100% | data_profile |  |
| `data-report` | 100% | use_skill |  |
| `data-csv-convert` | 100% | workspace_list |  |
| `chart-no-file` | 100% | chart_generate |  |
| `skill-weekly` | 100% | use_skill |  |
| `skill-official` | 100% | use_skill |  |
| `skill-resume` | 100% | use_skill+workspace_list |  |
| `skill-competitor` | 100% | use_skill |  |
| `schedule-weekly-report` | 100% | schedule_create |  |
| `schedule-daily-news` | 100% | schedule_create |  |
| `schedule-monthly` | 100% | schedule_create |  |
| `schedule-list` | 100% | schedule_list |  |
| `ws-docx-edit` | 100% | workspace_search |  |
| `ws-docx-track` | 100% | docs_read |  |
| `ws-docx-insert` | 100% | docs_read |  |
| `ws-xlsx-formula` | 100% | sheet_read |  |
| `ws-xlsx-fix` | 100% | sheet_read |  |
| `ws-pptx-edit` | 100% | slides_read |  |
| `ws-pptx-notes` | 100% | slides_read |  |
| `new-doc-not-edit` | 100% | use_skill |  |
