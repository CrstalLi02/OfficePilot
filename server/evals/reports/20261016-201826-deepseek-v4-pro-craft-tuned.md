# 路由评测报告 · tuned

- 时间：2026-10-16 20:18:26
- 模型：`deepseek-v4-pro`
- 模式：craft
- 用例：63 条 × 3 次

| 指标 | 值 |
|---|---|
| 通过率 | **88.4%** (167/189) |
| 含准备步骤的通过率 | 97.4% (184/189)：第一轮只做了加载技能、找 / 读文件、搜索、提交计划，且未调用禁止的工具 |
| 调用失败 | 1 |
| 调用工具时平均 tool_call 数 | 1.23 |
| 单轮多工具比例 | 17.3% |
| 延迟 p50 / p95 | 2787 ms / 13821 ms |

## 分类

| 分类 | 通过 |
|---|---|
| attachment | 6/6 |
| chart | 12/12 |
| chat | 9/9 |
| compound | 6/9 |
| data | 10/15 |
| doc | 6/9 |
| drawio | 9/9 |
| image | 8/9 |
| image_understanding | 6/6 |
| markdown | 9/9 |
| ppt | 6/9 |
| reference | 6/6 |
| search | 9/9 |
| sheet | 9/9 |
| skill | 12/12 |
| text_first | 12/12 |
| ui_mode | 3/3 |
| video | 8/12 |
| workspace | 21/24 |

## 用例明细

| 用例 | 通过率 | 选择的工具 | 问题 |
|---|---|---|---|
| `ppt-basic` | 0% | use_skill / workspace_list+workspace_search / use_skill | （准备步骤）未调用 ["ppt_plan", "ppt_generate"] 中任何一个 |
| `ppt-en` | 100% | ppt_plan / ppt_plan / ppt_plan |  |
| `ppt-training` | 100% | ppt_plan / ppt_plan / ppt_plan |  |
| `doc-prd` | 100% | use_skill / use_skill / use_skill |  |
| `doc-report` | 0% | web_search / web_search / web_search | （准备步骤）未调用 ["doc_generate", "md_generate", "use_skill"] 中任何一个 |
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
| `image-edit-attach` | 67% | image_prompt / image_prompt / （文本） | 缺少 ["image_prompt"] |
| `video-simple` | 100% | video_generate / video_generate / video_generate |  |
| `video-animate-attach` | 67% | video_generate / （文本） / video_generate | 缺少 ["video_generate"] |
| `video-cartoon` | 0% | （文本） / （文本） / （文本） | LLM 调用失败: error sending request for url (https://api.deepseek.com/v1/chat/completions)；未调用 ["video_generate", "video_storyboard"] 中任何一个 |
| `video-complex` | 100% | video_storyboard / video_storyboard / video_storyboard |  |
| `text-first-prompt` | 100% | md_generate / md_generate / md_generate |  |
| `text-first-script` | 100% | md_generate / md_generate / md_generate |  |
| `text-first-plan` | 100% | md_generate / md_generate / md_generate |  |
| `text-first-imgprompt` | 100% | md_generate / md_generate / （文本） |  |
| `understand-what` | 100% | （文本） / （文本） / （文本） |  |
| `understand-ocr` | 100% | （文本） / （文本） / （文本） |  |
| `search-news` | 100% | web_search / web_search / web_search |  |
| `search-policy` | 100% | web_search / web_search / web_search |  |
| `search-then-md` | 100% | web_search / web_search / web_search |  |
| `chat-hello` | 100% | （文本） / （文本） / （文本） |  |
| `chat-qa` | 100% | （文本） / （文本） / （文本） |  |
| `chat-capability` | 100% | （文本） / （文本） / （文本） |  |
| `compound-outline-then-ppt` | 0% | use_skill / use_skill / use_skill | （准备步骤）未调用 ["md_generate", "ppt_plan", "doc_generate"] 中任何一个 |
| `multi-ppt-drawio-doc` | 100% | doc_generate+drawio_generate+ppt_plan / doc_generate+drawio_generate+ppt_plan / doc_generate+drawio_generate+ppt_plan |  |
| `multi-sheet-chart` | 100% | chart_generate+sheet_generate / chart_generate+sheet_generate / chart_generate+sheet_generate |  |
| `ref-image-to-video` | 100% | video_generate / video_generate / video_generate |  |
| `ref-modify-ppt` | 100% | workspace_list / workspace_list / artifact_edit |  |
| `attach-text-summarize` | 100% | ppt_plan / ppt_plan / ppt_plan |  |
| `attach-text-qa` | 100% | （文本） / （文本） / （文本） |  |
| `ui-image-mode-text-first` | 100% | （文本） / （文本） / （文本） |  |
| `ws-find` | 100% | workspace_list+workspace_search / workspace_search / workspace_search |  |
| `ws-list` | 100% | workspace_list / workspace_list / workspace_list |  |
| `ws-read-summarize` | 67% | workspace_search / workspace_search / workspace_list | （准备步骤）未调用 ["workspace_read", "workspace_search"] 中任何一个 |
| `ws-organize` | 100% | workspace_list / workspace_list / workspace_list |  |
| `ws-convert` | 100% | workspace_list / workspace_list / workspace_list |  |
| `ws-save-note` | 33% | workspace_list / workspace_list / workspace_write | （准备步骤）未调用 ["workspace_write", "workspace_organize"] 中任何一个 |
| `ws-ppt-from-file` | 100% | workspace_list / workspace_list+workspace_search / workspace_list |  |
| `ws-ask-mode-read` | 100% | workspace_search / workspace_search / workspace_search |  |
| `data-profile` | 100% | data_profile / data_profile / data_profile |  |
| `data-agg` | 33% | workspace_list / data_profile / workspace_list | （准备步骤）未调用 ["data_profile", "data_query"] 中任何一个 |
| `data-chart` | 0% | workspace_list / workspace_search / workspace_list | （准备步骤）未调用 ["data_profile", "data_query"] 中任何一个 |
| `data-report` | 100% | data_profile+use_skill / use_skill+workspace_list / use_skill+workspace_search |  |
| `data-csv-convert` | 100% | workspace_list / workspace_list / workspace_list |  |
| `chart-no-file` | 100% | chart_generate / chart_generate / chart_generate |  |
| `skill-weekly` | 100% | use_skill / use_skill / use_skill |  |
| `skill-official` | 100% | use_skill / use_skill / use_skill |  |
| `skill-resume` | 100% | use_skill+workspace_list / use_skill+workspace_list / use_skill+workspace_list |  |
| `skill-competitor` | 100% | use_skill / use_skill / use_skill |  |
