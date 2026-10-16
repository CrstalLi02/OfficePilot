// 产物版本对比：把不同类型的产物转成可读的文本行，再做逐行 diff。纯函数，便于测试。

import type { Artifact } from '@/types'

export type DiffLine = { type: 'same' | 'add' | 'remove'; text: string }

function cell(value: unknown): string {
  return value === null || value === undefined ? '' : String(value)
}

/** 产物 → 便于对比的文本行 */
export function artifactToLines(artifact: Pick<Artifact, 'kind' | 'title' | 'content'>): string[] {
  const c = artifact.content || {}
  const lines: string[] = [`标题：${artifact.title}`]
  switch (artifact.kind) {
    case 'markdown':
    case 'document':
      // 去掉末尾空行，避免只因结尾换行不同而显示差异
      lines.push(...String(c.markdown || '').replace(/\s+$/, '').split('\n'))
      break
    case 'sheet':
      for (const table of Array.isArray(c.tables) ? c.tables : []) {
        lines.push(`【${cell(table.title)}】`, (table.headers || []).map(cell).join(' | '))
        for (const row of Array.isArray(table.rows) ? table.rows : []) lines.push((row || []).map(cell).join(' | '))
      }
      break
    case 'ppt':
      ;(Array.isArray(c.slides) ? c.slides : []).forEach((slide: any, index: number) => {
        lines.push(`— 第 ${index + 1} 页：${cell(slide.title)}`)
        for (const element of Array.isArray(slide.elements) ? slide.elements : []) {
          const text = cell(element?.text).trim()
          if (text) lines.push(`  ${text}`)
        }
      })
      break
    case 'chart': {
      lines.push(`类型：${cell(c.chart_type || c.type)}`)
      const labels: unknown[] = c.chart_data?.labels || []
      const values: unknown[] = c.chart_data?.values || []
      labels.forEach((label, i) => lines.push(`${cell(label)}：${cell(values[i])}`))
      break
    }
    case 'drawio':
      // 每个节点一行，方便看出增删
      lines.push(...String(c.xml || '').replace(/></g, '>\n<').split('\n'))
      break
    default:
      lines.push(...JSON.stringify(c, null, 2).split('\n'))
  }
  return lines
}

/** 最长公共子序列逐行 diff；行数过多时退化为整体替换，避免卡住界面 */
export function diffLines(before: string[], after: string[]): DiffLine[] {
  const n = before.length
  const m = after.length
  if (n * m > 4_000_000) {
    return [...before.map((text) => ({ type: 'remove' as const, text })), ...after.map((text) => ({ type: 'add' as const, text }))]
  }
  const dp: Uint32Array[] = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1))
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i][j] = before[i] === after[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1])
    }
  }
  const out: DiffLine[] = []
  let i = 0
  let j = 0
  while (i < n && j < m) {
    if (before[i] === after[j]) {
      out.push({ type: 'same', text: before[i] })
      i++
      j++
    } else if (dp[i + 1][j] >= dp[i][j + 1]) {
      out.push({ type: 'remove', text: before[i++] })
    } else {
      out.push({ type: 'add', text: after[j++] })
    }
  }
  while (i < n) out.push({ type: 'remove', text: before[i++] })
  while (j < m) out.push({ type: 'add', text: after[j++] })
  return out
}

/** 只保留改动及其上下文（context 行），其余折叠 */
export function collapseUnchanged(lines: DiffLine[], context = 2): Array<DiffLine | { type: 'skip'; count: number }> {
  const keep = lines.map((line) => line.type !== 'same')
  const visible = lines.map((_, index) => {
    for (let k = Math.max(0, index - context); k <= Math.min(lines.length - 1, index + context); k++) {
      if (keep[k]) return true
    }
    return false
  })
  const out: Array<DiffLine | { type: 'skip'; count: number }> = []
  let skipped = 0
  lines.forEach((line, index) => {
    if (visible[index]) {
      if (skipped) out.push({ type: 'skip', count: skipped })
      skipped = 0
      out.push(line)
    } else {
      skipped++
    }
  })
  if (skipped) out.push({ type: 'skip', count: skipped })
  return out
}

/** 去掉只影响预览的字段，用来判断用户是否真的改了内容 */
export function contentFingerprint(content: unknown): string {
  if (!content || typeof content !== 'object') return JSON.stringify(content ?? null)
  const { preview: _preview, ...rest } = content as Record<string, unknown>
  return JSON.stringify(rest)
}
