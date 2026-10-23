import { useEffect, useState } from 'react'
import { Check, Copy, Download, Loader2, PenLine, Terminal } from 'lucide-react'
import { editorApi, integrationsApi, type EditorAiSync, type EditorStatus, type IntegrationsOverview } from '@/api'

function errorText(err: any, fallback: string) {
  return err?.response?.data?.detail?.replace(/^参数错误:\s*/, '') || fallback
}

/** 一段可复制的命令 / 配置 */
function Snippet({ label, text }: { label: string; text: string }) {
  const [copied, setCopied] = useState(false)
  const copy = async () => {
    await navigator.clipboard.writeText(text)
    setCopied(true)
    setTimeout(() => setCopied(false), 1500)
  }
  return (
    <div className="mt-3">
      <div className="mb-1 text-xs font-medium text-surface-600">{label}</div>
      <div className="relative rounded-xl bg-surface-950 px-3 py-2.5 pr-10 font-mono text-xs leading-relaxed text-surface-100">
        <pre className="whitespace-pre-wrap break-all">{text}</pre>
        <button type="button" onClick={copy} className="absolute right-2 top-2 rounded-lg p-1.5 text-surface-400 hover:bg-white/10 hover:text-white" title="复制">
          {copied ? <Check className="h-3.5 w-3.5" /> : <Copy className="h-3.5 w-3.5" />}
        </button>
      </div>
    </div>
  )
}

