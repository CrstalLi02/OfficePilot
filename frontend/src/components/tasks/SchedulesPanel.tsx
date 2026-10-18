import { useEffect, useMemo, useState } from 'react'
import { AlarmClock, Loader2, Pencil, Play, Plus, Trash2 } from 'lucide-react'
import { browserTimezone, imApi, schedulesApi, type ImTarget, type Schedule, type ScheduleInput } from '@/api'

interface SchedulesPanelProps {
  onOpenSession: (sessionId: string) => void
  onToast?: (message: string, tone?: 'success' | 'error' | 'info') => void
}

type Frequency = 'daily' | 'weekdays' | 'weekly' | 'monthly' | 'hourly' | 'custom'

interface Draft {
  id?: string
  name: string
  prompt: string
  frequency: Frequency
  time: string
  weekday: string
  monthday: string
  hours: string
  cron: string
  mode: 'craft' | 'ask'
  session_strategy: 'new' | 'same'
  enabled: boolean
  /** 推送目标 "channel_id|chat_id"，空表示不推送 */
  notify: string
  /** 编辑已有任务时保留它的时区与技能；新建时用浏览器时区 */
  timezone?: string
  skills?: string[]
}

const targetKey = (t?: ImTarget | null) => (t ? `${t.channel_id}|${t.chat_id}` : '')
const parseTarget = (key: string): ImTarget | null => {
  const [channel_id, chat_id] = key.split('|')
  return channel_id && chat_id ? { channel_id, chat_id } : null
}

const WEEKDAYS = [
  ['1', '周一'],
  ['2', '周二'],
  ['3', '周三'],
  ['4', '周四'],
  ['5', '周五'],
  ['6', '周六'],
  ['0', '周日'],
]

const EMPTY: Draft = {
  name: '',
  prompt: '',
  frequency: 'weekly',
  time: '17:00',
  weekday: '5',
  monthday: '1',
  hours: '4',
  cron: '0 9 * * 1-5',
  mode: 'craft',
  session_strategy: 'new',
  enabled: true,
  notify: '',
}

/** 频率选项 → 5 段 cron */
function toCron(d: Draft): string {
  const [h, m] = d.time.split(':').map((x) => String(Number(x) || 0))
  switch (d.frequency) {
    case 'daily':
      return `${m} ${h} * * *`
    case 'weekdays':
      return `${m} ${h} * * 1-5`
    case 'weekly':
      return `${m} ${h} * * ${d.weekday}`
    case 'monthly':
      return `${m} ${h} ${d.monthday} * *`
    case 'hourly':
      return `0 */${d.hours} * * *`
    default:
      return d.cron.trim()
  }
}

/** 已有定时任务 → 表单（常见写法还原成频率选项，其余显示为自定义 cron） */
function fromSchedule(s: Schedule): Draft {
  const base: Draft = { ...EMPTY, id: s.id, name: s.name, prompt: s.prompt, cron: s.cron, mode: s.mode === 'ask' ? 'ask' : 'craft', session_strategy: s.session_strategy, enabled: s.enabled, notify: targetKey(s.notify), timezone: s.timezone, skills: s.skills, frequency: 'custom' }
  const [min, hour, dom, mon, dow] = s.cron.split(/\s+/)
  const isNum = (x?: string) => !!x && /^\d+$/.test(x)
  if (mon !== '*') return base
  const time = isNum(min) && isNum(hour) ? `${hour.padStart(2, '0')}:${min.padStart(2, '0')}` : base.time
  if (isNum(min) && isNum(hour) && dom === '*' && dow === '*') return { ...base, frequency: 'daily', time }
  if (isNum(min) && isNum(hour) && dom === '*' && dow === '1-5') return { ...base, frequency: 'weekdays', time }
  if (isNum(min) && isNum(hour) && dom === '*' && /^[0-6]$/.test(dow)) return { ...base, frequency: 'weekly', time, weekday: dow }
  if (isNum(min) && isNum(hour) && isNum(dom) && dow === '*') return { ...base, frequency: 'monthly', time, monthday: dom }
  if (min === '0' && hour?.startsWith('*/') && dom === '*' && dow === '*') return { ...base, frequency: 'hourly', hours: hour.slice(2) }
  return base
}

