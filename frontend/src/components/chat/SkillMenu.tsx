import { useEffect, useMemo, useRef, useState } from 'react'
import { BookOpen, Loader2, Puzzle, Search, Settings2 } from 'lucide-react'
import { mcpApi, skillsApi, type McpPrompt } from '@/api'
import type { SkillSummary } from '@/types'

interface SkillMenuProps {
  onSelect: (skill: SkillSummary) => void
  onManage: () => void
  onClose: () => void
  /** 选了插件提示词：把渲染出的文本填进输入框 */
  onUsePrompt?: (text: string) => void
}

/** 输入框 `/` 菜单：选择要在本轮使用的技能 */
export function SkillMenu({ onSelect, onManage, onClose, onUsePrompt }: SkillMenuProps) {
  const [skills, setSkills] = useState<SkillSummary[] | null>(null)
  const [prompts, setPrompts] = useState<McpPrompt[]>([])
  const [form, setForm] = useState<{ prompt: McpPrompt; values: Record<string, string> } | null>(null)
  const [promptBusy, setPromptBusy] = useState(false)
  const [promptError, setPromptError] = useState('')
  const [query, setQuery] = useState('')
  const [index, setIndex] = useState(0)
  const containerRef = useRef<HTMLDivElement>(null)
  const inputRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    skillsApi.list().then(({ data }) => setSkills(data.skills)).catch(() => setSkills([]))
    if (onUsePrompt) mcpApi.prompts().then(({ data }) => setPrompts(data.prompts)).catch(() => setPrompts([]))
    inputRef.current?.focus()
  }, [])

  const renderPrompt = async (prompt: McpPrompt, values: Record<string, string>) => {
    setPromptBusy(true)
    setPromptError('')
    try {
      const { data } = await mcpApi.getPrompt(prompt.server_id, prompt.name, values)
      onUsePrompt?.(data.text)
    } catch (err: any) {
      setPromptError(err?.response?.data?.detail?.replace(/^参数错误:\s*/, '') || '获取提示词失败')
    } finally {
      setPromptBusy(false)
    }
  }

  const choosePrompt = (prompt: McpPrompt) => {
    if (prompt.arguments.length > 0) setForm({ prompt, values: {} })
    else renderPrompt(prompt, {})
  }

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase()
    return (skills || []).filter(
      (s) => s.enabled && s.user_invocable && (!q || s.name.includes(q) || s.description.toLowerCase().includes(q) || (s.category || '').includes(q))
    )
  }, [skills, query])
  const visiblePrompts = useMemo(() => {
    const q = query.trim().toLowerCase()
    return prompts.filter((p) => !q || [p.name, p.title, p.description, p.server_name].some((f) => f?.toLowerCase().includes(q)))
  }, [prompts, query])
  // 顺序：技能、插件提示词、最后一项“管理技能”
  const total = visible.length + visiblePrompts.length + 1

  useEffect(() => setIndex(0), [query])

  useEffect(() => {
    const onMouseDown = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) onClose()
    }
    const onKeyDown = (e: KeyboardEvent) => {
      // 填写提示词参数时：Esc 返回列表，其余按键交给表单
      if (form) {
        if (e.key === 'Escape') {
          e.preventDefault()
          setForm(null)
        }
        return
      }
      if (e.key === 'Escape') {
        e.preventDefault()
        onClose()
      } else if (e.key === 'ArrowDown') {
        e.preventDefault()
        setIndex((i) => Math.min(i + 1, total - 1))
      } else if (e.key === 'ArrowUp') {
        e.preventDefault()
        setIndex((i) => Math.max(i - 1, 0))
      } else if (e.key === 'Enter') {
        e.preventDefault()
        if (index < visible.length) onSelect(visible[index])
        else if (index < visible.length + visiblePrompts.length) choosePrompt(visiblePrompts[index - visible.length])
        else onManage()
      }
    }
    document.addEventListener('mousedown', onMouseDown, true)
    document.addEventListener('keydown', onKeyDown)
    return () => {
      document.removeEventListener('mousedown', onMouseDown, true)
      document.removeEventListener('keydown', onKeyDown)
    }
  }, [visible, visiblePrompts, form, index, total, onSelect, onManage, onClose])

  return (
    <div ref={containerRef} className="absolute bottom-full left-0 right-0 z-50 mb-2 flex max-h-[360px] flex-col overflow-hidden rounded-2xl border border-surface-200 bg-white shadow-[0_8px_30px_rgba(15,23,42,0.12)]">
      <div className="flex items-center gap-2 border-b border-surface-100 px-3 py-2">
        <Search className="h-3.5 w-3.5 shrink-0 text-surface-400" />
        <input
          ref={inputRef}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="选择技能，本轮按技能的流程和标准执行…"
          className="w-full border-0 bg-transparent p-0 text-xs text-surface-800 outline-none placeholder:text-surface-300"
        />
      </div>
      {form && (
        <form
          className="flex-1 space-y-2 overflow-y-auto px-3 py-2.5"
          onSubmit={(e) => {
            e.preventDefault()
            renderPrompt(form.prompt, form.values)
          }}
        >
          <div className="text-xs font-semibold text-surface-800">
            {form.prompt.title || form.prompt.name}
            <span className="ml-1.5 font-normal text-surface-400">· {form.prompt.server_name}</span>
          </div>
          {form.prompt.arguments.map((arg, i) => (
            <label key={arg.name} className="block text-[11px] text-surface-600">
              {arg.description || arg.name}
              {arg.required && <span className="text-red-500"> *</span>}
              <input
                autoFocus={i === 0}
                required={arg.required}
                value={form.values[arg.name] || ''}
                onChange={(e) => setForm({ ...form, values: { ...form.values, [arg.name]: e.target.value } })}
                className="mt-1 w-full rounded-lg border border-surface-200 px-2 py-1.5 text-xs outline-none focus:border-surface-400"
              />
            </label>
          ))}
          {promptError && <div className="text-[11px] text-red-600">{promptError}</div>}
          <div className="flex gap-2 pt-1">
            <button type="submit" disabled={promptBusy} className="inline-flex items-center gap-1 rounded-lg bg-surface-950 px-3 py-1.5 text-xs font-medium text-white disabled:opacity-50">
              {promptBusy && <Loader2 className="h-3 w-3 animate-spin" />}填入输入框
            </button>
            <button type="button" onClick={() => setForm(null)} className="rounded-lg px-3 py-1.5 text-xs text-surface-500 hover:bg-surface-50">返回</button>
          </div>
        </form>
      )}
      <div className={`flex-1 overflow-y-auto py-1 ${form ? 'hidden' : ''}`}>
        {skills === null && (
          <div className="flex items-center gap-2 px-3 py-4 text-xs text-surface-400">
            <Loader2 className="h-3.5 w-3.5 animate-spin" />正在加载技能…
          </div>
        )}
        {visible.map((skill, i) => (
          <button
            key={skill.name}
            type="button"
            onMouseEnter={() => setIndex(i)}
            onClick={() => onSelect(skill)}
            className={`flex w-full items-start gap-2.5 px-3 py-2 text-left text-xs ${i === index ? 'bg-primary-50' : 'hover:bg-surface-50'}`}
          >
            <BookOpen className="mt-0.5 h-3.5 w-3.5 shrink-0 text-surface-400" />
            <span className="min-w-0 flex-1">
              <span className="flex items-center gap-1.5 font-medium text-surface-800">
                /{skill.name}
                {skill.category && <span className="rounded bg-surface-100 px-1 text-[10px] font-normal text-surface-500">{skill.category}</span>}
                {skill.source === 'user' && <span className="rounded bg-amber-50 px-1 text-[10px] font-normal text-amber-700">我的</span>}
              </span>
              <span className="mt-0.5 line-clamp-2 block text-[11px] text-surface-500">{skill.description}</span>
            </span>
          </button>
        ))}
        {skills !== null && visible.length === 0 && visiblePrompts.length === 0 && <div className="px-3 py-4 text-center text-xs text-surface-300">没有匹配的技能</div>}
        {visiblePrompts.length > 0 && <div className="border-t border-surface-100 px-3 pb-1 pt-2 text-[10px] font-medium text-surface-400">插件提示词</div>}
        {visiblePrompts.map((prompt, j) => {
          const i = visible.length + j
          return (
            <button
              key={`${prompt.server_id}:${prompt.name}`}
              type="button"
              disabled={promptBusy}
              onMouseEnter={() => setIndex(i)}
              onClick={() => choosePrompt(prompt)}
              className={`flex w-full items-start gap-2.5 px-3 py-2 text-left text-xs disabled:opacity-60 ${i === index ? 'bg-primary-50' : 'hover:bg-surface-50'}`}
            >
              <Puzzle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-violet-400" />
              <span className="min-w-0 flex-1">
                <span className="flex items-center gap-1.5 font-medium text-surface-800">
                  {prompt.title || prompt.name}
                  <span className="rounded bg-violet-50 px-1 text-[10px] font-normal text-violet-700">{prompt.server_name}</span>
                </span>
                {prompt.description && <span className="mt-0.5 line-clamp-2 block text-[11px] text-surface-500">{prompt.description}</span>}
              </span>
            </button>
          )
        })}
        {promptError && !form && <div className="px-3 py-1.5 text-[11px] text-red-600">{promptError}</div>}
        <button
          type="button"
          onMouseEnter={() => setIndex(visible.length + visiblePrompts.length)}
          onClick={onManage}
          className={`flex w-full items-center gap-2.5 border-t border-surface-100 px-3 py-2 text-left text-xs text-surface-600 ${index === visible.length + visiblePrompts.length ? 'bg-primary-50' : 'hover:bg-surface-50'}`}
        >
          <Settings2 className="h-3.5 w-3.5 text-surface-400" />
          管理技能（启用 / 停用、导入、从对话生成）
        </button>
      </div>
    </div>
  )
}
