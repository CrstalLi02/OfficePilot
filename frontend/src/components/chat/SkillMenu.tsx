import { useEffect, useMemo, useRef, useState } from 'react'
import { BookOpen, Loader2, Search, Settings2 } from 'lucide-react'
import { skillsApi } from '@/api'
import type { SkillSummary } from '@/types'

interface SkillMenuProps {
  onSelect: (skill: SkillSummary) => void
  onManage: () => void
  onClose: () => void
}

/** 输入框 `/` 菜单：选择要在本轮使用的技能 */
export function SkillMenu({ onSelect, onManage, onClose }: SkillMenuProps) {
  const [skills, setSkills] = useState<SkillSummary[] | null>(null)
  const [query, setQuery] = useState('')
  const [index, setIndex] = useState(0)
  const containerRef = useRef<HTMLDivElement>(null)
  const inputRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    skillsApi.list().then(({ data }) => setSkills(data.skills)).catch(() => setSkills([]))
    inputRef.current?.focus()
  }, [])

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase()
    return (skills || []).filter(
      (s) => s.enabled && s.user_invocable && (!q || s.name.includes(q) || s.description.toLowerCase().includes(q) || (s.category || '').includes(q))
    )
  }, [skills, query])
  // 最后一项是"管理技能"
  const total = visible.length + 1

  useEffect(() => setIndex(0), [query])

  useEffect(() => {
    const onMouseDown = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) onClose()
    }
    const onKeyDown = (e: KeyboardEvent) => {
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
        else onManage()
      }
    }
    document.addEventListener('mousedown', onMouseDown, true)
    document.addEventListener('keydown', onKeyDown)
    return () => {
      document.removeEventListener('mousedown', onMouseDown, true)
      document.removeEventListener('keydown', onKeyDown)
    }
  }, [visible, index, total, onSelect, onManage, onClose])

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
      <div className="flex-1 overflow-y-auto py-1">
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
        {skills !== null && visible.length === 0 && <div className="px-3 py-4 text-center text-xs text-surface-300">没有匹配的技能</div>}
        <button
          type="button"
          onMouseEnter={() => setIndex(visible.length)}
          onClick={onManage}
          className={`flex w-full items-center gap-2.5 border-t border-surface-100 px-3 py-2 text-left text-xs text-surface-600 ${index === visible.length ? 'bg-primary-50' : 'hover:bg-surface-50'}`}
        >
          <Settings2 className="h-3.5 w-3.5 text-surface-400" />
          管理技能（启用 / 停用、导入、从对话生成）
        </button>
      </div>
    </div>
  )
}