function when(iso?: string | null) {
  if (!iso) return '—'
  const d = new Date(iso)
  return Number.isNaN(d.getTime()) ? '—' : d.toLocaleString('zh-CN', { month: 'numeric', day: 'numeric', weekday: 'short', hour: '2-digit', minute: '2-digit' })
}

function errorText(err: any) {
  return err?.response?.data?.detail || err?.message || '操作失败'
}

export function SchedulesPanel({ onOpenSession, onToast }: SchedulesPanelProps) {
  const [schedules, setSchedules] = useState<Schedule[] | null>(null)
  const [draft, setDraft] = useState<Draft | null>(null)
  const [preview, setPreview] = useState<{ description: string; next_runs: string[] } | null>(null)
  const [previewError, setPreviewError] = useState('')
  const [busy, setBusy] = useState<string | null>(null)
  const [targets, setTargets] = useState<Array<ImTarget & { label: string }>>([])
  const timezone = useMemo(() => browserTimezone(), [])
  const draftTimezone = draft?.timezone || timezone

  const reload = () => schedulesApi.list().then(({ data }) => setSchedules(data.schedules)).catch(() => setSchedules([]))
  useEffect(() => {
    reload()
    imApi.targets().then(({ data }) => setTargets(data.targets)).catch(() => setTargets([]))
  }, [])

  // 表单变化时实时预览接下来的触发时间
  const cron = draft ? toCron(draft) : ''
  useEffect(() => {
    if (!draft) return
    const timer = window.setTimeout(() => {
      schedulesApi
        .preview(cron, draftTimezone)
        .then(({ data }) => {
          setPreview(data)
          setPreviewError('')
        })
        .catch((err) => {
          setPreview(null)
          setPreviewError(errorText(err))
        })
    }, 300)
    return () => window.clearTimeout(timer)
  }, [cron, draft !== null, draftTimezone])

  const save = async () => {
    if (!draft) return
    const input: ScheduleInput = { name: draft.name, prompt: draft.prompt, cron, timezone: draftTimezone, skills: draft.skills, mode: draft.mode, session_strategy: draft.session_strategy, enabled: draft.enabled, notify: parseTarget(draft.notify) }
    setBusy('save')
    try {
      if (draft.id) await schedulesApi.update(draft.id, input)
      else await schedulesApi.create(input)
      onToast?.(draft.id ? '已保存' : '已创建定时任务', 'success')
      setDraft(null)
      reload()
    } catch (err) {
      onToast?.(errorText(err), 'error')
    } finally {
      setBusy(null)
    }
  }

  const toggle = async (s: Schedule) => {
    setBusy(s.id)
    try {
      await schedulesApi.update(s.id, { name: s.name, prompt: s.prompt, cron: s.cron, timezone: s.timezone, mode: s.mode, skills: s.skills, session_strategy: s.session_strategy, enabled: !s.enabled, notify: s.notify ? { channel_id: s.notify.channel_id, chat_id: s.notify.chat_id } : null })
      reload()
    } catch (err) {
      onToast?.(errorText(err), 'error')
    } finally {
      setBusy(null)
    }
  }

  const runNow = async (s: Schedule) => {
    setBusy(s.id)
    try {
      const { data } = await schedulesApi.runNow(s.id)
      if (data.status === 'skipped') onToast?.(data.message || '上一次运行还没结束', 'info')
      else {
        onToast?.('已开始运行', 'success')
        if (data.session_id) onOpenSession(data.session_id)
      }
      reload()
    } catch (err) {
      onToast?.(errorText(err), 'error')
    } finally {
      setBusy(null)
    }
  }

  const remove = async (s: Schedule) => {
    if (!window.confirm(`删除定时任务《${s.name}》？`)) return
    try {
      await schedulesApi.remove(s.id)
      reload()
    } catch (err) {
      onToast?.(errorText(err), 'error')
    }
  }

  if (draft) {
    const set = (patch: Partial<Draft>) => setDraft({ ...draft, ...patch })
    const field = 'w-full rounded-lg border border-black/[0.08] bg-white px-2 py-1.5 text-xs outline-none focus:border-surface-400'
    return (
      <div className="space-y-2.5 px-1 py-2">
        <label className="block text-[11px] text-surface-500">
          名称
          <input className={`${field} mt-1`} value={draft.name} onChange={(e) => set({ name: e.target.value })} placeholder="例如：每周周报" />
        </label>
        <label className="block text-[11px] text-surface-500">
          每次要做什么（运行时不会带上当前对话，请写完整）
          <textarea
            className={`${field} mt-1`}
            rows={4}
            value={draft.prompt}
            onChange={(e) => set({ prompt: e.target.value })}
            placeholder="例如：根据工作区里本周的会议纪要和待办，按周报技能写本周周报，保存到 /周报 目录"
          />
        </label>
        <div className="flex flex-wrap items-end gap-2 text-[11px] text-surface-500">
          <label>
            频率
            <select className={`${field} mt-1`} value={draft.frequency} onChange={(e) => set({ frequency: e.target.value as Frequency })}>
              <option value="daily">每天</option>
              <option value="weekdays">每个工作日</option>
              <option value="weekly">每周</option>
              <option value="monthly">每月</option>
              <option value="hourly">每隔几小时</option>
              <option value="custom">自定义 cron</option>
            </select>
          </label>
          {draft.frequency === 'weekly' && (
            <label>
              星期
              <select className={`${field} mt-1`} value={draft.weekday} onChange={(e) => set({ weekday: e.target.value })}>
                {WEEKDAYS.map(([v, l]) => (
                  <option key={v} value={v}>{l}</option>
                ))}
              </select>
            </label>
          )}
          {draft.frequency === 'monthly' && (
            <label>
              日期
              <select className={`${field} mt-1`} value={draft.monthday} onChange={(e) => set({ monthday: e.target.value })}>
                {Array.from({ length: 28 }, (_, i) => String(i + 1)).map((d) => (
                  <option key={d} value={d}>{d} 日</option>
                ))}
              </select>
            </label>
          )}
          {draft.frequency === 'hourly' && (
            <label>
              间隔
              <select className={`${field} mt-1`} value={draft.hours} onChange={(e) => set({ hours: e.target.value })}>
                {['1', '2', '3', '4', '6', '8', '12'].map((h) => (
                  <option key={h} value={h}>每 {h} 小时</option>
                ))}
              </select>
            </label>
          )}
          {!['hourly', 'custom'].includes(draft.frequency) && (
            <label>
              时间
              <input type="time" className={`${field} mt-1`} value={draft.time} onChange={(e) => set({ time: e.target.value })} />
            </label>
          )}
          {draft.frequency === 'custom' && (
            <label className="min-w-[160px] flex-1">
              cron（分 时 日 月 周）
              <input className={`${field} mt-1 font-mono`} value={draft.cron} onChange={(e) => set({ cron: e.target.value })} />
            </label>
          )}
        </div>
        <div className={`rounded-lg px-2 py-1.5 text-[11px] ${previewError ? 'bg-red-50 text-red-600' : 'bg-surface-50 text-surface-600'}`}>
          {previewError || (preview ? `${preview.description}（${draftTimezone || '默认时区'}）· 接下来：${preview.next_runs.slice(0, 3).join('、')}` : '正在计算…')}
        </div>
        <div className="flex flex-wrap gap-3 text-[11px] text-surface-600">
          <label className="flex items-center gap-1">
            模式
            <select className="rounded border border-black/[0.08] px-1 py-0.5" value={draft.mode} onChange={(e) => set({ mode: e.target.value as Draft['mode'] })}>
              <option value="craft">执行（生成产物）</option>
              <option value="ask">问答（只回答）</option>
            </select>
          </label>
          <label className="flex items-center gap-1">
            会话
            <select className="rounded border border-black/[0.08] px-1 py-0.5" value={draft.session_strategy} onChange={(e) => set({ session_strategy: e.target.value as Draft['session_strategy'] })}>
              <option value="new">每次新建</option>
              <option value="same">写入同一个会话</option>
            </select>
          </label>
          {(targets.length > 0 || draft.notify) && (
            <label className="flex items-center gap-1">
              完成后推送到
              <select className="max-w-[12rem] rounded border border-black/[0.08] px-1 py-0.5" value={draft.notify} onChange={(e) => set({ notify: e.target.value })}>
                <option value="">不推送</option>
                {targets.map((t) => (
                  <option key={targetKey(t)} value={targetKey(t)}>{t.label}</option>
                ))}
              </select>
            </label>
          )}
          <label className="flex items-center gap-1">
            <input type="checkbox" className="accent-surface-900" checked={draft.enabled} onChange={(e) => set({ enabled: e.target.checked })} />
            启用
          </label>
        </div>
        <div className="flex gap-2 pt-1">
          <button type="button" disabled={busy === 'save' || !!previewError || !draft.name.trim() || !draft.prompt.trim()} onClick={save} className="inline-flex items-center gap-1 rounded-lg bg-surface-950 px-3 py-1.5 text-xs font-medium text-white disabled:opacity-40">
            {busy === 'save' && <Loader2 className="h-3 w-3 animate-spin" />}
            {draft.id ? '保存' : '创建'}
          </button>
          <button type="button" onClick={() => setDraft(null)} className="rounded-lg px-3 py-1.5 text-xs text-surface-500 hover:text-surface-800">
            取消
          </button>
        </div>
      </div>
    )
  }

  return (
    <div className="py-1">
      <div className="mb-2 flex items-center justify-between px-1">
        <div className="text-[11px] text-surface-400">到点自动运行，结果在“运行记录”和通知里查看；也可以直接在对话里说“每周五下午五点帮我……”</div>
        <button type="button" onClick={() => setDraft({ ...EMPTY })} className="inline-flex shrink-0 items-center gap-1 rounded-lg bg-surface-950 px-2.5 py-1 text-[11px] font-medium text-white">
          <Plus className="h-3 w-3" />新建
        </button>
      </div>
      {schedules === null && (
        <div className="flex items-center gap-2 px-1 py-6 text-xs text-surface-400">
          <Loader2 className="h-3.5 w-3.5 animate-spin" />正在加载…
        </div>
      )}
      {schedules?.length === 0 && <div className="py-8 text-center text-xs text-surface-400">还没有定时任务</div>}
      {schedules?.map((s) => (
        <div key={s.id} className={`mb-1.5 rounded-xl border border-black/[0.05] px-3 py-2 ${s.enabled ? '' : 'opacity-60'}`}>
          <div className="flex items-start gap-2">
            <AlarmClock className="mt-0.5 h-3.5 w-3.5 shrink-0 text-surface-500" />
            <div className="min-w-0 flex-1">
              <div className="truncate text-xs font-medium text-surface-800">{s.name}</div>
              <div className="mt-0.5 text-[11px] text-surface-500">
                {s.description}{s.timezone && s.timezone !== timezone ? `（${s.timezone}）` : ''} · {s.mode === 'ask' ? '问答' : '执行'} · {s.session_strategy === 'same' ? '同一会话' : '每次新会话'}
                {s.notify && ` · 推送到 ${s.notify.label}`}
              </div>
              <div className="mt-0.5 text-[11px] text-surface-400">
                下次：{s.enabled ? when(s.next_run_at) : '已停用'} · 上次：{when(s.last_run_at)}
              </div>
              <div className="mt-1 line-clamp-2 text-[11px] text-surface-500">{s.prompt}</div>
            </div>
            <label className="flex shrink-0 items-center gap-1 text-[11px] text-surface-500">
              <input type="checkbox" className="accent-surface-900" checked={s.enabled} disabled={busy === s.id} onChange={() => toggle(s)} />
              启用
            </label>
          </div>
          <div className="mt-1.5 flex justify-end gap-1">
            <button type="button" disabled={busy === s.id} onClick={() => runNow(s)} className="inline-flex items-center gap-1 rounded-lg px-2 py-1 text-[11px] text-surface-700 hover:bg-surface-100 disabled:opacity-50">
              <Play className="h-3 w-3" />立即运行
            </button>
            <button type="button" onClick={() => setDraft(fromSchedule(s))} className="inline-flex items-center gap-1 rounded-lg px-2 py-1 text-[11px] text-surface-700 hover:bg-surface-100">
              <Pencil className="h-3 w-3" />编辑
            </button>
            <button type="button" onClick={() => remove(s)} className="rounded-lg p-1 text-surface-400 hover:bg-red-50 hover:text-red-600" aria-label={`删除 ${s.name}`}>
              <Trash2 className="h-3.5 w-3.5" />
            </button>
          </div>
        </div>
      ))}
    </div>
  )
}
