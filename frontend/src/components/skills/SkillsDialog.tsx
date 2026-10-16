import { useEffect, useMemo, useRef, useState } from 'react'
import { BookOpen, ChevronDown, Loader2, Sparkles, Trash2, Upload, X } from 'lucide-react'
import { skillsApi } from '@/api'
import type { SkillSummary } from '@/types'

interface SkillsDialogProps {
  open: boolean
  onClose: () => void
  /** 当前会话 id；有值时可以"从当前对话生成技能" */
  sessionId: string | null
  onToast?: (message: string, tone?: 'success' | 'error' | 'info') => void
}

function errorText(err: unknown) {
  const anyErr = err as { response?: { data?: { detail?: string } }; message?: string }
  return anyErr?.response?.data?.detail || anyErr?.message || '操作失败'
}

/** 技能管理：内置 / 我的技能，启用开关、查看源码、导入、删除、从对话生成 */
export function SkillsDialog({ open, onClose, sessionId, onToast }: SkillsDialogProps) {
  const [skills, setSkills] = useState<SkillSummary[] | null>(null)
  const [expanded, setExpanded] = useState<string | null>(null)
  const [source, setSource] = useState<Record<string, string>>({})
  const [busy, setBusy] = useState<string | null>(null)
  const [draft, setDraft] = useState<{ text: string; error?: string | null } | null>(null)
  const fileRef = useRef<HTMLInputElement>(null)

  const reload = () => skillsApi.list().then(({ data }) => setSkills(data.skills)).catch((err) => onToast?.(errorText(err), 'error'))

  useEffect(() => {
    if (open) reload()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open])

  const groups = useMemo(() => {
    const byCategory: Record<string, SkillSummary[]> = {}
    for (const s of skills || []) (byCategory[s.category || '其他'] ||= []).push(s)
    return Object.entries(byCategory).sort(([a], [b]) => a.localeCompare(b, 'zh'))
  }, [skills])

  if (!open) return null

  const toggle = async (skill: SkillSummary) => {
    setBusy(skill.name)
    try {
      await skillsApi.setEnabled(skill.name, !skill.enabled)
      setSkills((list) => list?.map((s) => (s.name === skill.name ? { ...s, enabled: !s.enabled } : s)) || null)
    } catch (err) {
      onToast?.(errorText(err), 'error')
    } finally {
      setBusy(null)
    }
  }

  const expand = async (name: string) => {
    if (expanded === name) return setExpanded(null)
    setExpanded(name)
    if (!source[name]) {
      try {
        const { data } = await skillsApi.detail(name)
        setSource((m) => ({ ...m, [name]: data.skill_md || '' }))
      } catch (err) {
        onToast?.(errorText(err), 'error')
      }
    }
  }

  const remove = async (skill: SkillSummary) => {
    if (!window.confirm(`删除技能 ${skill.name}？`)) return
    try {
      await skillsApi.remove(skill.name)
      onToast?.(`已删除 ${skill.name}`, 'success')
      reload()
    } catch (err) {
      onToast?.(errorText(err), 'error')
    }
  }

  const importFile = async (file: File) => {
    setBusy('import')
    try {
      const { data } = await skillsApi.importFile(file)
      onToast?.(`已导入技能 ${data.name}`, 'success')
      reload()
    } catch (err) {
      onToast?.(errorText(err), 'error')
    } finally {
      setBusy(null)
      if (fileRef.current) fileRef.current.value = ''
    }
  }

  const generateDraft = async () => {
    if (!sessionId) return
    setBusy('draft')
    try {
      const { data } = await skillsApi.draft(sessionId)
      setDraft({ text: data.skill_md, error: data.error })
    } catch (err) {
      onToast?.(errorText(err), 'error')
    } finally {
      setBusy(null)
    }
  }

  const saveDraft = async () => {
    if (!draft) return
    setBusy('save')
    try {
      const { data } = await skillsApi.create(draft.text)
      onToast?.(`已保存技能 ${data.name}`, 'success')
      setDraft(null)
      reload()
    } catch (err) {
      setDraft({ ...draft, error: errorText(err) })
    } finally {
      setBusy(null)
    }
  }

  return (
    <div className="fixed inset-0 z-[100] flex items-center justify-center bg-black/30 p-3" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div role="dialog" aria-label="技能" className="flex max-h-[88vh] w-full max-w-2xl flex-col overflow-hidden rounded-2xl bg-white shadow-2xl">
        <div className="flex items-center gap-2 border-b border-surface-100 px-4 py-3">
          <BookOpen className="h-4 w-4 text-surface-600" />
          <div className="min-w-0 flex-1">
            <div className="text-sm font-semibold text-surface-900">技能</div>
            <div className="text-[11px] text-surface-400">兼容 Agent Skills（SKILL.md）与 OpenClaw 技能；在输入框输入 / 可直接调用</div>
          </div>
          <button type="button" onClick={onClose} className="rounded-lg p-1.5 text-surface-500 hover:bg-surface-100" aria-label="关闭">
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="flex flex-wrap gap-2 border-b border-surface-100 px-4 py-2">
          <input ref={fileRef} type="file" accept=".zip,.md" className="hidden" onChange={(e) => e.target.files?.[0] && importFile(e.target.files[0])} />
          <button type="button" onClick={() => fileRef.current?.click()} disabled={busy === 'import'} className="btn-secondary h-8 px-3 text-xs">
            {busy === 'import' ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Upload className="h-3.5 w-3.5" />}
            导入 .zip / SKILL.md
          </button>
          <button
            type="button"
            onClick={generateDraft}
            disabled={!sessionId || busy === 'draft'}
            title={sessionId ? '把当前对话的做法总结成技能' : '先进行一段对话'}
            className="btn-secondary h-8 px-3 text-xs"
          >
            {busy === 'draft' ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Sparkles className="h-3.5 w-3.5" />}
            从当前对话生成技能
          </button>
        </div>

        {draft && (
          <div className="border-b border-surface-100 bg-surface-50/60 px-4 py-3">
            <div className="mb-1.5 text-xs font-medium text-surface-700">技能草稿（可直接修改后保存）</div>
            <textarea
              value={draft.text}
              onChange={(e) => setDraft({ text: e.target.value, error: null })}
              rows={12}
              className="w-full rounded-lg border border-black/[0.08] bg-white p-2 font-mono text-[11px] leading-5 outline-none focus:border-surface-400"
            />
            {draft.error && <div className="mt-1 text-xs text-red-600">{draft.error}</div>}
            <div className="mt-2 flex gap-2">
              <button type="button" onClick={saveDraft} disabled={busy === 'save'} className="inline-flex items-center gap-1 rounded-lg bg-surface-950 px-3 py-1.5 text-xs font-medium text-white disabled:opacity-50">
                {busy === 'save' && <Loader2 className="h-3 w-3 animate-spin" />}保存技能
              </button>
              <button type="button" onClick={() => setDraft(null)} className="rounded-lg px-3 py-1.5 text-xs text-surface-500 hover:text-surface-800">
                放弃
              </button>
            </div>
          </div>
        )}

        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-2">
          {skills === null && (
            <div className="flex items-center gap-2 py-6 text-xs text-surface-400">
              <Loader2 className="h-3.5 w-3.5 animate-spin" />正在加载…
            </div>
          )}
          {groups.map(([category, items]) => (
            <div key={category} className="py-1.5">
              <div className="py-1 text-[11px] font-semibold text-surface-400">{category}</div>
              {items.map((skill) => (
                <div key={skill.name} className="rounded-xl border border-black/[0.05] bg-white px-3 py-2 mb-1.5">
                  <div className="flex items-start gap-2">
                    <button type="button" onClick={() => expand(skill.name)} className="min-w-0 flex-1 text-left">
                      <div className="flex items-center gap-1.5 text-xs font-medium text-surface-800">
                        {skill.name}
                        <span className={`rounded px-1 text-[10px] font-normal ${skill.source === 'user' ? 'bg-amber-50 text-amber-700' : 'bg-surface-100 text-surface-500'}`}>
                          {skill.source === 'user' ? '我的' : '内置'}
                        </span>
                        {skill.disable_model_invocation && <span className="rounded bg-surface-100 px-1 text-[10px] font-normal text-surface-500">仅手动调用</span>}
                        <ChevronDown className={`h-3 w-3 text-surface-400 transition-transform ${expanded === skill.name ? 'rotate-180' : ''}`} />
                      </div>
                      <div className="mt-0.5 text-[11px] leading-5 text-surface-500">{skill.description}</div>
                    </button>
                    {skill.source === 'user' && (
                      <button type="button" onClick={() => remove(skill)} className="rounded p-1 text-surface-400 hover:bg-red-50 hover:text-red-600" aria-label={`删除 ${skill.name}`}>
                        <Trash2 className="h-3.5 w-3.5" />
                      </button>
                    )}
                    <label className="flex shrink-0 cursor-pointer items-center gap-1 text-[11px] text-surface-500">
                      <input type="checkbox" checked={skill.enabled} disabled={busy === skill.name} onChange={() => toggle(skill)} className="accent-surface-900" />
                      启用
                    </label>
                  </div>
                  {expanded === skill.name && (
                    <div className="mt-2">
                      {skill.files.length > 0 && <div className="mb-1 text-[11px] text-surface-400">附带文件：{skill.files.join('、')}</div>}
                      <pre className="max-h-64 overflow-auto whitespace-pre-wrap rounded-lg bg-surface-50 p-2 text-[11px] leading-5 text-surface-700">
                        {source[skill.name] ?? '加载中…'}
                      </pre>
                    </div>
                  )}
                </div>
              ))}
            </div>
          ))}
        </div>
      </div>
    </div>
  )
}
