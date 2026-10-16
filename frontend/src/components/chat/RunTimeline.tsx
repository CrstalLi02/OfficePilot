import { useEffect, useState } from 'react'
import { AlertCircle, Ban, Check, Clapperboard, FileEdit, Globe, Image as ImageIcon, LayoutDashboard, Loader2, PenTool, PieChart, Sheet, Wrench } from 'lucide-react'
import { peakConcurrency, type AgentRunState, type ToolRun } from '@/lib/agent-run'

const TOOL_META: Record<string, { label: string; icon: typeof Wrench }> = {
  ppt_plan: { label: '规划 PPT 大纲', icon: LayoutDashboard },
  ppt_generate: { label: '生成 PPT', icon: LayoutDashboard },
  doc_generate: { label: '生成 Word 文档', icon: FileEdit },
  md_generate: { label: '生成 Markdown', icon: FileEdit },
  sheet_generate: { label: '生成表格', icon: Sheet },
  chart_generate: { label: '生成图表', icon: PieChart },
  drawio_generate: { label: '绘制流程图', icon: PenTool },
  image_prompt: { label: '生成图片', icon: ImageIcon },
  video_generate: { label: '生成视频', icon: Clapperboard },
  video_batch_generate: { label: '批量生成视频', icon: Clapperboard },
  video_storyboard: { label: '规划视频分镜', icon: Clapperboard },
  web_search: { label: '联网搜索', icon: Globe },
}

function formatDuration(ms: number) {
  if (ms < 1000) return `${ms}ms`
  const seconds = ms / 1000
  if (seconds < 60) return `${seconds.toFixed(seconds < 10 ? 1 : 0)}s`
  return `${Math.floor(seconds / 60)}m${Math.round(seconds % 60)}s`
}

function formatTokens(n: number) {
  return n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n)
}

function StatusIcon({ status }: { status: ToolRun['status'] }) {
  switch (status) {
    case 'success':
      return <Check className="h-3.5 w-3.5 text-emerald-600" />
    case 'error':
      return <AlertCircle className="h-3.5 w-3.5 text-red-500" />
    case 'cancelled':
      return <Ban className="h-3.5 w-3.5 text-surface-400" />
    default:
      return <Loader2 className="h-3.5 w-3.5 animate-spin text-surface-500" />
  }
}

function ToolCard({ run, now }: { run: ToolRun; now: number }) {
  const meta = TOOL_META[run.tool] || { label: run.tool, icon: Wrench }
  const Icon = meta.icon
  const active = run.status === 'pending' || run.status === 'running'
  const elapsed = run.durationMs ?? (run.runningAt ? now - run.runningAt : undefined)
  const detail =
    run.status === 'error' ? run.error :
    run.status === 'cancelled' ? '已停止' :
    run.status === 'pending' ? '正在准备参数…' :
    run.progress

  return (
    <div
      className={`min-w-0 rounded-xl border px-2.5 py-2 transition-colors ${
        active ? 'border-surface-300 bg-white' :
        run.status === 'error' ? 'border-red-200 bg-red-50/60' :
        'border-black/[0.05] bg-white/50'
      }`}
    >
      <div className="flex items-center gap-1.5 text-xs font-medium text-surface-800">
        <Icon className="h-3.5 w-3.5 shrink-0 text-surface-500" />
        <span className="truncate">{meta.label}</span>
        <span className="ml-auto flex shrink-0 items-center gap-1 text-[11px] font-normal text-surface-400">
          {elapsed !== undefined && formatDuration(Math.max(0, elapsed))}
          <StatusIcon status={run.status} />
        </span>
      </div>
      {detail && (
        <div className={`mt-1 truncate text-[11px] ${run.status === 'error' ? 'text-red-600' : 'text-surface-500'}`} title={detail}>
          {detail}
        </div>
      )}
    </div>
  )
}

/** 工具调用卡片：每次调用一张，并发执行的工具并排显示 */
export function RunTimeline({ run, isStreaming }: { run: AgentRunState; isStreaming: boolean }) {
  const [now, setNow] = useState(() => Date.now())
  const hasActive = run.tools.some((item) => item.status === 'pending' || item.status === 'running')

  useEffect(() => {
    if (!isStreaming || !hasActive) return
    const timer = window.setInterval(() => setNow(Date.now()), 500)
    return () => window.clearInterval(timer)
  }, [isStreaming, hasActive])

  if (run.tools.length === 0 && !run.usage) return null

  const parallel = peakConcurrency(run.tools) > 1
  const tokens = run.usage ? run.usage.prompt_tokens + run.usage.completion_tokens : 0

  return (
    <div className="border-b border-black/[0.04] px-3 py-2">
      {run.tools.length > 0 && (
        <>
          <div className="mb-1.5 flex items-center gap-2 text-[11px] text-surface-400">
            <span>工具调用 {run.tools.length}</span>
            {parallel && <span className="rounded-full bg-surface-100 px-1.5 py-0.5 text-surface-600">并行执行</span>}
            {run.cancelled && <span className="text-surface-500">已停止</span>}
            {tokens > 0 && <span className="ml-auto">{formatTokens(tokens)} tokens</span>}
          </div>
          <div className="grid grid-cols-1 gap-1.5 sm:grid-cols-2">
            {run.tools.map((item) => (
              <ToolCard key={item.callId} run={item} now={now} />
            ))}
          </div>
        </>
      )}
      {run.tools.length === 0 && tokens > 0 && (
        <div className="text-right text-[11px] text-surface-400">{formatTokens(tokens)} tokens</div>
      )}
    </div>
  )
}
