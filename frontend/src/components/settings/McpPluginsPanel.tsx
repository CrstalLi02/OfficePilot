import { useEffect, useState } from 'react'
import { ChevronDown, ExternalLink, FileJson, LayoutGrid, Loader2, Plus, Puzzle, RefreshCw, Trash2, X } from 'lucide-react'
import { mcpApi, type McpCatalogEntry, type McpServer, type McpServerInput, type McpTool } from '@/api'

const STATE: Record<string, { label: string; dot: string }> = {
  connected: { label: '已连接', dot: 'bg-emerald-500' },
  error: { label: '连接失败', dot: 'bg-red-500' },
  idle: { label: '未连接', dot: 'bg-surface-300' },
}

const TRANSPORTS = [
  ['http', 'Streamable HTTP', '远程服务（当前 MCP 规范）'],
  ['sse', 'SSE', '较早的远程服务'],
  ['stdio', '本地命令', '在服务器上启动命令，例如 npx'],
] as const

interface Pair {
  name: string
  value: string
  /** 已保存过值（编辑时留空沿用） */
  saved?: boolean
}

interface Draft {
  id?: string
  name: string
  slug: string
  transport: McpServer['transport']
  url: string
  command: string
  args: string
  description: string
  headers: Pair[]
  env: Pair[]
  enabled: boolean
  approval: McpServer['approval']
  assumeReadOnly: boolean
}

const APPROVALS = [
  ['writes', '写操作需要确认（推荐）'],
  ['always', '每次调用都确认'],
  ['never', '无需确认（定时任务里也能用写操作）'],
] as const

const emptyDraft = (): Draft => ({
  name: '',
  slug: '',
  transport: 'http',
  url: '',
  command: '',
  args: '',
  description: '',
  headers: [],
  env: [],
  enabled: true,
  approval: 'writes',
  assumeReadOnly: false,
})

const fromServer = (s: McpServer): Draft => ({
  id: s.id,
  name: s.name,
  slug: s.slug,
  transport: s.transport,
  url: s.url || '',
  command: s.command || '',
  args: s.args.join('\n'),
  description: s.description,
  headers: s.header_names.map((name) => ({ name, value: '', saved: true })),
  env: s.env_names.map((name) => ({ name, value: '', saved: true })),
  enabled: s.enabled,
  approval: s.approval || 'writes',
  assumeReadOnly: !!s.assume_read_only,
})

const toInput = (d: Draft): McpServerInput => ({
  name: d.name.trim(),
  slug: d.slug.trim() || undefined,
  transport: d.transport,
  url: d.transport === 'stdio' ? undefined : d.url.trim(),
  command: d.transport === 'stdio' ? d.command.trim() : undefined,
  args: d.transport === 'stdio' ? d.args.split('\n').map((a) => a.trim()).filter(Boolean) : [],
  description: d.description.trim(),
  headers: d.transport === 'stdio' ? [] : d.headers.filter((h) => h.name.trim()).map(({ name, value }) => ({ name: name.trim(), value })),
  env: d.transport === 'stdio' ? d.env.filter((h) => h.name.trim()).map(({ name, value }) => ({ name: name.trim(), value })) : [],
  enabled: d.enabled,
  approval: d.approval,
  assume_read_only: d.assumeReadOnly,
})

const serverToInput = (s: McpServer, enabled: boolean): McpServerInput => toInput({ ...fromServer(s), enabled })

const errorText = (err: any, fallback: string) => err?.response?.data?.detail?.replace(/^参数错误:\s*/, '') || err?.message || fallback

const field = 'mt-1.5 w-full rounded-xl border border-black/[0.08] bg-white px-3 py-2 text-sm outline-none focus:border-surface-400'

