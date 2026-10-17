import { useEffect, useState } from 'react'
import { Ban, CheckCircle2, Clock, ListChecks, Loader2, RotateCcw, X, XCircle, AlertTriangle, ClipboardList } from 'lucide-react'
import { tasksApi, type AgentTask } from '@/api'
import { SchedulesPanel } from './SchedulesPanel'

interface TasksDialogProps {
  open: boolean
  onClose: () => void
  /** 打开任务所在的会话（会自动接上仍在运行的任务进度） */
  onOpenSession: (sessionId: string) => void
  onToast?: (message: string, tone?: 'success' | 'error' | 'info') => void
}

const STATUS: Record<string, { label: string; icon: typeof Clock; tone: string }> = {
  queued: { label: '排队中', icon: Clock, tone: 'text-surface-500 bg-surface-100' },
  running: { label: '运行中', icon: Loader2, tone: 'text-blue-700 bg-blue-50' },
  succeeded: { label: '已完成', icon: CheckCircle2, tone: 'text-emerald-700 bg-emerald-50' },
  failed: { label: '失败', icon: XCircle, tone: 'text-red-700 bg-red-50' },
  cancelled: { label: '已停止', icon: Ban, tone: 'text-surface-500 bg-surface-100' },
  awaiting_approval: { label: '待确认计划', icon: ClipboardList, tone: 'text-amber-700 bg-amber-50' },
  interrupted: { label: '已中断', icon: AlertTriangle, tone: 'text-amber-700 bg-amber-50' },
}

const ORIGIN: Record<string, string> = { chat: '对话', schedule: '定时', im: 'IM' }

function duration(task: AgentTask) {
  if (!task.started_at) return ''
  const end = task.finished_at ? new Date(task.finished_at).getTime() : Date.now()
  const seconds = Math.max(0, Math.round((end - new Date(task.started_at).getTime()) / 1000))
  return seconds < 60 ? `${seconds}s` : `${Math.floor(seconds / 60)}m${seconds % 60}s`
}

