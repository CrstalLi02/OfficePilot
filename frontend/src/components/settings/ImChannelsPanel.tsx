import { useEffect, useState } from 'react'
import { ChevronDown, Copy, Link2, Loader2, MessageSquare, Plus, RefreshCw, Trash2, UserMinus } from 'lucide-react'
import { browserTimezone, imApi, type ImBinding, type ImChannel, type ImChannelInput } from '@/api'

const STATE: Record<string, { label: string; dot: string }> = {
  connected: { label: '已连接', dot: 'bg-emerald-500' },
  connecting: { label: '连接中', dot: 'bg-amber-400 animate-pulse' },
  webhook: { label: '等待事件回调', dot: 'bg-sky-500' },
  error: { label: '连接失败', dot: 'bg-red-500' },
  disabled: { label: '已停用', dot: 'bg-surface-300' },
  stopped: { label: '未连接', dot: 'bg-surface-300' },
}

const MODES = [
  ['craft', '执行'],
  ['plan', '规划'],
  ['ask', '问答'],
] as const

interface Draft extends ImChannelInput {
  id?: string
}

const emptyDraft = (): Draft => ({
  platform: 'feishu',
  name: '飞书机器人',
  config: { app_id: '', domain: 'feishu', connection: 'ws', default_mode: 'craft', timezone: browserTimezone() || 'Asia/Shanghai' },
  app_secret: '',
  verification_token: '',
  encrypt_key: '',
  enabled: true,
})

