---
name: data-analysis-report
description: 基于用户的 Excel / CSV 数据做分析并写成数据报告：先摸清数据结构，再用 SQL 计算关键指标、趋势、构成和异常，配图表，最后写出有结论、有依据的报告。用户提供数据文件并要求"分析一下""出个数据报告""看看趋势"时使用。
allowed-tools: data_profile data_query doc_generate md_generate ppt_plan ppt_generate workspace_list workspace_search workspace_read workspace_write
metadata: {"walioffice": {"category": "数据", "icon": "chart"}}
---

# 数据分析报告

## 流程
1. **找到数据**：用户没有指明文件时，用 workspace_search / workspace_list 找到数据文件并确认。
2. **摸清结构**：调用 data_profile，记下表名、关键维度（时间、地区、产品、渠道等）和指标列；留意空值、异常值和单位。
3. **规划问题**：先列出 3–5 个要回答的业务问题，例如：
   - 总量和增速如何？（总计、同比 / 环比）
   - 结构怎样？（按维度的占比和排名）
   - 趋势如何？（按时间聚合）
   - 有没有异常？（离群值、突变点）
4. **计算**：每个问题对应一次 data_query；互不依赖的查询放在同一轮并行调用；需要图表时直接在 data_query 里传 chart 参数（趋势用 line，构成用 pie，对比和排名用 bar）。
5. **写报告**（doc_generate，或用户要求的格式）：
   - 开头一段"核心结论"，3 条以内，每条带具体数字；
   - 每一节：结论 → 数据依据 → 可能的原因（标明是推断）→ 建议；
   - 末尾写数据口径和局限（数据范围、缺失值处理方式）。

## 硬性要求
- 报告里的每个数字都必须来自 data_query 的结果，不要估算或编造。
- 同比 / 环比要写清楚比较的基期。
- 发现数据质量问题（大量空值、重复、口径不一致）要在报告里说明。
