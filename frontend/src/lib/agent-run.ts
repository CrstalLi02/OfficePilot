// Agent 运行状态：把 SSE 中的工具事件（tool_start / tool_call / tool_result / 带 call_id 的进度）
// 归并成每个工具调用一条记录，供 RunTimeline 渲染。纯函数，便于测试。

export type ToolRunStatus = 'pending' | 'running' | 'success' | 'error' | 'cancelled'

export interface ToolRun {
  callId: string
  tool: string
  status: ToolRunStatus
  /** 首次出现的时间（ms） */
  startedAt: number
  /** 开始执行的时间（参数生成完毕） */
  runningAt?: number
  durationMs?: number
  progress?: string
  error?: string
  /** 工作区写入 / 整理的操作 id，可撤销 */
  opId?: string
  /** 一句话结果，如“整理了 5 项” */
  summary?: string
  /** 调用前确认（插件的写操作等） */
  approval?: ToolApproval
}

export interface ToolApproval {
  id: string
  source: string
  label: string
  reason: string
  arguments: unknown
  expiresAt: number
  status: 'pending' | 'approved' | 'rejected' | 'expired' | 'cancelled'
}

export interface AgentRunState {
  tools: ToolRun[]
  usage?: { prompt_tokens: number; completion_tokens: number }
  cancelled?: boolean
}

export const EMPTY_RUN: AgentRunState = { tools: [] }

function upsert(tools: ToolRun[], callId: string, patch: (prev: ToolRun | undefined) => ToolRun): ToolRun[] {
  const index = tools.findIndex((item) => item.callId === callId)
  if (index === -1) return [...tools, patch(undefined)]
  const next = tools.slice()
  next[index] = patch(tools[index])
  return next
}

function progressText(data: any): string | undefined {
  if (!data || typeof data !== 'object') return undefined
  if (typeof data.slide_count === 'number' && typeof data.total_slides === 'number') {
    return `第 ${data.slide_count} / ${data.total_slides} 页`
  }
  const text = data.detail || data.step || data.message
  return typeof text === 'string' && text.trim() ? text.trim().slice(0, 80) : undefined
}

/** 应用一个 SSE 事件；与工具无关的事件原样返回 state */
export function applyRunEvent(state: AgentRunState, event: string, data: any, now = Date.now()): AgentRunState {
  const callId: string | undefined = data && typeof data === 'object' ? data.call_id : undefined

  switch (event) {
    case 'tool_start':
      // propose_plan 不是真正执行的工具，由计划卡片展示
      if (!callId || data.tool === 'propose_plan') return state
      return {
        ...state,
        tools: upsert(state.tools, callId, (prev) => prev ?? { callId, tool: data.tool, status: 'pending', startedAt: now }),
      }

    case 'tool_call':
      if (!callId) return state
      return {
        ...state,
        tools: upsert(state.tools, callId, (prev) => ({
          ...(prev ?? { callId, tool: data.tool, startedAt: now }),
          tool: data.tool || prev?.tool,
          status: 'running',
          runningAt: now,
        })),
      }

    case 'tool_result': {
      if (!callId) return state
      const error = data.error || data.result?.error
      const cancelled = typeof error === 'string' && /停止生成/.test(error)
      return {
        ...state,
        tools: upsert(state.tools, callId, (prev) => ({
          ...(prev ?? { callId, tool: data.tool, startedAt: now }),
          status: data.success ? 'success' : cancelled ? 'cancelled' : 'error',
          durationMs: typeof data.duration_ms === 'number' ? data.duration_ms : undefined,
          error: data.success ? undefined : error ? String(error).slice(0, 200) : '未返回具体错误',
          progress: undefined,
          opId: data.success && typeof data.result?.op_id === 'string' ? data.result.op_id : undefined,
          summary: data.success && typeof data.result?.summary === 'string' ? data.result.summary : undefined,
        })),
      }
    }

    case 'state_update':
    case 'slide_update':
    case 'project_update': {
      if (!callId) return state
      const text = progressText(data)
      if (!text) return state
      return {
        ...state,
        tools: state.tools.map((item) =>
          item.callId === callId && (item.status === 'running' || item.status === 'pending') ? { ...item, progress: text } : item
        ),
      }
    }

    case 'tool_approval':
      if (!callId) return state
      return {
        ...state,
        tools: upsert(state.tools, callId, (prev) => ({
          ...(prev ?? { callId, tool: data.tool, status: 'running', startedAt: now }),
          approval: {
            id: String(data.approval_id),
            source: data.source || '',
            label: data.label || data.tool,
            reason: data.reason || '',
            arguments: data.arguments,
            expiresAt: Date.parse(data.expires_at) || now + 600_000,
            status: 'pending',
          },
        })),
      }

    case 'tool_approval_resolved':
      if (!callId) return state
      return {
        ...state,
        tools: state.tools.map((item) =>
          item.callId === callId && item.approval ? { ...item, approval: { ...item.approval, status: data.decision || 'rejected' } } : item
        ),
      }

    case 'usage':
      return { ...state, usage: { prompt_tokens: data.prompt_tokens || 0, completion_tokens: data.completion_tokens || 0 } }

    case 'done':
    case 'error': {
      const cancelled = event === 'done' && Boolean(data?.cancelled)
      return {
        ...state,
        cancelled,
        usage: data?.usage && typeof data.usage === 'object' ? data.usage : state.usage,
        tools: state.tools.map((item): ToolRun => {
          const next: ToolRun =
            item.status === 'running' || item.status === 'pending'
              ? { ...item, status: cancelled ? 'cancelled' : event === 'error' ? 'error' : item.status }
              : item
          // 运行结束时还没处理的确认一并失效
          return next.approval?.status === 'pending' ? { ...next, approval: { ...next.approval, status: 'cancelled' } } : next
        }),
      }
    }

    default:
      return state
  }
}

/** 同时处于执行中的最大工具数，用于判断是否并行展示 */
export function peakConcurrency(tools: ToolRun[]): number {
  const points: Array<[number, number]> = []
  for (const item of tools) {
    if (item.runningAt === undefined) continue
    const end = item.durationMs !== undefined ? item.runningAt + item.durationMs : Number.POSITIVE_INFINITY
    points.push([item.runningAt, 1], [end, -1])
  }
  points.sort((a, b) => a[0] - b[0] || a[1] - b[1])
  let current = 0
  let peak = 0
  for (const [, delta] of points) {
    current += delta
    peak = Math.max(peak, current)
  }
  return peak
}