function PairsEditor({ label, hint, pairs, onChange, namePlaceholder }: { label: string; hint: string; pairs: Pair[]; onChange: (p: Pair[]) => void; namePlaceholder: string }) {
  return (
    <div className="mt-3">
      <div className="flex items-center justify-between text-xs font-medium text-surface-600">
        <span>{label}</span>
        <button type="button" onClick={() => onChange([...pairs, { name: '', value: '' }])} className="inline-flex items-center gap-1 rounded-lg px-2 py-0.5 text-[11px] text-surface-600 hover:bg-surface-100">
          <Plus className="h-3 w-3" />添加
        </button>
      </div>
      <div className="mt-0.5 text-[11px] text-surface-400">{hint}</div>
      {pairs.map((pair, i) => (
        <div key={i} className="mt-1.5 flex gap-2">
          <input className={`${field} mt-0 w-2/5`} placeholder={namePlaceholder} value={pair.name} onChange={(e) => onChange(pairs.map((p, j) => (j === i ? { ...p, name: e.target.value } : p)))} />
          <input
            className={`${field} mt-0 flex-1`}
            type="password"
            autoComplete="off"
            placeholder={pair.saved ? '已填写，留空不修改' : '值'}
            value={pair.value}
            onChange={(e) => onChange(pairs.map((p, j) => (j === i ? { ...p, value: e.target.value } : p)))}
          />
          <button type="button" onClick={() => onChange(pairs.filter((_, j) => j !== i))} className="rounded-lg px-1.5 text-surface-400 hover:bg-red-50 hover:text-red-600" title="删除">
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      ))}
    </div>
  )
}