function when(iso: string) {
  const date = new Date(iso)
  return Number.isNaN(date.getTime()) ? '' : date.toLocaleString('zh-CN', { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit' })
}

/** 后台任务：最近的运行记录，运行中的可停止，失败 / 中断 / 停止的可重试 */
export function TasksDialog({ open, onClose, onOpenSession, onToast }: TasksDialogProps) {
  const [tasks, setTasks] = useState<AgentTask[] | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [tab, setTab] = useState<'runs' | 'schedules'>('runs')

  const reload = () => tasksApi.list().then(({ data }) => setTasks(data.tasks)).catch(() => setTasks([]))

  useEffect(() => {
    if (!open) return
    reload()
    // 有任务在运行时定时刷新状态
    const timer = window.setInterval(reload, 3000)
    return () => window.clearInterval(timer)
  }, [open])

  if (!open) return null

  const act = async (task: AgentTask, action: 'cancel' | 'retry') => {
    setBusy(task.id)
    try {
      if (action === 'cancel') {
        await tasksApi.cancel(task.id)
        onToast?.('已停止任务', 'info')
      } else {
        const { data } = await tasksApi.retry(task.id)
        onToast?.('已重新运行', 'success')
        onClose()
        onOpenSession(data.session_id)
      }
      reload()
    } catch (err: any) {
      onToast?.(err?.response?.data?.detail || '操作失败', 'error')
    } finally {
      setBusy(null)
    }
  }

  return (
    <div className="fixed inset-0 z-[100] flex items-center justify-center bg-black/30 p-3" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div role="dialog" aria-label="后台任务" className="flex max-h-[85vh] w-full max-w-xl flex-col overflow-hidden rounded-2xl bg-white shadow-2xl">
        <div className="flex items-center gap-2 border-b border-surface-100 px-4 py-3">
          <ListChecks className="h-4 w-4 text-surface-600" />
          <div className="min-w-0 flex-1">
            <div className="text-sm font-semibold text-surface-900">后台任务</div>
            <div className="text-[11px] text-surface-400">关掉页面任务也会继续运行；完成后会发通知</div>
          </div>
          <button type="button" onClick={onClose} className="rounded-lg p-1.5 text-surface-500 hover:bg-surface-100" aria-label="关闭">
            <X className="h-4 w-4" />
          </button>
        </div>
        <div role="tablist" className="flex gap-1 border-b border-surface-100 px-3 pt-2">
          {([['runs', '运行记录'], ['schedules', '定时任务']] as const).map(([id, label]) => (
            <button
              key={id}
              role="tab"
              aria-selected={tab === id}
              type="button"
              onClick={() => setTab(id)}
              className={`-mb-px border-b-2 px-2.5 pb-1.5 text-xs font-medium ${tab === id ? 'border-surface-900 text-surface-900' : 'border-transparent text-surface-400 hover:text-surface-700'}`}
            >
              {label}
            </button>
          ))}
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-3 py-2">
          {tab === 'schedules' && (
            <SchedulesPanel
              onOpenSession={(id) => {
                onClose()
                onOpenSession(id)
              }}
              onToast={onToast}
            />
          )}
          {tab === 'runs' && <>
          {tasks === null && (
            <div className="flex items-center gap-2 px-1 py-6 text-xs text-surface-400">
              <Loader2 className="h-3.5 w-3.5 animate-spin" />正在加载…
            </div>
          )}
          {tasks?.length === 0 && <div className="py-8 text-center text-xs text-surface-400">还没有任务</div>}
          {tasks?.map((task) => {
            const meta = STATUS[task.status] || STATUS.queued
            const Icon = meta.icon
            const retryable = !task.live && ['failed', 'interrupted', 'cancelled'].includes(task.status)
            return (
              <div key={task.id} className="mb-1.5 rounded-xl border border-black/[0.05] px-3 py-2">
                <div className="flex items-start gap-2">
                  <button
                    type="button"
                    onClick={() => {
                      onClose()
                      onOpenSession(task.session_id)
                    }}
                    className="min-w-0 flex-1 text-left"
                    title="打开对话"
                  >
                    <div className="truncate text-xs font-medium text-surface-800">{task.title || '未命名任务'}</div>
                    <div className="mt-0.5 flex flex-wrap items-center gap-x-2 text-[11px] text-surface-400">
                      <span>{ORIGIN[task.origin] || task.origin}</span>
                      <span>{when(task.created_at)}</span>
                      {duration(task) && <span>耗时 {duration(task)}</span>}
                    </div>
                    {(task.error || task.summary) && (
                      <div className={`mt-1 line-clamp-2 text-[11px] ${task.error ? 'text-red-600' : 'text-surface-500'}`}>{task.error || task.summary}</div>
                    )}
                  </button>
                  <span className={`inline-flex shrink-0 items-center gap-1 rounded-full px-2 py-0.5 text-[11px] ${meta.tone}`}>
                    <Icon className={`h-3 w-3 ${task.status === 'running' ? 'animate-spin' : ''}`} />
                    {meta.label}
                  </span>
                </div>
                {(task.live || retryable) && (
                  <div className="mt-1.5 flex justify-end gap-1.5">
                    {task.live && (
                      <button type="button" disabled={busy === task.id} onClick={() => act(task, 'cancel')} className="rounded-lg px-2 py-1 text-[11px] text-surface-600 hover:bg-surface-100 disabled:opacity-50">
                        停止
                      </button>
                    )}
                    {retryable && (
                      <button type="button" disabled={busy === task.id} onClick={() => act(task, 'retry')} className="inline-flex items-center gap-1 rounded-lg px-2 py-1 text-[11px] text-surface-700 hover:bg-surface-100 disabled:opacity-50">
                        <RotateCcw className="h-3 w-3" />重试
                      </button>
                    )}
                  </div>
                )}
              </div>
            )
          })}
          </>}
        </div>
      </div>
    </div>
  )
}
