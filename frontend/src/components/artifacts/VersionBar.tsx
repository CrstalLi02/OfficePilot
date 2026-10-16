import { useEffect, useMemo, useState } from 'react'
import { Bot, ChevronDown, History, Loader2, RotateCcw, User, X } from 'lucide-react'
import { artifactApi, type ArtifactVersionMeta } from '@/api'
import { artifactToLines, collapseUnchanged, diffLines } from '@/lib/artifact-diff'
import type { Artifact } from '@/types'

function formatTime(iso: string) {
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return ''
  const diff = (Date.now() - date.getTime()) / 1000
  if (diff < 60) return '刚刚'
  if (diff < 3600) return `${Math.floor(diff / 60)} 分钟前`
  if (diff < 86400) return `${Math.floor(diff / 3600)} 小时前`
  return date.toLocaleString('zh-CN', { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit' })
}

function errorText(err: unknown) {
  const anyErr = err as { response?: { data?: { detail?: string } }; message?: string }
  return anyErr?.response?.data?.detail || anyErr?.message || '请求失败'
}

/** 旧版本与当前版本的逐行差异 */
function DiffView({ before, after }: { before: Artifact; after: Artifact }) {
  const rows = useMemo(() => collapseUnchanged(diffLines(artifactToLines(before), artifactToLines(after))), [before, after])
  const changed = rows.some((row) => row.type === 'add' || row.type === 'remove')
  if (!changed) return <div className="p-4 text-center text-xs text-surface-400">两个版本内容相同</div>
  return (
    <div className="font-mono text-[11px] leading-5">
      {rows.map((row, index) =>
        row.type === 'skip' ? (
          <div key={index} className="bg-surface-50 px-3 text-surface-400">⋯ {row.count} 行未改动</div>
        ) : (
          <div
            key={index}
            className={`whitespace-pre-wrap break-all px-3 ${
              row.type === 'add' ? 'bg-emerald-50 text-emerald-800' : row.type === 'remove' ? 'bg-red-50 text-red-700 line-through decoration-red-300' : 'text-surface-500'
            }`}
          >
            <span className="mr-2 select-none text-surface-300">{row.type === 'add' ? '+' : row.type === 'remove' ? '−' : ' '}</span>
            {row.text || ' '}
          </div>
        )
      )}
    </div>
  )
}

interface VersionBarProps {
  artifact: Artifact
  sessionId: string | null
  /** 恢复后用服务端返回的新版本替换当前产物 */
  onReplaced: (artifact: Artifact) => void
}

/** 产物版本：当前版本号、历史列表、与当前版本对比、恢复旧版本 */
export function VersionBar({ artifact, sessionId, onReplaced }: VersionBarProps) {
  const [open, setOpen] = useState(false)
  const [versions, setVersions] = useState<ArtifactVersionMeta[] | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState('')
  const [compare, setCompare] = useState<Artifact | null>(null)
  const [restoring, setRestoring] = useState(false)

  // 切换产物或版本变化后，历史需要重新加载
  useEffect(() => {
    setVersions(null)
    setCompare(null)
    setError('')
  }, [artifact.id, artifact.version])

  useEffect(() => {
    if (!open || versions || !sessionId) return
    setLoading(true)
    artifactApi
      .versions(artifact.id, sessionId)
      .then(({ data }) => setVersions(data.versions))
      .catch((err) => setError(errorText(err)))
      .finally(() => setLoading(false))
  }, [open, versions, sessionId, artifact.id])

  if (!sessionId) return null

  const openCompare = async (version: number) => {
    setError('')
    try {
      const { data } = await artifactApi.version(artifact.id, version, sessionId)
      setCompare(data.artifact)
      setOpen(false)
    } catch (err) {
      setError(errorText(err))
    }
  }

  const restore = async () => {
    if (!compare) return
    setRestoring(true)
    try {
      const { data } = await artifactApi.restore(artifact.id, sessionId, compare.version)
      onReplaced(data.artifact)
      setCompare(null)
    } catch (err) {
      setError(errorText(err))
    } finally {
      setRestoring(false)
    }
  }

  return (
    <>
      <div className="relative">
        <button
          type="button"
          onClick={() => setOpen((value) => !value)}
          className="inline-flex h-7 items-center gap-1 rounded-full border border-black/[0.08] bg-white px-2 text-[11px] font-medium text-surface-600 hover:text-surface-900"
          title="版本历史"
          aria-expanded={open}
        >
          <History className="h-3 w-3" />v{artifact.version}
          <ChevronDown className={`h-3 w-3 transition-transform ${open ? 'rotate-180' : ''}`} />
        </button>
        {open && (
          <div className="absolute right-0 top-8 z-40 w-72 overflow-hidden rounded-xl border border-black/[0.08] bg-white shadow-xl">
            <div className="border-b border-black/[0.05] px-3 py-2 text-[11px] text-surface-400">版本历史 · 点击与当前版本对比</div>
            <div className="max-h-72 overflow-y-auto">
              {loading && <div className="flex items-center gap-2 px-3 py-3 text-xs text-surface-400"><Loader2 className="h-3 w-3 animate-spin" />加载中</div>}
              {versions?.map((item) => {
                const current = item.version === artifact.version
                return (
                  <button
                    key={item.version}
                    type="button"
                    disabled={current}
                    onClick={() => openCompare(item.version)}
                    className="flex w-full items-start gap-2 px-3 py-2 text-left hover:bg-surface-50 disabled:cursor-default disabled:hover:bg-transparent"
                  >
                    <span className={`mt-0.5 shrink-0 rounded-full px-1.5 text-[10px] font-semibold ${current ? 'bg-surface-950 text-white' : 'bg-surface-100 text-surface-600'}`}>v{item.version}</span>
                    <span className="min-w-0 flex-1">
                      <span className="block truncate text-xs text-surface-800">{item.change_summary || (item.version === 1 ? '初次生成' : item.title)}</span>
                      <span className="mt-0.5 flex items-center gap-1 text-[10px] text-surface-400">
                        {item.author === 'user' ? <User className="h-2.5 w-2.5" /> : <Bot className="h-2.5 w-2.5" />}
                        {item.author === 'user' ? '我' : 'AI'} · {formatTime(item.created_at)}
                        {current && ' · 当前'}
                      </span>
                    </span>
                  </button>
                )
              })}
              {error && <div className="px-3 py-2 text-xs text-red-600">{error}</div>}
            </div>
          </div>
        )}
      </div>

      {compare && (
        <div className="absolute inset-x-0 bottom-0 top-14 z-30 flex flex-col bg-white">
          <div className="flex items-center gap-2 border-b border-black/[0.06] px-4 py-2">
            <div className="min-w-0 flex-1">
              <div className="truncate text-xs font-semibold text-surface-800">v{compare.version} → 当前 v{artifact.version}</div>
              <div className="truncate text-[11px] text-surface-400">{compare.change_summary || '初次生成'} · <span className="text-red-600">删除</span> / <span className="text-emerald-700">新增</span></div>
            </div>
            <button
              type="button"
              disabled={restoring}
              onClick={restore}
              className="inline-flex shrink-0 items-center gap-1 rounded-lg bg-surface-950 px-2.5 py-1.5 text-xs font-medium text-white disabled:opacity-50"
            >
              {restoring ? <Loader2 className="h-3 w-3 animate-spin" /> : <RotateCcw className="h-3 w-3" />}
              恢复 v{compare.version}
            </button>
            <button type="button" onClick={() => setCompare(null)} className="shrink-0 rounded-lg p-1.5 text-surface-500 hover:bg-surface-100" aria-label="关闭对比">
              <X className="h-4 w-4" />
            </button>
          </div>
          {error && <div className="border-b border-red-100 bg-red-50 px-4 py-1.5 text-xs text-red-600">{error}</div>}
          <div className="min-h-0 flex-1 overflow-auto py-2">
            <DiffView before={compare} after={artifact} />
          </div>
        </div>
      )}
    </>
  )
}
