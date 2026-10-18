import { useEffect, useState } from 'react'
import { Loader2, ShieldAlert } from 'lucide-react'
import { tasksApi } from '@/api'
import type { AgentRunState, ToolRun } from '@/lib/agent-run'

function formatArgs(args: unknown): string {
  if (args === undefined || args === null) return ''
  try {
    const text = JSON.stringify(args, null, 2)
    return text === '{}' ? '' : text
  } catch {
    return String(args)
  }
}

function ApprovalCard({ run }: { run: ToolRun }) {
  const approval = run.approval!
  const [busy, setBusy] = useState<string | null>(null)
  const [error, setError] = useState('')
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000)
    return () => window.clearInterval(timer)
  }, [])
  const left = Math.max(0, Math.round((approval.expiresAt - now) / 1000))
  const args = formatArgs(approval.arguments)

  const decide = async (decision: 'approve' | 'reject', remember = false) => {
    setBusy(remember ? 'remember' : decision)
    setError('')
    try {
      await tasksApi.approve(approval.id, decision, remember)
    } catch (err: any) {
      setError(err?.response?.data?.detail?.replace(/^参数错误:\s*/, '') || '操作失败')
      setBusy(null)
    }
  }

  return (
    <div className="rounded-xl border border-amber-200 bg-amber-50/80 px-3 py-2.5" role="alertdialog" aria-label="需要确认的操作">
      <div className="flex items-start gap-2">
        <ShieldAlert className="mt-0.5 h-4 w-4 shrink-0 text-amber-600" />
        <div className="min-w-0 flex-1">
          <div className="text-xs font-semibold text-surface-900">
            需要你确认：{approval.source} · {approval.label}
          </div>
          <div className="mt-0.5 text-[11px] text-amber-800">
            {approval.reason}，{Math.floor(left / 60)}:{String(left % 60).padStart(2, '0')} 内没有处理会自动拒绝
          </div>
          {args && (
            <pre className="mt-1.5 max-h-40 overflow-auto rounded-lg bg-white/80 px-2 py-1.5 text-[11px] leading-relaxed text-surface-700">{args}</pre>
          )}
          {error && <div className="mt-1 text-[11px] text-red-600">{error}</div>}
          <div className="mt-2 flex flex-wrap gap-1.5">
            <button
              type="button"
              disabled={!!busy}
              onClick={() => decide('approve')}
              className="inline-flex items-center gap-1 rounded-lg bg-surface-950 px-3 py-1 text-xs font-medium text-white hover:bg-surface-800 disabled:opacity-50"
            >
              {busy === 'approve' && <Loader2 className="h-3 w-3 animate-spin" />}允许
            </button>
            <button
              type="button"
              disabled={!!busy}
              onClick={() => decide('approve', true)}
              className="inline-flex items-center gap-1 rounded-lg border border-black/10 bg-white px-3 py-1 text-xs text-surface-700 hover:bg-surface-50 disabled:opacity-50"
            >
              {busy === 'remember' && <Loader2 className="h-3 w-3 animate-spin" />}本次对话都允许
            </button>
            <button
              type="button"
              disabled={!!busy}
              onClick={() => decide('reject')}
              className="inline-flex items-center gap-1 rounded-lg px-3 py-1 text-xs text-red-600 hover:bg-red-50 disabled:opacity-50"
            >
              {busy === 'reject' && <Loader2 className="h-3 w-3 animate-spin" />}拒绝
            </button>
          </div>
        </div>
      </div>
    </div>
  )
}

/** 运行中等待确认的工具调用（过程面板折叠时也显示） */
export function ApprovalPrompt({ run }: { run: AgentRunState }) {
  const pending = run.tools.filter((item) => item.approval?.status === 'pending')
  if (pending.length === 0) return null
  return (
    <div className="space-y-2 px-3 py-2">
      {pending.map((item) => (
        <ApprovalCard key={item.approval!.id} run={item} />
      ))}
    </div>
  )
}
