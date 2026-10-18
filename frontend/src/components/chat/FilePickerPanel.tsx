import { useEffect, useRef, useState, useMemo } from 'react'
import { FileEdit, Sheet, PenTool, Image as ImageIcon, Clapperboard, FileText, Search, Clock, MessageSquare, FolderOpen, Loader2, Puzzle } from 'lucide-react'
import { fileApi, mcpApi, type McpResource } from '@/api'
import type { Artifact, WorkspaceFile } from '@/types'

export interface FilePickerProps {
  /** 当前会话产物 */
  artifacts: Artifact[]
  /** 历史会话产物（从其他会话加载的） */
  historyArtifacts?: HistoryArtifactItem[]
  /** 选择产物回调 */
  onSelect: (artifact: Artifact, sessionId?: string) => void
  /** 选择工作区文件回调；不传则不显示"文件"标签 */
  onSelectFile?: (file: WorkspaceFile) => void
  /** 引用插件资源 */
  onSelectResource?: (resource: McpResource) => void
  onClose: () => void
}

export interface HistoryArtifactItem {
  artifact: Artifact
  sessionTitle: string
  sessionId: string
}

const KIND_ICON: Record<string, typeof FileEdit> = {
  document: FileText,
  markdown: FileText,
  ppt: FileEdit,
  sheet: Sheet,
  drawio: PenTool,
  image: ImageIcon,
  video: Clapperboard,
  chart: Sheet,
  code: FileEdit,
  mixed: FileEdit,
  search: FileEdit,
}

const KIND_LABEL: Record<string, string> = {
  document: '文档',
  markdown: 'MD',
  ppt: 'PPT',
  sheet: '表格',
  drawio: '图表',
  image: '图片',
  video: '视频',
  chart: '图表',
  code: '代码',
  mixed: '混合',
  search: '搜索',
}

/** 文件类型（files.file_type）对应的图标与标签 */
const FILE_TYPE: Record<string, { icon: typeof FileEdit; label: string }> = {
  doc: { icon: FileText, label: '文档' },
  excel: { icon: Sheet, label: '表格' },
  ppt: { icon: FileEdit, label: 'PPT' },
  drawio: { icon: PenTool, label: '图表' },
  image: { icon: ImageIcon, label: '图片' },
  video: { icon: Clapperboard, label: '视频' },
}

type PickerTab = 'current' | 'history' | 'files' | 'plugins'

interface PickerItem {
  key: string
  title: string
  label: string
  icon: typeof FileEdit
  pick: () => void
}

interface PickerGroup {
  name: string
  items: PickerItem[]
}

function matches(query: string, ...fields: Array<string | undefined>) {
  const q = query.trim().toLowerCase()
  return !q || fields.some((f) => f?.toLowerCase().includes(q))
}