/** 设置 → 集成：把 officepilot skill 装进编程 Agent，MCP 接入方式，命令行安装到 PATH */
export function IntegrationsPanel() {
  const [data, setData] = useState<IntegrationsOverview | null>(null)
  const [editor, setEditor] = useState<EditorStatus | null>(null)
  const [aiSync, setAiSync] = useState<EditorAiSync | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null)

  const load = async () => {
    try {
      const { data } = await integrationsApi.overview()
      setData(data)
    } catch (err) {
      setNote({ ok: false, text: errorText(err, '读取集成状态失败') })
    }
  }

  useEffect(() => {
    load()
    editorApi.status().then(({ data }) => setEditor(data)).catch(() => {})
    editorApi.aiSync().then(({ data }) => setAiSync(data)).catch(() => {})
  }, [])

  const act = async (key: string, run: () => Promise<string | null>) => {
    setBusy(key)
    setNote(null)
    try {
      const text = await run()
      if (text) setNote({ ok: true, text })
      await load()
    } catch (err) {
      setNote({ ok: false, text: errorText(err, '操作失败') })
    } finally {
      setBusy(null)
    }
  }

  const download = async () => {
    const { data: blob } = await integrationsApi.skillZip()
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = 'officepilot-skill.zip'
    a.click()
    URL.revokeObjectURL(url)
  }

  const link = () =>
    act('link', async () => {
      let { data: r } = await integrationsApi.linkCli()
      if (r.exists) {
        if (!window.confirm(`${r.path} 已经存在，要替换成 OfficePilot 附带的命令行吗？`)) return null
        r = (await integrationsApi.linkCli(true)).data
      }
      return r.on_path === false ? `已安装到 ${r.path}。这个目录不在 PATH 里，请把它加入 shell 配置` : `已安装到 ${r.path}，在终端里可以直接运行 officepilot`
    })

  if (!data) {
    return <div className="flex items-center gap-2 text-sm text-surface-500"><Loader2 className="h-4 w-4 animate-spin" />正在读取…</div>
  }

  return (
    <div className="space-y-6">
      <div>
        <div className="text-base font-bold text-surface-900">集成</div>
        <div className="mt-1 text-xs leading-relaxed text-surface-500">让 Claude Code、Codex、Cursor、Gemini CLI 等编程 Agent 直接创建和修改真实的 Word / Excel / PPT 文件。它们在本机调用 officepilot，文件不经过网络。</div>
      </div>

      {note && <div className={`rounded-xl px-3 py-2 text-xs ${note.ok ? 'bg-emerald-50 text-emerald-800' : 'bg-red-50 text-red-700'}`}>{note.text}</div>}

      <section>
        <div className="flex items-center justify-between">
          <div className="text-sm font-semibold text-surface-900">安装 Skill</div>
          <button type="button" onClick={download} className="inline-flex items-center gap-1.5 rounded-full border border-black/10 px-3 py-1.5 text-xs text-surface-700 hover:bg-surface-50">
            <Download className="h-3.5 w-3.5" />下载压缩包
          </button>
        </div>
        {!data.local && <div className="mt-2 text-xs text-surface-500">网页版不能写入你电脑上的目录。下载压缩包后解压到 Agent 的 skills 目录，例如 ~/.claude/skills/。</div>}
        {data.local && (
          <div className="mt-3 divide-y divide-black/[0.05] rounded-2xl border border-black/[0.06] bg-white">
            {data.agents.map((a) => (
              <div key={a.id} className="flex items-center gap-3 px-4 py-3">
                <div className="min-w-0 flex-1">
                  <div className="text-sm font-medium text-surface-900">{a.name}</div>
                  <div className="truncate text-xs text-surface-500" title={a.dir || ''}>
                    {a.foreign ? '已有同名 skill（不是 OfficePilot 安装的，不会覆盖）' : a.installed ? (a.up_to_date ? '已安装，最新版' : '已安装，有新版本') : a.detected ? '已检测到，未安装' : '未检测到这个 Agent'}
                    {a.dir ? ` · ${a.dir}` : ''}
                  </div>
                </div>
                {a.installed && (
                  <button type="button" disabled={!!busy} onClick={() => act(a.id + ':rm', async () => { await integrationsApi.uninstall(a.id); return `已从 ${a.name} 移除` })} className="text-xs text-surface-400 hover:text-red-600 disabled:opacity-50">移除</button>
                )}
                {!a.foreign && (!a.installed || !a.up_to_date) && (
                  <button
                    type="button"
                    disabled={!!busy}
                    onClick={() => act(a.id, async () => { const { data: r } = await integrationsApi.install(a.id); return `已安装到 ${r.dir}，新开一个 ${a.name} 会话即可使用` })}
                    className="inline-flex items-center gap-1.5 rounded-full bg-surface-950 px-3 py-1.5 text-xs font-semibold text-white hover:bg-surface-800 disabled:opacity-50"
                  >
                    {busy === a.id && <Loader2 className="h-3 w-3 animate-spin" />}{a.installed ? '更新' : '安装'}
                  </button>
                )}
              </div>
            ))}
          </div>
        )}
      </section>

      <section>
        <div className="text-sm font-semibold text-surface-900">MCP 接入</div>
        <div className="mt-1 text-xs text-surface-500">不能运行命令行的助手（Claude Desktop、Cursor 等）通过 MCP 使用同样的能力，不需要安装 skill。</div>
        <Snippet label="Claude Code" text={data.mcp.claude_code} />
        <Snippet label="Cursor / Claude Desktop / 其它 MCP 客户端" text={data.mcp.json} />
        <Snippet label="给其它机器或容器用（HTTP，务必设置令牌）" text={data.mcp.http} />
      </section>

      <section>
        <div className="text-sm font-semibold text-surface-900">命令行</div>
        {data.cli.path ? (
          <div className="mt-2 flex items-center gap-3 rounded-2xl border border-black/[0.06] bg-white px-4 py-3">
            <Terminal className="h-4 w-4 shrink-0 text-surface-500" />
            <div className="min-w-0 flex-1">
              <div className="truncate font-mono text-xs text-surface-800" title={data.cli.path}>{data.cli.path}</div>
              <div className="text-xs text-surface-500">{data.cli.linked ? `已安装到 ${data.cli.link}` : '在终端里运行 officepilot --help 查看用法'}</div>
            </div>
            {data.local && !data.cli.linked && (
              <button type="button" disabled={!!busy} onClick={link} className="inline-flex items-center gap-1.5 rounded-full border border-black/10 px-3 py-1.5 text-xs text-surface-700 hover:bg-surface-50 disabled:opacity-50">
                {busy === 'link' && <Loader2 className="h-3 w-3 animate-spin" />}安装到 PATH
              </button>
            )}
            {data.local && data.cli.linked && (
              <button type="button" disabled={!!busy} onClick={() => act('unlink', async () => { await integrationsApi.unlinkCli(); return '已从 PATH 移除' })} className="text-xs text-surface-400 hover:text-red-600">移除</button>
            )}
          </div>
        ) : (
          <div className="mt-2 text-xs text-surface-500">本机没有找到 officepilot。从源码安装：<code className="rounded bg-surface-100 px-1">cargo install --path cli</code></div>
        )}
      </section>

      <section>
        <div className="text-sm font-semibold text-surface-900">编辑器套件</div>
        <div className="mt-1 text-xs text-surface-500">在「我的文件」里点「用编辑器打开」，用编辑器编辑 Word / Excel / PPT / PDF / Markdown / HTML。保存后自动同步回来，可以撤销。</div>
        {!editor?.enabled ? (
          <div className="mt-2 text-xs text-surface-500">需要 OfficePilot 桌面版。</div>
        ) : editor.app ? (
          <div className="mt-2 flex items-center gap-3 rounded-2xl border border-black/[0.06] bg-white px-4 py-3">
            <PenLine className="h-4 w-4 shrink-0 text-surface-500" />
            <div className="min-w-0 flex-1">
              <div className="truncate font-mono text-xs text-surface-800" title={editor.app}>{editor.app}</div>
              <div className="text-xs text-surface-500">{editor.open.length ? `${editor.open.length} 个文件正在用编辑器编辑${editor.open.some((f) => f.dirty) ? '，有修改正在同步' : ''}` : '已就绪'}</div>
            </div>
          </div>
        ) : null}
        {editor?.enabled && editor.app && editor.agent_tools.length > 0 && (
          <div className="mt-2 text-xs leading-relaxed text-surface-500">智能助手也能使用编辑器套件：{editor.agent_tools.join('、')}。转换和填写模板的结果另存为新文件，可以撤销。</div>
        )}
        {aiSync?.available && (
          <div className="mt-3 flex items-start gap-3 rounded-2xl border border-black/[0.06] bg-white px-4 py-3">
            <div className="min-w-0 flex-1">
              <div className="text-sm font-medium text-surface-900">编辑器套件使用 OfficePilot 的模型</div>
              <div className="mt-0.5 text-xs leading-relaxed text-surface-500">
                {aiSync.enabled
                  ? `已同步：${aiSync.profile_name ?? ''} · ${aiSync.model ?? ''}。修改模型设置后自动更新。`
                  : '开启后，编辑器里的 AI 助手直接使用这里配置的模型服务，不用再填一遍。'}
                密钥不会写进编辑器套件的设置文件，只在从 OfficePilot 打开编辑器时交给它；直接从访达或程序坞打开的编辑器拿不到密钥。
              </div>
            </div>
            <button
              type="button"
              role="switch"
              aria-checked={aiSync.enabled}
              aria-label="编辑器套件使用 OfficePilot 的模型"
              disabled={!!busy}
              onClick={() =>
                act('ai-sync', async () => {
                  const { data } = await editorApi.setAiSync(!aiSync.enabled)
                  setAiSync(data)
                  return data.enabled ? '已开启。下次从 OfficePilot 打开编辑器时生效' : '已关闭，编辑器套件恢复使用原来的模型设置'
                })
              }
              className={`relative mt-0.5 h-5 w-9 shrink-0 rounded-full transition disabled:opacity-50 ${aiSync.enabled ? 'bg-surface-950' : 'bg-surface-200'}`}
            >
              <span className={`absolute top-0.5 h-4 w-4 rounded-full bg-white shadow transition ${aiSync.enabled ? 'left-[18px]' : 'left-0.5'}`} />
            </button>
          </div>
        )}
        {!editor?.enabled || editor.app ? null : (
          <div className="mt-2 text-xs text-surface-500">没有找到编辑器套件。从源码构建：<code className="rounded bg-surface-100 px-1">scripts/build-suite.sh</code></div>
        )}
      </section>
    </div>
  )
}
