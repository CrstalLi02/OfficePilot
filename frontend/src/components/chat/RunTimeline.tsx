import { useEffect, useState } from 'react'
import { AlarmClock, AlertCircle, Ban, Puzzle, Check, Clapperboard, FileEdit, FilePen, FileSearch, FolderOpen, FolderTree, Globe, Image as ImageIcon, LayoutDashboard, Loader2, PenTool, PieChart, Save, Sheet, Table2, Database, BookOpen, Undo2, Wrench, Repeat, ShieldCheck, TextSelect, PenLine } from 'lucide-react'
import { workspaceApi } from '@/api'
import { peakConcurrency, type AgentRunState, type ToolRun } from '@/lib/agent-run'

/** MCP 插件工具：mcp__标识__工具名 → 「标识 · 工具名」 */
export function mcpMeta(tool: string): { label: string; icon: typeof Wrench } | null {
  const m = /^mcp__([a-z0-9_]+?)__(.+)$/.exec(tool)
  return m ? { label: `插件 ${m[1]} · ${m[2]}`, icon: Puzzle } : null
}

export const TOOL_META: Record<string, { label: string; icon: typeof Wrench }> = {
  schedule_create: { label: '创建定时任务', icon: AlarmClock },
  schedule_list: { label: '查看定时任务', icon: AlarmClock },
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
  artifact_edit: { label: '局部修改产物', icon: FilePen },
  use_skill: { label: '使用技能', icon: BookOpen },
  read_skill_file: { label: '读取技能资料', icon: BookOpen },
  data_profile: { label: '查看数据结构', icon: Table2 },
  data_query: { label: '数据查询', icon: Database },
  workspace_list: { label: '查看文件', icon: FolderOpen },
  workspace_search: { label: '搜索文件', icon: FileSearch },
  workspace_read: { label: '读取文件', icon: FileSearch },
  workspace_write: { label: '保存到文件', icon: Save },
  workspace_organize: { label: '整理文件', icon: FolderTree },
  docs_read: { label: '读取 Word', icon: FileSearch },
  docs_apply: { label: '修改 Word', icon: FilePen },
  sheet_read: { label: '读取 Excel', icon: Table2 },
  sheet_apply: { label: '修改 Excel', icon: Sheet },
  slides_read: { label: '读取 PPT', icon: FileSearch },
  slides_apply: { label: '修改 PPT', icon: LayoutDashboard },
  office_convert: { label: '转换格式', icon: Repeat },
  office_merge: { label: '填写模板', icon: FilePen },
  office_check: { label: '文档体检', icon: ShieldCheck },
  office_render: { label: '渲染页面', icon: ImageIcon },
  editor_selection: { label: '读取编辑器选区', icon: TextSelect },
  editor_open: { label: '在编辑器中打开', icon: PenLine },
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

/** 工作区写入 / 整理的撤销按钮 */
function UndoButton({ opId }: { opId: string }) {
  const [state, setState] = useState<'idle' | 'busy' | 'done' | 'error'>('idle')
  const [note, setNote] = useState('')
  const undo = async () => {
    setState('busy')
    try {
      const { data } = await workspaceApi.undo(opId)
      setState('done')
      setNote(data.skipped.length ? `已撤销，${data.skipped.length} 项已变动未恢复` : '已撤销')
    } catch (err: any) {
      setState('error')
      setNote(err?.response?.data?.detail || '撤销失败')
    }
  }
  if (state === 'done' || state === 'error') {
    return <span className={`text-[11px] ${state === 'error' ? 'text-red-600' : 'text-surface-400'}`}>{note}</span>
  }
  return (
    <button
      type="button"
      onClick={undo}
      disabled={state === 'busy'}
      className="inline-flex items-center gap-1 rounded-md px-1.5 py-0.5 text-[11px] text-surface-500 hover:bg-surface-100 hover:text-surface-900 disabled:opacity-50"
    >
      {state === 'busy' ? <Loader2 className="h-3 w-3 animate-spin" /> : <Undo2 className="h-3 w-3" />}
      撤销
    </button>
  )
}

function ToolCard({ run, now }: { run: ToolRun; now: number }) {
  const meta = TOOL_META[run.tool] || mcpMeta(run.tool) || { label: run.tool, icon: Wrench }
  const Icon = meta.icon
  const active = run.status === 'pending' || run.status === 'running'
  const elapsed = run.durationMs ?? (run.runningAt ? now - run.runningAt : undefined)
  const waiting = run.approval?.status === 'pending'
  const detail =
    waiting ? '等待你确认…' :
    run.approval?.status === 'rejected' && run.status === 'error' ? '你拒绝了这次调用' :
    run.status === 'error' ? run.error :
    run.status === 'cancelled' ? '已停止' :
    run.status === 'pending' ? '正在准备参数…' :
    run.status === 'success' && run.summary ? run.summary :
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
      {(detail || run.opId) && (
        <div className="mt-1 flex items-center gap-2">
          {detail && (
            <div className={`min-w-0 flex-1 truncate text-[11px] ${run.status === 'error' ? 'text-red-600' : 'text-surface-500'}`} title={detail}>
              {detail}
            </div>
          )}
          {run.opId && <span className="ml-auto shrink-0"><UndoButton opId={run.opId} /></span>}
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