export function FilePickerPanel({ artifacts, historyArtifacts, onSelect, onSelectFile, onSelectResource, onClose }: FilePickerProps) {
  const [query, setQuery] = useState('')
  const [selectedIndex, setSelectedIndex] = useState(0)
  const [activeTab, setActiveTab] = useState<PickerTab>('current')
  const [files, setFiles] = useState<WorkspaceFile[] | null>(onSelectFile ? null : [])
  const inputRef = useRef<HTMLInputElement>(null)
  const listRef = useRef<HTMLDivElement>(null)
  const containerRef = useRef<HTMLDivElement>(null)
  const history = useMemo(() => historyArtifacts || [], [historyArtifacts])

  // 工作区文件按需加载（只在打开时加载一次）
  const wantsFiles = Boolean(onSelectFile)
  useEffect(() => {
    if (!wantsFiles) return
    let cancelled = false
    fileApi
      .search('')
      .then(({ data }) => !cancelled && setFiles(Array.isArray(data?.files) ? data.files : []))
      .catch(() => !cancelled && setFiles([]))
    return () => {
      cancelled = true
    }
  }, [wantsFiles])

  // 插件资源按需加载；没有资源时不显示这个标签
  const [resources, setResources] = useState<McpResource[] | null>(onSelectResource ? null : [])
  const wantsResources = Boolean(onSelectResource)
  useEffect(() => {
    if (!wantsResources) return
    let cancelled = false
    mcpApi
      .resources()
      .then(({ data }) => !cancelled && setResources(data.resources))
      .catch(() => !cancelled && setResources([]))
    return () => {
      cancelled = true
    }
  }, [wantsResources])

  const tabs = useMemo(() => {
    const list: Array<{ id: PickerTab; label: string; icon: typeof FileEdit; count: number | null }> = []
    if (artifacts.length > 0) list.push({ id: 'current', label: '当前会话', icon: MessageSquare, count: artifacts.length })
    if (history.length > 0) list.push({ id: 'history', label: '历史会话', icon: Clock, count: history.length })
    if (onSelectFile) list.push({ id: 'files', label: '文件', icon: FolderOpen, count: files ? files.length : null })
    if (resources && resources.length > 0) list.push({ id: 'plugins', label: '插件资源', icon: Puzzle, count: resources.length })
    return list
  }, [artifacts.length, history.length, onSelectFile, files, resources])

  // 当前标签没有内容时切到第一个有内容的标签
  useEffect(() => {
    if (!tabs.some((tab) => tab.id === activeTab) && tabs.length > 0) setActiveTab(tabs[0].id)
  }, [tabs, activeTab])

  const groups: PickerGroup[] = useMemo(() => {
    if (activeTab === 'current') {
      const byKind: Record<string, PickerItem[]> = {}
      for (const a of artifacts.filter((a) => matches(query, a.title, a.kind))) {
        const kind = a.kind || 'mixed'
        ;(byKind[kind] ||= []).push({ key: a.id, title: a.title || '未命名', label: KIND_LABEL[kind] || kind, icon: KIND_ICON[kind] || FileEdit, pick: () => onSelect(a) })
      }
      return Object.keys(byKind).sort().map((kind) => ({ name: KIND_LABEL[kind] || kind, items: byKind[kind] }))
    }
    if (activeTab === 'history') {
      const bySession: Record<string, PickerItem[]> = {}
      for (const item of history.filter((i) => matches(query, i.artifact.title, i.artifact.kind, i.sessionTitle))) {
        const kind = item.artifact.kind || 'mixed'
        ;(bySession[item.sessionTitle || '未命名会话'] ||= []).push({
          key: `${item.sessionId}:${item.artifact.id}`,
          title: item.artifact.title || '未命名',
          label: KIND_LABEL[kind] || kind,
          icon: KIND_ICON[kind] || FileEdit,
          pick: () => onSelect(item.artifact, item.sessionId),
        })
      }
      return Object.keys(bySession).sort().map((name) => ({ name, items: bySession[name] }))
    }
    if (activeTab === 'plugins') {
      const byServer: Record<string, PickerItem[]> = {}
      for (const r of (resources || []).filter((r) => matches(query, r.title || undefined, r.name, r.uri, r.server_name))) {
        ;(byServer[r.server_name] ||= []).push({
          key: `${r.server_id}:${r.uri}`,
          title: r.title || r.name || r.uri,
          label: r.mime_type?.startsWith('image/') ? '图片' : '资源',
          icon: r.mime_type?.startsWith('image/') ? ImageIcon : Puzzle,
          pick: () => onSelectResource?.(r),
        })
      }
      return Object.keys(byServer).sort().map((name) => ({ name, items: byServer[name] }))
    }
    const items = (files || [])
      .filter((f) => matches(query, f.name))
      .slice(0, 100)
      .map((f) => {
        const meta = FILE_TYPE[f.file_type] || { icon: FileEdit, label: '文件' }
        return { key: f.id, title: f.name, label: meta.label, icon: meta.icon, pick: () => onSelectFile?.(f) }
      })
    return items.length ? [{ name: '最近修改', items }] : []
  }, [activeTab, artifacts, history, files, resources, query, onSelect, onSelectFile, onSelectResource])

  const flat = useMemo(() => groups.flatMap((g) => g.items), [groups])

  useEffect(() => {
    inputRef.current?.focus()
  }, [])

  // 点击外部关闭
  useEffect(() => {
    const handleMouseDown = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) onClose()
    }
    // 用 mousedown 且 capture 阶段，确保在 click 之前拦截
    document.addEventListener('mousedown', handleMouseDown, true)
    return () => document.removeEventListener('mousedown', handleMouseDown, true)
  }, [onClose])

  // 全局键盘导航（不依赖 search input 焦点）
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault()
        onClose()
      } else if (e.key === 'ArrowDown') {
        e.preventDefault()
        setSelectedIndex((i) => Math.min(i + 1, flat.length - 1))
      } else if (e.key === 'ArrowUp') {
        e.preventDefault()
        setSelectedIndex((i) => Math.max(i - 1, 0))
      } else if (e.key === 'Enter') {
        e.preventDefault()
        flat[selectedIndex]?.pick()
      } else if (e.key === 'Tab' && tabs.length > 1) {
        e.preventDefault()
        const index = tabs.findIndex((tab) => tab.id === activeTab)
        setActiveTab(tabs[(index + 1) % tabs.length].id)
      }
    }
    document.addEventListener('keydown', handleKeyDown)
    return () => document.removeEventListener('keydown', handleKeyDown)
  }, [flat, selectedIndex, tabs, activeTab, onClose])

  useEffect(() => {
    setSelectedIndex(0)
  }, [query, activeTab])

  // 滚动到选中项
  useEffect(() => {
    listRef.current?.querySelector(`[data-idx="${selectedIndex}"]`)?.scrollIntoView({ block: 'nearest' })
  }, [selectedIndex])

  const loadingFiles = activeTab === 'files' && files === null
  let runningIdx = -1

  return (
    <div ref={containerRef} className="absolute bottom-full left-0 right-0 z-50 mb-2 flex max-h-[360px] flex-col overflow-hidden rounded-2xl border border-surface-200 bg-white shadow-[0_8px_30px_rgba(15,23,42,0.12)]">
      {/* 搜索栏 + Tab 切换 */}
      <div className="border-b border-surface-100 px-3 py-2">
        <div className="flex items-center gap-2">
          <Search className="h-3.5 w-3.5 shrink-0 text-surface-400" />
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={activeTab === 'files' ? '搜索文件…' : activeTab === 'plugins' ? '搜索插件资源…' : '搜索产物…'}
            className="w-full border-0 bg-transparent p-0 text-xs text-surface-800 outline-none placeholder:text-surface-300"
          />
          <span className="shrink-0 rounded bg-surface-50 px-1.5 py-0.5 text-[10px] font-medium text-surface-400">ESC 关闭</span>
        </div>
        {tabs.length > 1 && (
          <div className="mt-2 flex gap-1">
            {tabs.map((tab) => {
              const Icon = tab.icon
              return (
                <button
                  key={tab.id}
                  type="button"
                  onClick={() => setActiveTab(tab.id)}
                  className={`inline-flex items-center gap-1 rounded-md px-2 py-1 text-[10px] font-medium transition ${
                    activeTab === tab.id ? 'bg-primary-50 text-primary-700' : 'text-surface-400 hover:bg-surface-50'
                  }`}
                >
                  <Icon className="h-3 w-3" />
                  {tab.label}
                  {tab.count !== null && `（${tab.count}）`}
                </button>
              )
            })}
          </div>
        )}
      </div>

      {/* 列表 */}
      <div ref={listRef} className="flex-1 overflow-y-auto py-1">
        {loadingFiles && (
          <div className="flex items-center gap-2 px-3 py-6 text-xs text-surface-400">
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
            正在加载文件…
          </div>
        )}
        {!loadingFiles && flat.length === 0 && (
          <div className="px-3 py-6 text-center text-xs text-surface-300">
            {tabs.length === 0 ? '当前还没有任何可引用的产物' : query ? '没有匹配的结果' : '这里还没有内容'}
          </div>
        )}
        {groups.map((group) => (
          <div key={group.name}>
            <div className="px-3 py-1 text-[10px] font-semibold uppercase tracking-wide text-surface-300">
              {group.name}（{group.items.length}）
            </div>
            {group.items.map((item) => {
              runningIdx++
              const idx = runningIdx
              const Icon = item.icon
              const isSelected = idx === selectedIndex
              return (
                <div
                  key={item.key}
                  data-idx={idx}
                  onMouseEnter={() => setSelectedIndex(idx)}
                  onClick={item.pick}
                  className={`flex cursor-pointer items-center gap-2.5 px-3 py-2 text-xs transition-colors ${
                    isSelected ? 'bg-primary-50 text-primary-700' : 'text-surface-700 hover:bg-surface-50'
                  }`}
                >
                  <Icon className={`h-3.5 w-3.5 shrink-0 ${isSelected ? 'text-primary-500' : 'text-surface-400'}`} />
                  <span className="truncate font-medium">{item.title}</span>
                  <span className="ml-auto shrink-0 text-[10px] text-surface-300">{item.label}</span>
                </div>
              )
            })}
          </div>
        ))}
      </div>

      {/* 底部提示 */}
      <div className="border-t border-surface-100 px-3 py-1.5 text-[10px] text-surface-300">
        <kbd className="font-sans">↑↓</kbd> 选择 · <kbd className="font-sans">Enter</kbd> 引入 · <kbd className="font-sans">Tab</kbd> 切换 · <kbd className="font-sans">ESC</kbd> 关闭
      </div>
    </div>
  )
}
