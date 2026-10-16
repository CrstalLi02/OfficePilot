# Agent 路由评测

检查 Agent 在**第一轮**是否选对了工具。它复用线上的 `resolve_routing` 和 `build_initial_request` 组装请求，每条用例调用一次 LLM，只判断 tool_calls，不实际执行工具。

## 运行

```bash
cd server
cargo build --release
./target/release/walioffice eval --label baseline --repeat 3
```

模型地址和 key 读取 `.env` 中的 `LLM_TEXT_*` 配置，用 `--model` 可以覆盖模型名。报告写到 `evals/reports/<时间>-<模型>-<label>.md`，同目录下还有一份 `.json` 明细。

| 参数 | 默认 | 说明 |
|---|---|---|
| `--cases` | `evals/routing.jsonl` | 用例文件 |
| `--repeat` | 1 | 每条用例重复次数（线上 temperature=0.7，建议 ≥3） |
| `--concurrency` | 4 | 并发请求数 |
| `--filter` | – | 只跑 id 或分类包含该文本的用例 |
| `--label` | – | 报告标签，例如 `baseline`、`m2` |

mock LLM（`dev/mock_llm.py`）总是调用第一个可用工具，只能用来验证评测流程本身，**分数没有意义**。

## 用例格式（`routing.jsonl`，一行一条）

```json
{"id":"ppt-basic","category":"ppt","message":"帮我做个PPT",
 "tool_kind":null, "attachments":[{"kind":"image|text","name":"a.png","text":"..."}],
 "history":[{"role":"user","content":"..."}],
 "expect":["必须全部调用"], "expect_any":["至少调用一个"], "forbid":["不允许调用"], "expect_no_tool":false}
```

- 如果调用了 `propose_plan`，计划 `steps[].tool` 里列出的工具也算作"已选择"。M2 引入计划确认后，用例无需修改。
- 图片附件统一使用 1x1 占位 PNG，测的是意图路由，不测识图能力。
- 多产物用例只检查第一轮有没有走对方向，不检查最终是否交付了全部文件。