function when(iso?: string | null) {
  if (!iso) return ''
  const d = new Date(iso)
  return Number.isNaN(d.getTime()) ? '' : d.toLocaleString('zh-CN', { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit' })
}

const errorText = (err: any, fallback: string) => err?.response?.data?.detail || err?.message || fallback

const input = 'mt-1.5 w-full rounded-xl border border-black/[0.08] bg-white px-3 py-2 text-sm outline-none focus:border-surface-400'

function Guide() {
  const [open, setOpen] = useState(false)
  return (
    <div className="mb-4 rounded-2xl border border-black/[0.06] bg-white/70">
      <button type="button" onClick={() => setOpen(!open)} className="flex w-full items-center justify-between px-4 py-3 text-left text-sm font-semibold text-surface-800">
        如何在飞书开放平台创建机器人
        <ChevronDown className={`h-4 w-4 transition-transform ${open ? 'rotate-180' : ''}`} />
      </button>
      {open && (
        <ol className="list-decimal space-y-1.5 px-8 pb-4 text-xs leading-5 text-surface-600">
          <li>在飞书开放平台创建「企业自建应用」，在「添加应用能力」里添加「机器人」。</li>
          <li>
            「权限管理」开通：<code>im:message</code>、<code>im:message:send_as_bot</code>、<code>im:message.p2p_msg:readonly</code>、
            <code>im:message.group_at_msg:readonly</code>、<code>im:resource</code>。
          </li>
          <li>把「凭证与基础信息」里的 App ID、App Secret 填到下面并保存，等状态变成「已连接」。</li>
          <li>
            「事件与回调」→ 订阅方式选「使用长连接接收事件」（必须先在这里连上才能保存），然后添加事件
            <code>im.message.receive_v1</code>（接收消息）。
          </li>
          <li>创建版本并发布，审核通过后在飞书里搜索机器人。</li>
          <li>点「生成绑定码」，在飞书里给机器人发送「绑定 123456」，之后就能直接让它干活。</li>
        </ol>
      )}
    </div>
  )
}

function ChannelForm({ draft, onChange, onSubmit, onCancel, saving }: { draft: Draft; onChange: (d: Draft) => void; onSubmit: () => void; onCancel: () => void; saving: boolean }) {
  const editing = Boolean(draft.id)
  const original = draft as Draft & Partial<ImChannel>
  const setConfig = (patch: Partial<Draft['config']>) => onChange({ ...draft, config: { ...draft.config, ...patch } })
  const secretHint = (has?: boolean) => (editing && has ? '已填写，留空不修改' : '')
  const callbackUrl = draft.id ? `${window.location.origin}/api/im/feishu/${draft.id}/event` : ''
  return (
    <div className="mb-4 rounded-2xl border border-black/[0.08] bg-white p-4">
      <div className="mb-3 text-sm font-semibold text-surface-900">{editing ? '编辑机器人' : '添加飞书机器人'}</div>
      <div className="grid gap-3 sm:grid-cols-2">
        <label className="text-xs font-medium text-surface-600">
          名称
          <input className={input} value={draft.name} onChange={(e) => onChange({ ...draft, name: e.target.value })} />
        </label>
        <label className="text-xs font-medium text-surface-600">
          版本
          <select className={input} value={draft.config.domain} onChange={(e) => setConfig({ domain: e.target.value as 'feishu' | 'lark' })}>
            <option value="feishu">飞书（open.feishu.cn）</option>
            <option value="lark">Lark 国际版（open.larksuite.com）</option>
          </select>
        </label>
        <label className="text-xs font-medium text-surface-600">
          App ID
          <input className={input} value={draft.config.app_id} placeholder="cli_xxx" onChange={(e) => setConfig({ app_id: e.target.value })} />
        </label>
        <label className="text-xs font-medium text-surface-600">
          App Secret
          <input
            className={input}
            type="password"
            autoComplete="off"
            value={draft.app_secret || ''}
            placeholder={secretHint(original.has_app_secret)}
            onChange={(e) => onChange({ ...draft, app_secret: e.target.value })}
          />
        </label>
      </div>

      <div className="mt-3 text-xs font-medium text-surface-600">接收消息的方式</div>
      <div className="mt-1.5 grid gap-2 sm:grid-cols-2">
        {([
          ['ws', '长连接（推荐）', '不需要公网地址，本机运行也能用'],
          ['webhook', '事件回调 URL', '需要飞书能访问到本服务的公网 HTTPS 地址'],
        ] as const).map(([value, label, hint]) => (
          <button
            key={value}
            type="button"
            onClick={() => setConfig({ connection: value })}
            className={`rounded-xl border px-3 py-2 text-left ${draft.config.connection === value ? 'border-surface-900 bg-surface-50' : 'border-black/[0.08] hover:border-surface-300'}`}
          >
            <div className="text-xs font-semibold text-surface-900">{label}</div>
            <div className="mt-0.5 text-[11px] text-surface-500">{hint}</div>
          </button>
        ))}
      </div>

      {draft.config.connection === 'webhook' && (
        <div className="mt-3 grid gap-3 sm:grid-cols-2">
          <label className="text-xs font-medium text-surface-600">
            Verification Token
            <input
              className={input}
              type="password"
              autoComplete="off"
              value={draft.verification_token || ''}
              placeholder={secretHint(original.has_verification_token)}
              onChange={(e) => onChange({ ...draft, verification_token: e.target.value })}
            />
          </label>
          <label className="text-xs font-medium text-surface-600">
            Encrypt Key（可选）
            <input
              className={input}
              type="password"
              autoComplete="off"
              value={draft.encrypt_key || ''}
              placeholder={secretHint(original.has_encrypt_key)}
              onChange={(e) => onChange({ ...draft, encrypt_key: e.target.value })}
            />
          </label>
          <div className="text-[11px] text-surface-500 sm:col-span-2">
            请求地址：{callbackUrl ? <code className="select-all break-all">{callbackUrl}</code> : '保存后显示'}
          </div>
        </div>
      )}

      <div className="mt-3 grid gap-3 sm:grid-cols-2">
        <label className="text-xs font-medium text-surface-600">
          新对话默认模式
          <select className={input} value={draft.config.default_mode} onChange={(e) => setConfig({ default_mode: e.target.value as Draft['config']['default_mode'] })}>
            {MODES.map(([value, label]) => (
              <option key={value} value={value}>{label}</option>
            ))}
          </select>
        </label>
        <label className="text-xs font-medium text-surface-600">
          时区
          <input className={input} value={draft.config.timezone} onChange={(e) => setConfig({ timezone: e.target.value })} />
        </label>
      </div>

      <div className="mt-4 flex items-center gap-2">
        <button type="button" disabled={saving} onClick={onSubmit} className="inline-flex items-center gap-1.5 rounded-full bg-surface-950 px-4 py-2 text-sm font-semibold text-white hover:bg-surface-800 disabled:opacity-50">
          {saving && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
          {editing ? '保存' : '保存并连接'}
        </button>
        <button type="button" onClick={onCancel} className="rounded-full px-4 py-2 text-sm text-surface-600 hover:bg-surface-100">取消</button>
      </div>
    </div>
  )
}

function ChannelCard({ channel, onEdit, onChanged, onToast }: { channel: ImChannel; onEdit: () => void; onChanged: () => void; onToast: (m: string, ok?: boolean) => void }) {
  const [bindings, setBindings] = useState<ImBinding[] | null>(null)
  const [code, setCode] = useState<{ code: string; expires: number } | null>(null)
  const [now, setNow] = useState(Date.now())
  const [busy, setBusy] = useState<string | null>(null)
  const state = STATE[channel.status?.state] || STATE.stopped

  const loadBindings = () => imApi.bindings(channel.id).then(({ data }) => setBindings(data.bindings)).catch(() => setBindings([]))

  useEffect(() => {
    loadBindings()
  }, [channel.id, channel.status?.last_event_at])

  useEffect(() => {
    if (!code) return
    const timer = window.setInterval(() => setNow(Date.now()), 1000)
    return () => window.clearInterval(timer)
  }, [code])

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

  const left = code ? Math.max(0, Math.round((code.expires - now) / 1000)) : 0
  const toInput = (enabled: boolean): ImChannelInput => ({ name: channel.name, config: channel.config, enabled })

  return (
    <div className="mb-3 rounded-2xl border border-black/[0.06] bg-white p-4">
      <div className="flex items-start gap-3">
        <div className="mt-0.5 rounded-xl bg-sky-50 p-2 text-sky-600">
          <MessageSquare className="h-4 w-4" />
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-sm font-semibold text-surface-900">{channel.name}</span>
            <span className="inline-flex items-center gap-1.5 rounded-full bg-surface-50 px-2 py-0.5 text-[11px] text-surface-600">
              <span className={`h-1.5 w-1.5 rounded-full ${state.dot}`} />
              {state.label}
            </span>
            {channel.status?.bot_name && <span className="text-[11px] text-surface-400">机器人：{channel.status.bot_name}</span>}
          </div>
          <div className="mt-0.5 text-[11px] text-surface-400">
            {channel.config.domain === 'lark' ? 'Lark' : '飞书'} · {channel.config.connection === 'ws' ? '长连接' : '事件回调'} · App ID {channel.config.app_id}
            {channel.status?.last_event_at && ` · 最近收到消息 ${when(channel.status.last_event_at)}`}
          </div>
          {(channel.status?.error || channel.secrets_broken) && (
            <div className="mt-1.5 rounded-lg bg-red-50 px-2 py-1 text-[11px] text-red-700">{channel.status?.error || '密钥无法解密，请重新填写 App Secret'}</div>
          )}
        </div>
        <label className="flex shrink-0 items-center gap-1.5 text-xs text-surface-600">
          <input
            type="checkbox"
            checked={channel.enabled}
            onChange={(e) => run('toggle', async () => {
              await imApi.update(channel.id, toInput(e.target.checked))
              onChanged()
            })}
          />
          启用
        </label>
      </div>

      {code && left > 0 && (
        <div className="mt-3 rounded-xl bg-amber-50 px-3 py-2.5">
          <div className="text-xs text-amber-800">在飞书里给机器人发送（{Math.floor(left / 60)}:{String(left % 60).padStart(2, '0')} 内有效）：</div>
          <div className="mt-1 flex items-center gap-2">
            <code className="select-all text-lg font-bold tracking-wider text-amber-900">绑定 {code.code}</code>
            <button
              type="button"
              title="复制"
              onClick={() => navigator.clipboard?.writeText(`绑定 ${code.code}`).then(() => onToast('已复制'))}
              className="rounded-lg p-1 text-amber-700 hover:bg-amber-100"
            >
              <Copy className="h-3.5 w-3.5" />
            </button>
          </div>
        </div>
      )}

      <div className="mt-3">
        <div className="text-[11px] font-medium text-surface-500">已绑定的飞书账号</div>
        {bindings === null ? (
          <div className="mt-1 text-[11px] text-surface-400">加载中…</div>
        ) : bindings.length === 0 ? (
          <div className="mt-1 text-[11px] text-surface-400">还没有，生成绑定码后在飞书里发给机器人</div>
        ) : (
          <div className="mt-1 space-y-1">
            {bindings.map((b) => (
              <div key={b.id} className="flex items-center gap-2 text-[11px] text-surface-600">
                <span className="truncate">{b.display_name || b.im_user_id}</span>
                <span className="text-surface-400">{when(b.created_at)}</span>
                <button
                  type="button"
                  title="解绑"
                  onClick={() => run(`unbind-${b.id}`, async () => {
                    await imApi.unbind(channel.id, b.id)
                    await loadBindings()
                  })}
                  className="ml-auto rounded p-0.5 text-surface-400 hover:bg-surface-100 hover:text-red-600"
                >
                  <UserMinus className="h-3.5 w-3.5" />
                </button>
              </div>
            ))}
          </div>
        )}
      </div>

      <div className="mt-3 flex flex-wrap justify-end gap-1.5">
        <button
          type="button"
          disabled={busy === 'test'}
          onClick={() => run('test', async () => {
            const { data } = await imApi.test(channel.id)
            onToast(data.ok ? `连接正常：${data.bot_name}` : `测试失败：${data.error}`, data.ok)
            onChanged()
          })}
          className="inline-flex items-center gap-1 rounded-lg px-2.5 py-1.5 text-xs text-surface-700 hover:bg-surface-100 disabled:opacity-50"
        >
          {busy === 'test' ? <Loader2 className="h-3 w-3 animate-spin" /> : <RefreshCw className="h-3 w-3" />}测试连接
        </button>
        <button
          type="button"
          disabled={!channel.enabled}
          onClick={() => run('code', async () => {
            const { data } = await imApi.bindCode(channel.id)
            setCode({ code: data.code, expires: Date.now() + data.expires_in * 1000 })
          })}
          className="inline-flex items-center gap-1 rounded-lg px-2.5 py-1.5 text-xs text-surface-700 hover:bg-surface-100 disabled:opacity-50"
        >
          <Link2 className="h-3 w-3" />生成绑定码
        </button>
        <button type="button" onClick={onEdit} className="rounded-lg px-2.5 py-1.5 text-xs text-surface-700 hover:bg-surface-100">编辑</button>
        <button
          type="button"
          onClick={() => {
            if (!window.confirm(`删除「${channel.name}」？已绑定的账号也会一起删除。`)) return
            run('remove', async () => {
              await imApi.remove(channel.id)
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

/** 设置 → IM 接入：飞书机器人的配置、连接状态与账号绑定 */
export function ImChannelsPanel() {
  const [channels, setChannels] = useState<ImChannel[] | null>(null)
  const [draft, setDraft] = useState<Draft | null>(null)
  const [saving, setSaving] = useState(false)
  const [toast, setToast] = useState<{ text: string; ok: boolean } | null>(null)

  const reload = () => imApi.list().then(({ data }) => setChannels(data.channels)).catch(() => setChannels([]))

  useEffect(() => {
    reload()
    // 连接状态在后台变化，定时刷新
    const timer = window.setInterval(reload, 3000)
    return () => window.clearInterval(timer)
  }, [])

  const showToast = (text: string, ok = true) => {
    setToast({ text, ok })
    window.setTimeout(() => setToast(null), 3500)
  }

  const submit = async () => {
    if (!draft) return
    setSaving(true)
    try {
      const { id, ...body } = draft
      if (id) await imApi.update(id, body)
      else await imApi.create(body)
      setDraft(null)
      showToast(id ? '已保存' : '已添加，正在连接飞书')
      reload()
    } catch (err) {
      showToast(errorText(err, '保存失败'), false)
    } finally {
      setSaving(false)
    }
  }

  return (
    <div>
      <div className="mb-5 flex items-start justify-between gap-4">
        <div>
          <h2 className="text-xl font-bold tracking-tight text-surface-950">IM 接入</h2>
          <p className="mt-1 text-sm text-surface-500">在飞书里给机器人发消息就能让 Agent 干活，进度和生成的文件会发回聊天里。企业微信即将支持。</p>
        </div>
        {!draft && (
          <button onClick={() => setDraft(emptyDraft())} className="inline-flex shrink-0 items-center gap-1.5 rounded-full bg-surface-950 px-4 py-2 text-sm font-semibold text-white hover:bg-surface-800">
            <Plus className="h-4 w-4" />
            添加飞书机器人
          </button>
        )}
      </div>

      {toast && (
        <div className={`mb-3 rounded-xl px-3 py-2 text-xs ${toast.ok ? 'bg-emerald-50 text-emerald-800' : 'bg-red-50 text-red-700'}`}>{toast.text}</div>
      )}

      <Guide />

      {draft && <ChannelForm draft={draft} onChange={setDraft} onSubmit={submit} onCancel={() => setDraft(null)} saving={saving} />}

      {channels === null && (
        <div className="flex items-center gap-2 py-6 text-xs text-surface-400">
          <Loader2 className="h-3.5 w-3.5 animate-spin" />正在加载…
        </div>
      )}
      {channels?.length === 0 && !draft && <div className="rounded-2xl border border-dashed border-black/[0.1] py-10 text-center text-sm text-surface-400">还没有接入机器人</div>}
      {channels?.map((channel) => (
        <ChannelCard
          key={channel.id}
          channel={channel}
          onToast={showToast}
          onChanged={reload}
          onEdit={() => setDraft({ ...channel, id: channel.id, app_secret: '', verification_token: '', encrypt_key: '' })}
        />
      ))}
    </div>
  )
}
