import { useState } from 'react'
import { AlertCircle, Check, ClipboardList, Loader2, Play, RotateCcw, X } from 'lucide-react'
import type { PlanCardState, PlanDecisionKind, PlanStep } from '@/types'
import { TOOL_META } from './RunTimeline'

const STATUS_LABEL: Record<PlanCardState['status'], string> = {
  pending: '等待确认',
  approved: '已确认',
  revised: '已请求修改',
  rejected: '已取消',
}

interface PlanCardProps {
  state: PlanCardState
  /** 正在运行时禁用操作 */
  busy: boolean
  onDecide: (decision: PlanDecisionKind, steps: PlanStep[], feedback: string) => void
}

/** Plan 模式的计划确认卡片：可勾选步骤、补充意见，然后执行 / 修改 / 取消 */
export function PlanCard({ state, busy, onDecide }: PlanCardProps) {
  const { plan, status } = state
  const pending = status === 'pending'
  const [selected, setSelected] = useState<Set<string>>(() => new Set(plan.steps.map((step) => step.id)))
  const [feedback, setFeedback] = useState('')

  const toggle = (id: string) => {
    setSelected((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  const chosenSteps = plan.steps.filter((step) => selected.has(step.id))
  const executedIds = state.approvedStepIds ? new Set(state.approvedStepIds) : null

  return (
    <div className="mt-3 overflow-hidden rounded-2xl border border-black/[0.08] bg-white/90 text-surface-800">
      <div className="flex items-center gap-2 border-b border-black/[0.05] px-3 py-2">
        <ClipboardList className="h-4 w-4 shrink-0 text-surface-600" />
        <span className="min-w-0 flex-1 truncate text-xs font-semibold">{plan.goal || '执行计划'}</span>
        <span className={`shrink-0 rounded-full px-2 py-0.5 text-[11px] ${pending ? 'bg-amber-50 text-amber-700' : 'bg-surface-100 text-surface-500'}`}>
          {STATUS_LABEL[status]}
        </span>
      </div>

      <ol className="space-y-1 px-3 py-2">
        {plan.steps.map((step, index) => {
          const meta = TOOL_META[step.tool]
          const Icon = meta?.icon
          const skipped = executedIds !== null && !executedIds.has(step.id)
          const progress = state.stepStatus?.[step.id]
          const running = status === 'approved' && !skipped && !progress && busy
          return (
            <li key={step.id} className={`flex items-start gap-2 rounded-lg px-1.5 py-1 text-xs ${skipped ? 'opacity-45' : ''}`}>
              {pending ? (
                <input
                  type="checkbox"
                  className="mt-0.5 h-3.5 w-3.5 shrink-0 accent-surface-900"
                  checked={selected.has(step.id)}
                  onChange={() => toggle(step.id)}
                  disabled={busy}
                  aria-label={`执行第 ${index + 1} 步`}
                />
              ) : (
                <span className="mt-0.5 flex h-3.5 w-3.5 shrink-0 items-center justify-center">
                  {progress === 'done' ? <Check className="h-3.5 w-3.5 text-emerald-600" /> :
                   progress === 'error' ? <AlertCircle className="h-3.5 w-3.5 text-red-500" /> :
                   running ? <Loader2 className="h-3.5 w-3.5 animate-spin text-surface-400" /> :
                   <span className="text-[11px] text-surface-400">{index + 1}</span>}
                </span>
              )}
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-1.5">
                  {Icon && <Icon className="h-3.5 w-3.5 shrink-0 text-surface-500" />}
                  <span className={`font-medium ${skipped ? 'line-through' : ''}`}>{step.purpose || meta?.label || step.tool}</span>
                </div>
                <div className="mt-0.5 truncate text-[11px] text-surface-400">
                  {meta?.label || step.tool}
                  {step.deliverable ? ` · ${step.deliverable}` : ''}
                </div>
              </div>
            </li>
          )
        })}
      </ol>

      {plan.notes && <div className="border-t border-black/[0.04] px-3 py-2 text-[11px] text-surface-500">{plan.notes}</div>}

      {pending && (
        <div className="space-y-2 border-t border-black/[0.05] px-3 py-2">
          <textarea
            value={feedback}
            onChange={(e) => setFeedback(e.target.value)}
            disabled={busy}
            rows={2}
            placeholder="补充意见（可选），例如：语气更正式、PPT 控制在 8 页"
            className="w-full resize-none rounded-lg border border-black/[0.08] bg-white px-2 py-1.5 text-xs outline-none focus:border-surface-400"
          />
          <div className="flex flex-wrap items-center gap-1.5">
            <button
              type="button"
              disabled={busy || chosenSteps.length === 0}
              onClick={() => onDecide('approve', chosenSteps, feedback)}
              className="inline-flex items-center gap-1 rounded-lg bg-surface-950 px-3 py-1.5 text-xs font-medium text-white disabled:opacity-40"
            >
              <Play className="h-3 w-3" />
              执行{chosenSteps.length < plan.steps.length ? ` ${chosenSteps.length} 步` : '计划'}
            </button>
            <button
              type="button"
              disabled={busy || !feedback.trim()}
              onClick={() => onDecide('revise', [], feedback)}
              title={feedback.trim() ? '按补充意见重新规划' : '先填写补充意见'}
              className="inline-flex items-center gap-1 rounded-lg border border-black/[0.1] px-3 py-1.5 text-xs text-surface-700 disabled:opacity-40"
            >
              <RotateCcw className="h-3 w-3" />
              按意见修改
            </button>
            <button
              type="button"
              disabled={busy}
              onClick={() => onDecide('reject', [], '')}
              className="ml-auto inline-flex items-center gap-1 rounded-lg px-2 py-1.5 text-xs text-surface-500 hover:text-surface-800 disabled:opacity-40"
            >
              <X className="h-3 w-3" />
              取消
            </button>
          </div>
        </div>
      )}
    </div>
  )
}