function ServerForm({ draft, onChange, onSubmit, onCancel, saving, stdioAllowed }: { draft: Draft; onChange: (d: Draft) => void; onSubmit: () => void; onCancel: () => void; saving: boolean; stdioAllowed: boolean }) {
  const set = (patch: Partial<Draft>) => onChange({ ...draft, ...patch })
  return (
    <div className="mb-4 rounded-2xl border border-black/[0.08] bg-white p-4">
      <div className="mb-3 text-sm font-semibold text-surface-900">{draft.id ? '编辑插件' : '添加插件'}</div>
      <div className="grid gap-3 sm:grid-cols-2">
        <label className="text-xs font-medium text-surface-600">
          名称
          <input className={field} value={draft.name} placeholder="例如 高德地图" onChange={(e) => set({ name: e.target.value })} />
        </label>
        <label className="text-xs font-medium text-surface-600">
          标识（工具名前缀，可选）
          <input className={field} value={draft.slug} placeholder="不填则按名称生成，如 amap" onChange={(e) => set({ slug: e.target.value.toLowerCase() })} />
        </label>
      </div>

      <div className="mt-3 text-xs font-medium text-surface-600">连接方式</div>
      <div className="mt-1.5 grid gap-2 sm:grid-cols-3">
        {TRANSPORTS.map(([value, label, hint]) => {
          const disabled = value === 'stdio' && !stdioAllowed
          return (
            <button
              key={value}
              type="button"
              disabled={disabled}
              onClick={() => set({ transport: value })}
              title={disabled ? '服务端没有开启本地命令插件（自部署时设置 WALIOFFICE_MCP_STDIO=1）' : undefined}
              className={`rounded-xl border px-3 py-2 text-left disabled:cursor-not-allowed disabled:opacity-40 ${draft.transport === value ? 'border-surface-900 bg-surface-50' : 'border-black/[0.08] hover:border-surface-300'}`}
            >
              <div className="text-xs font-semibold text-surface-900">{label}</div>
              <div className="mt-0.5 text-[11px] text-surface-500">{disabled ? '服务端未开启' : hint}</div>
            </button>
          )
        })}
      </div>

      {draft.transport === 'stdio' ? (
        <>
          <div className="mt-3 grid gap-3 sm:grid-cols-2">
            <label className="text-xs font-medium text-surface-600">
              命令
              <input className={field} value={draft.command} placeholder="npx" onChange={(e) => set({ command: e.target.value })} />
            </label>
            <label className="text-xs font-medium text-surface-600">
              参数（每行一个）
              <textarea className={`${field} h-20 font-mono text-xs`} value={draft.args} placeholder={'-y\n@modelcontextprotocol/server-everything'} onChange={(e) => set({ args: e.target.value })} />
            </label>
          </div>
          <PairsEditor label="环境变量" hint="例如 API_KEY；值加密保存，不会显示出来" pairs={draft.env} onChange={(env) => set({ env })} namePlaceholder="名称，如 API_KEY" />
        </>
      ) : (
        <>
          <label className="mt-3 block text-xs font-medium text-surface-600">
            服务地址
            <input className={field} value={draft.url} placeholder={draft.transport === 'sse' ? 'https://example.com/sse' : 'https://example.com/mcp'} onChange={(e) => set({ url: e.target.value })} />
          </label>
          <PairsEditor label="请求头" hint="例如 Authorization: Bearer xxx；值加密保存，不会显示出来" pairs={draft.headers} onChange={(headers) => set({ headers })} namePlaceholder="名称，如 Authorization" />
        </>
      )}

      <label className="mt-3 block text-xs font-medium text-surface-600">
        调用前确认
        <select className={field} value={draft.approval} onChange={(e) => set({ approval: e.target.value as Draft['approval'] })}>
          {APPROVALS.map(([value, label]) => (
            <option key={value} value={value}>{label}</option>
          ))}
        </select>
        <span className="mt-1 block text-[11px] font-normal text-surface-400">需要确认时运行会暂停，在网页或飞书里点“允许”后继续；定时任务没人确认，会直接拒绝。</span>
      </label>
      <label className="mt-3 flex items-start gap-2 text-xs text-surface-600">
        <input type="checkbox" className="mt-0.5" checked={draft.assumeReadOnly} onChange={(e) => set({ assumeReadOnly: e.target.checked })} />
        <span>
          这个插件的工具都只读取数据
          <span className="block text-[11px] text-surface-400">服务没有标注只读时可以勾选：规划 / 问答模式也能用，且不需要确认。只对确实不会修改数据的服务勾选。</span>
        </span>
      </label>
      <label className="mt-3 block text-xs font-medium text-surface-600">
        说明（可选）
        <input className={field} value={draft.description} placeholder="这个插件是做什么的" onChange={(e) => set({ description: e.target.value })} />
      </label>

      <div className="mt-4 flex items-center gap-2">
        <button type="button" disabled={saving || !draft.name.trim()} onClick={onSubmit} className="inline-flex items-center gap-1.5 rounded-full bg-surface-950 px-4 py-2 text-sm font-semibold text-white hover:bg-surface-800 disabled:opacity-50">
          {saving && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
          {draft.id ? '保存' : '添加并连接'}
        </button>
        <button type="button" onClick={onCancel} className="rounded-full px-4 py-2 text-sm text-surface-600 hover:bg-surface-100">取消</button>
      </div>
    </div>
  )
}

function ImportBox({ onDone, onCancel }: { onDone: (msg: string, ok: boolean) => void; onCancel: () => void }) {
  const [text, setText] = useState('')
  const [busy, setBusy] = useState(false)
  const submit = async () => {
    setBusy(true)
    try {
      const { data } = await mcpApi.importConfig(text)
      const parts = [`导入了 ${data.created.length} 个插件`]
      if (data.errors.length) parts.push(`失败：${data.errors.join('；')}`)
      onDone(parts.join('，'), data.errors.length === 0)
    } catch (err) {
      onDone(errorText(err, '导入失败'), false)
    } finally {
      setBusy(false)
    }
  }
  return (
    <div className="mb-4 rounded-2xl border border-black/[0.08] bg-white p-4">
      <div className="text-sm font-semibold text-surface-900">从配置导入</div>
      <div className="mt-1 text-[11px] text-surface-500">粘贴 Claude Desktop、Cursor 等使用的 mcpServers JSON，会逐个创建插件。</div>
      <textarea
        className={`${field} h-36 font-mono text-xs`}
        value={text}
        onChange={(e) => setText(e.target.value)}
        placeholder={'{\n  "mcpServers": {\n    "weather": { "url": "https://example.com/mcp", "headers": { "Authorization": "Bearer …" } }\n  }\n}'}
      />
      <div className="mt-3 flex gap-2">
        <button type="button" disabled={busy || !text.trim()} onClick={submit} className="inline-flex items-center gap-1.5 rounded-full bg-surface-950 px-4 py-2 text-sm font-semibold text-white disabled:opacity-50">
          {busy && <Loader2 className="h-3.5 w-3.5 animate-spin" />}导入
        </button>
        <button type="button" onClick={onCancel} className="rounded-full px-4 py-2 text-sm text-surface-600 hover:bg-surface-100">取消</button>
      </div>
    </div>
  )
}

function CatalogEntryCard({ entry, onInstalled, onToast }: { entry: McpCatalogEntry; onInstalled: () => void; onToast: (m: string, ok?: boolean) => void }) {
  const [open, setOpen] = useState(false)
  const [values, setValues] = useState<Record<string, string>>({})
  const [busy, setBusy] = useState(false)
  const install = async () => {
    setBusy(true)
    try {
      const { data } = await mcpApi.install(entry.id, values)
      const result = (await mcpApi.test(data.id)).data
      onToast(result.ok ? `已添加「${entry.name}」，发现 ${result.tools.length} 个工具` : `已添加「${entry.name}」，但连接失败：${result.error}`, result.ok)
      setOpen(false)
      onInstalled()
    } catch (err) {
      onToast(errorText(err, '添加失败'), false)
    } finally {
      setBusy(false)
    }
  }
  const disabledReason = entry.installed ? '已添加' : !entry.available ? '服务端未开启本地命令' : ''
  return (
    <div className="rounded-2xl border border-black/[0.06] bg-white p-3.5">
      <div className="flex items-start gap-2">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-1.5">
            <span className="text-sm font-semibold text-surface-900">{entry.name}</span>
            <span className="rounded bg-surface-100 px-1.5 text-[10px] text-surface-500">{entry.category}</span>
            {entry.fields.length === 0 && entry.transport !== 'stdio' && <span className="rounded bg-emerald-50 px-1.5 text-[10px] text-emerald-700">免密钥</span>}
          </div>
          <div className="mt-1 text-[11px] leading-5 text-surface-500">{entry.description}</div>
          <a href={entry.docs} target="_blank" rel="noreferrer" className="mt-1 inline-flex items-center gap-0.5 text-[11px] text-surface-400 hover:text-surface-700">
            文档 <ExternalLink className="h-3 w-3" />
          </a>
        </div>
        <button
          type="button"
          disabled={!!disabledReason || busy}
          onClick={() => (entry.fields.length ? setOpen(!open) : install())}
          className="inline-flex shrink-0 items-center gap-1 rounded-full bg-surface-950 px-3 py-1.5 text-xs font-semibold text-white hover:bg-surface-800 disabled:bg-surface-200 disabled:text-surface-500"
        >
          {busy && <Loader2 className="h-3 w-3 animate-spin" />}
          {disabledReason || '添加'}
        </button>
      </div>
      {open && (
        <div className="mt-3 space-y-2 border-t border-surface-100 pt-3">
          {entry.fields.map((f) => (
            <label key={f.key} className="block text-xs font-medium text-surface-600">
              {f.label}
              {f.required && <span className="text-red-500"> *</span>}
              <input
                className={field}
                type={f.secret ? 'password' : 'text'}
                autoComplete="off"
                value={values[f.key] || ''}
                onChange={(e) => setValues({ ...values, [f.key]: e.target.value })}
              />
              {f.help && <span className="mt-1 block text-[11px] font-normal text-surface-400">{f.help}</span>}
            </label>
          ))}
          <div className="flex gap-2">
            <button type="button" disabled={busy} onClick={install} className="inline-flex items-center gap-1 rounded-full bg-surface-950 px-3.5 py-1.5 text-xs font-semibold text-white disabled:opacity-50">
              {busy && <Loader2 className="h-3 w-3 animate-spin" />}添加并连接
            </button>
            <button type="button" onClick={() => setOpen(false)} className="rounded-full px-3 py-1.5 text-xs text-surface-600 hover:bg-surface-100">取消</button>
          </div>
        </div>
      )}
    </div>
  )
}

function Catalog({ onInstalled, onToast, onClose }: { onInstalled: () => void; onToast: (m: string, ok?: boolean) => void; onClose: () => void }) {
  const [entries, setEntries] = useState<McpCatalogEntry[] | null>(null)
  const [category, setCategory] = useState('全部')
  const load = () => mcpApi.catalog().then(({ data }) => setEntries(data.entries)).catch(() => setEntries([]))
  useEffect(() => {
    load()
  }, [])
  const categories = ['全部', ...Array.from(new Set((entries || []).map((e) => e.category)))]
  const visible = (entries || []).filter((e) => category === '全部' || e.category === category)
  return (
    <div className="mb-4 rounded-2xl border border-black/[0.08] bg-surface-50/60 p-3">
      <div className="mb-2 flex items-center justify-between">
        <div className="text-sm font-semibold text-surface-900">插件目录</div>
        <button type="button" onClick={onClose} className="rounded-lg p-1 text-surface-400 hover:bg-white" aria-label="关闭目录">
          <X className="h-4 w-4" />
        </button>
      </div>
      <div className="mb-3 flex flex-wrap gap-1.5">
        {categories.map((c) => (
          <button
            key={c}
            type="button"
            onClick={() => setCategory(c)}
            className={`rounded-full px-2.5 py-1 text-[11px] ${category === c ? 'bg-surface-950 text-white' : 'bg-white text-surface-600 hover:bg-surface-100'}`}
          >
            {c}
          </button>
        ))}
      </div>
      {entries === null ? (
        <div className="flex items-center gap-2 py-4 text-xs text-surface-400"><Loader2 className="h-3.5 w-3.5 animate-spin" />正在加载…</div>
      ) : (
        <div className="grid gap-2 sm:grid-cols-2">
          {visible.map((entry) => (
            <CatalogEntryCard
              key={entry.id}
              entry={entry}
              onToast={onToast}
              onInstalled={() => {
                load()
                onInstalled()
              }}
            />
          ))}
        </div>
      )}
    </div>
  )
}

function ServerCard({ server, onEdit, onChanged, onToast }: { server: McpServer; onEdit: () => void; onChanged: () => void; onToast: (m: string, ok?: boolean) => void }) {
  const [open, setOpen] = useState(false)
  const [tools, setTools] = useState<McpTool[] | null>(null)
  const [toolsError, setToolsError] = useState('')
  const [busy, setBusy] = useState<string | null>(null)
  const state = STATE[server.status?.state] || STATE.idle

  const applyResult = (data: Awaited<ReturnType<typeof mcpApi.tools>>['data']) => {
    if (data.ok) {
      setTools(data.tools)
      setToolsError('')
    } else {
      setTools(null)
      setToolsError(data.error)
    }
    return data
  }

  const run = async (key: string, fn: () => Promise<void>) => {
    setBusy(key)
    try {
      await fn()
    } catch (err) {
      onToast(errorText(err, '操作失败'), false)
    } finally {
      setBusy(null)
    }
  }

  const toggleOpen = () => {
    const next = !open
    setOpen(next)
    if (next && tools === null) run('tools', async () => void applyResult((await mcpApi.tools(server.id)).data))
  }

  const toggleTool = (tool: McpTool) =>
    run(`tool-${tool.name}`, async () => {
      const disabled = new Set(server.disabled_tools)
      if (tool.enabled) disabled.add(tool.name)
      else disabled.delete(tool.name)
      await mcpApi.setDisabledTools(server.id, [...disabled])
      setTools((list) => list?.map((t) => (t.name === tool.name ? { ...t, enabled: !t.enabled } : t)) ?? null)
      onChanged()
    })

  const enabledCount = tools?.filter((t) => t.enabled).length

  return (
    <div className={`mb-3 rounded-2xl border border-black/[0.06] bg-white p-4 ${server.enabled ? '' : 'opacity-70'}`}>
      <div className="flex items-start gap-3">
        <div className="mt-0.5 rounded-xl bg-violet-50 p-2 text-violet-600">
          <Puzzle className="h-4 w-4" />
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-sm font-semibold text-surface-900">{server.name}</span>
            <span className="inline-flex items-center gap-1.5 rounded-full bg-surface-50 px-2 py-0.5 text-[11px] text-surface-600">
              <span className={`h-1.5 w-1.5 rounded-full ${state.dot}`} />
              {state.label}
              {server.status?.tool_count !== undefined && ` · ${server.status.tool_count} 个工具`}
            </span>
          </div>
          <div className="mt-0.5 truncate text-[11px] text-surface-400">
            <code>mcp__{server.slug}__*</code> · {server.approval === 'never' ? '无需确认' : server.approval === 'always' ? '每次确认' : '写操作需确认'} · {server.transport === 'stdio' ? `本地命令 ${server.command} ${server.args.join(' ')}` : `${server.transport === 'sse' ? 'SSE' : 'HTTP'} ${server.url}`}
          </div>
          {server.description && <div className="mt-1 text-[11px] text-surface-500">{server.description}</div>}
          {(server.status?.error || server.secrets_broken) && (
            <div className="mt-1.5 break-all rounded-lg bg-red-50 px-2 py-1 text-[11px] text-red-700">{server.secrets_broken ? '密钥无法解密，请重新填写请求头或环境变量' : server.status.error}</div>
          )}
        </div>
        <label className="flex shrink-0 items-center gap-1.5 text-xs text-surface-600">
          <input type="checkbox" checked={server.enabled} onChange={(e) => run('toggle', async () => { await mcpApi.update(server.id, serverToInput(server, e.target.checked)); onChanged() })} />
          启用
        </label>
      </div>

      {open && (
        <div className="mt-3 rounded-xl bg-surface-50 p-2">
          {busy === 'tools' && tools === null && (
            <div className="flex items-center gap-2 px-1 py-2 text-[11px] text-surface-400"><Loader2 className="h-3 w-3 animate-spin" />正在连接…</div>
          )}
          {toolsError && <div className="px-1 py-1 text-[11px] text-red-600">{toolsError}</div>}
          {tools?.length === 0 && <div className="px-1 py-1 text-[11px] text-surface-400">这个服务没有提供工具</div>}
          {tools?.map((tool) => (
            <label key={tool.name} className="flex cursor-pointer items-start gap-2 rounded-lg px-1.5 py-1.5 hover:bg-white">
              <input type="checkbox" className="mt-0.5" checked={tool.enabled} disabled={busy === `tool-${tool.name}`} onChange={() => toggleTool(tool)} />
              <div className="min-w-0 flex-1">
                <div className="flex flex-wrap items-center gap-1.5 text-xs font-medium text-surface-800">
                  {tool.title || tool.name}
                  {tool.read_only ? (
                    <span className="rounded bg-emerald-50 px-1 text-[10px] font-normal text-emerald-700">只读</span>
                  ) : (
                    <span className="rounded bg-amber-50 px-1 text-[10px] font-normal text-amber-700">可能修改数据 · 仅执行模式</span>
                  )}
                </div>
                {tool.description && <div className="line-clamp-2 text-[11px] text-surface-500">{tool.description}</div>}
              </div>
            </label>
          ))}
        </div>
      )}

      <div className="mt-3 flex flex-wrap items-center justify-end gap-1.5">
        {open && enabledCount !== undefined && <span className="mr-auto text-[11px] text-surface-400">启用 {enabledCount} / {tools?.length} 个工具</span>}
        <button type="button" onClick={toggleOpen} className="inline-flex items-center gap-1 rounded-lg px-2.5 py-1.5 text-xs text-surface-700 hover:bg-surface-100">
          工具
          <ChevronDown className={`h-3 w-3 transition-transform ${open ? 'rotate-180' : ''}`} />
        </button>
        <button
          type="button"
          disabled={busy === 'test'}
          onClick={() =>
            run('test', async () => {
              const data = applyResult((await mcpApi.test(server.id)).data)
              setOpen(true)
              onToast(data.ok ? `连接正常，${data.tools.length} 个工具` : `连接失败：${data.error}`, data.ok)
              onChanged()
            })
          }
          className="inline-flex items-center gap-1 rounded-lg px-2.5 py-1.5 text-xs text-surface-700 hover:bg-surface-100 disabled:opacity-50"
        >
          {busy === 'test' ? <Loader2 className="h-3 w-3 animate-spin" /> : <RefreshCw className="h-3 w-3" />}测试连接
        </button>
        <button type="button" onClick={onEdit} className="rounded-lg px-2.5 py-1.5 text-xs text-surface-700 hover:bg-surface-100">编辑</button>
        <button
          type="button"
          onClick={() => {
            if (!window.confirm(`删除插件「${server.name}」？`)) return
            run('remove', async () => {
              await mcpApi.remove(server.id)
              onChanged()
            })
          }}
          className="rounded-lg p-1.5 text-surface-400 hover:bg-red-50 hover:text-red-600"
          title="删除"
        >
          <Trash2 className="h-3.5 w-3.5" />
        </button>
      </div>
    </div>
  )
}

/** 设置 → 插件（MCP）：接入 MCP 服务器，它的工具会出现在 Agent 的工具箱里 */
export function McpPluginsPanel() {
  const [servers, setServers] = useState<McpServer[] | null>(null)
  const [caps, setCaps] = useState<{ stdio: boolean; max_tools_per_run: number } | null>(null)
  const [draft, setDraft] = useState<Draft | null>(null)
  const [importing, setImporting] = useState(false)
  const [browsing, setBrowsing] = useState(false)
  const [saving, setSaving] = useState(false)
  const [toast, setToast] = useState<{ text: string; ok: boolean } | null>(null)

  const reload = () => mcpApi.list().then(({ data }) => setServers(data.servers)).catch(() => setServers([]))

  useEffect(() => {
    reload()
    mcpApi.capabilities().then(({ data }) => setCaps(data)).catch(() => setCaps({ stdio: false, max_tools_per_run: 40 }))
  }, [])

  const showToast = (text: string, ok = true) => {
    setToast({ text, ok })
    window.setTimeout(() => setToast(null), 4000)
  }

  const submit = async () => {
    if (!draft) return
    setSaving(true)
    try {
      const input = toInput(draft)
      const { data } = draft.id ? await mcpApi.update(draft.id, input) : await mcpApi.create(input)
      setDraft(null)
      // 保存后立刻连一次，让状态和工具数准确
      const result = (await mcpApi.test(data.id)).data
      showToast(result.ok ? `已保存，发现 ${result.tools.length} 个工具` : `已保存，但连接失败：${result.error}`, result.ok)
      reload()
    } catch (err) {
      showToast(errorText(err, '保存失败'), false)
    } finally {
      setSaving(false)
    }
  }

  return (
    <div>
      <div className="mb-5 flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <h2 className="text-xl font-bold tracking-tight text-surface-950">插件（MCP）</h2>
          <p className="mt-1 text-sm text-surface-500">
            接入 MCP 服务后，它提供的工具会自动出现在 Agent 的工具箱里（网页、飞书、定时任务都能用）。只读工具在规划 / 问答模式也可用，可能修改数据的工具只在执行模式开放。
          </p>
        </div>
        {!draft && !importing && (
          <div className="flex shrink-0 flex-wrap gap-2">
            <button onClick={() => setBrowsing(!browsing)} className="inline-flex items-center gap-1.5 rounded-full border border-black/10 bg-white px-3.5 py-2 text-sm font-semibold text-surface-700 hover:bg-surface-50">
              <LayoutGrid className="h-4 w-4" />插件目录
            </button>
            <button onClick={() => setImporting(true)} className="inline-flex items-center gap-1.5 rounded-full border border-black/10 bg-white px-3.5 py-2 text-sm font-semibold text-surface-700 hover:bg-surface-50">
              <FileJson className="h-4 w-4" />导入配置
            </button>
            <button onClick={() => setDraft(emptyDraft())} className="inline-flex items-center gap-1.5 rounded-full bg-surface-950 px-4 py-2 text-sm font-semibold text-white hover:bg-surface-800">
              <Plus className="h-4 w-4" />添加插件
            </button>
          </div>
        )}
      </div>

      {toast && <div className={`mb-3 rounded-xl px-3 py-2 text-xs ${toast.ok ? 'bg-emerald-50 text-emerald-800' : 'bg-red-50 text-red-700'}`}>{toast.text}</div>}

      {browsing && !draft && !importing && <Catalog onInstalled={reload} onToast={showToast} onClose={() => setBrowsing(false)} />}

      {importing && (
        <ImportBox
          onCancel={() => setImporting(false)}
          onDone={(msg, ok) => {
            showToast(msg, ok)
            setImporting(false)
            reload()
          }}
        />
      )}
      {draft && <ServerForm draft={draft} onChange={setDraft} onSubmit={submit} onCancel={() => setDraft(null)} saving={saving} stdioAllowed={!!caps?.stdio} />}

      {servers === null && (
        <div className="flex items-center gap-2 py-6 text-xs text-surface-400"><Loader2 className="h-3.5 w-3.5 animate-spin" />正在加载…</div>
      )}
      {servers?.length === 0 && !draft && !importing && (
        <div className="rounded-2xl border border-dashed border-black/[0.1] py-10 text-center text-sm text-surface-400">还没有接入插件</div>
      )}
      {servers?.map((server) => (
        <ServerCard key={server.id} server={server} onToast={showToast} onChanged={reload} onEdit={() => setDraft(fromServer(server))} />
      ))}
      {caps && <div className="mt-2 text-[11px] text-surface-400">每次运行最多加载 {caps.max_tools_per_run} 个插件工具，可以在“工具”里关掉不常用的。</div>}
    </div>
  )
}
