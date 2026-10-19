import { useState } from 'react'
import { Check, Loader2, Sparkles } from 'lucide-react'
import { settingsApi } from '@/api'
import type { AppSettings } from '@/types'

/** 常用的 OpenAI 兼容服务；模型名可以改 */
const PRESETS = [
  { id: 'deepseek', label: 'DeepSeek', baseUrl: 'https://api.deepseek.com/v1', model: 'deepseek-chat', keyHint: '在 platform.deepseek.com 创建 API Key' },
  { id: 'dashscope', label: '通义千问（阿里云百炼）', baseUrl: 'https://dashscope.aliyuncs.com/compatible-mode/v1', model: 'qwen-plus', keyHint: '在阿里云百炼控制台创建 API Key' },
  { id: 'custom', label: '其他 OpenAI 兼容服务', baseUrl: '', model: '', keyHint: '服务商提供的 API Key' },
] as const

interface ModelSetupDialogProps {
  settings: AppSettings
  onSaved: (settings: AppSettings) => void
  onLater: () => void
}

const field = 'mt-1.5 w-full rounded-xl border border-black/[0.08] bg-white px-3 py-2 text-sm outline-none focus:border-surface-400'

/** 首次使用：还没有可用的模型服务时引导填写（桌面版没有环境变量里的模型配置） */
export function ModelSetupDialog({ settings, onSaved, onLater }: ModelSetupDialogProps) {
  const active = settings.llm_profiles.find((p) => p.id === settings.active_profile_id) || settings.llm_profiles[0]
  const [preset, setPreset] = useState<(typeof PRESETS)[number]['id']>(active?.base_url.includes('deepseek') || !active?.base_url ? 'deepseek' : 'custom')
  const [baseUrl, setBaseUrl] = useState(active?.base_url || PRESETS[0].baseUrl)
  const [model, setModel] = useState(active?.default_model || PRESETS[0].model)
  const [apiKey, setApiKey] = useState('')
  const [status, setStatus] = useState<{ ok: boolean; text: string } | null>(null)
  const [busy, setBusy] = useState<'test' | 'save' | null>(null)
  const current = PRESETS.find((p) => p.id === preset)!

  const choose = (id: (typeof PRESETS)[number]['id']) => {
    const next = PRESETS.find((p) => p.id === id)!
    setPreset(id)
    if (next.baseUrl) setBaseUrl(next.baseUrl)
    setModel(next.model)
    setStatus(null)
  }

  const test = async () => {
    setBusy('test')
    setStatus(null)
    try {
      const { data } = await settingsApi.testLlm({ base_url: baseUrl, api_key: apiKey, model })
      setStatus(data.ok ? { ok: true, text: `连接成功（${data.latency_ms} ms）` } : { ok: false, text: data.error || '连接失败' })
      return data.ok
    } catch (err: any) {
      setStatus({ ok: false, text: err?.response?.data?.detail?.replace(/^参数错误:\s*/, '') || '连接失败' })
      return false
    } finally {
      setBusy(null)
    }
  }

  const save = async () => {
    if (!(await test())) return
    setBusy('save')
    try {
      const profileId = active?.id || 'default'
      const profile = { ...(active || { id: profileId, name: '默认模型服务' }), base_url: baseUrl.trim(), api_keys: [apiKey.trim()], api_key: undefined, models: [model.trim()], default_model: model.trim() }
      const next: AppSettings = {
        ...settings,
        llm_profiles: settings.llm_profiles.some((p) => p.id === profileId) ? settings.llm_profiles.map((p) => (p.id === profileId ? profile : p)) : [profile, ...settings.llm_profiles],
        active_profile_id: profileId,
        default_model: model.trim(),
        active_model: model.trim(),
      }
      const { data } = await settingsApi.saveSettings(next)
      onSaved(data)
    } catch (err: any) {
      setStatus({ ok: false, text: err?.response?.data?.detail?.replace(/^参数错误:\s*/, '') || '保存失败' })
    } finally {
      setBusy(null)
    }
  }

  return (
    <div className="fixed inset-0 z-[120] flex items-center justify-center bg-black/30 p-4">
      <div role="dialog" aria-label="配置模型服务" className="w-full max-w-md rounded-3xl bg-white p-6 shadow-2xl">
        <div className="flex items-center gap-2">
          <div className="rounded-xl bg-violet-50 p-2 text-violet-600">
            <Sparkles className="h-4 w-4" />
          </div>
          <div>
            <div className="text-base font-bold text-surface-900">先配置一个模型服务</div>
            <div className="text-xs text-surface-500">Agent 需要一个大模型才能工作，密钥只保存在本机</div>
          </div>
        </div>

        <div className="mt-4 grid gap-1.5">
          {PRESETS.map((p) => (
            <button
              key={p.id}
              type="button"
              onClick={() => choose(p.id)}
              className={`rounded-xl border px-3 py-2 text-left text-sm ${preset === p.id ? 'border-surface-900 bg-surface-50 font-semibold' : 'border-black/[0.08] hover:border-surface-300'}`}
            >
              {p.label}
            </button>
          ))}
        </div>

        <label className="mt-4 block text-xs font-medium text-surface-600">
          服务地址
          <input className={field} value={baseUrl} placeholder="https://…/v1" onChange={(e) => setBaseUrl(e.target.value)} />
        </label>
        <label className="mt-3 block text-xs font-medium text-surface-600">
          API Key
          <input className={field} type="password" autoComplete="off" value={apiKey} placeholder={current.keyHint} onChange={(e) => setApiKey(e.target.value)} />
        </label>
        <label className="mt-3 block text-xs font-medium text-surface-600">
          模型名
          <input className={field} value={model} placeholder="例如 deepseek-chat" onChange={(e) => setModel(e.target.value)} />
        </label>

        {status && (
          <div className={`mt-3 flex items-center gap-1.5 rounded-xl px-3 py-2 text-xs ${status.ok ? 'bg-emerald-50 text-emerald-800' : 'bg-red-50 text-red-700'}`}>
            {status.ok && <Check className="h-3.5 w-3.5" />}
            {status.text}
          </div>
        )}

        <div className="mt-5 flex items-center gap-2">
          <button
            type="button"
            disabled={!!busy || !baseUrl.trim() || !apiKey.trim() || !model.trim()}
            onClick={save}
            className="inline-flex items-center gap-1.5 rounded-full bg-surface-950 px-4 py-2 text-sm font-semibold text-white hover:bg-surface-800 disabled:opacity-50"
          >
            {busy === 'save' && <Loader2 className="h-3.5 w-3.5 animate-spin" />}测试并保存
          </button>
          <button type="button" disabled={!!busy || !apiKey.trim()} onClick={test} className="inline-flex items-center gap-1.5 rounded-full border border-black/10 px-4 py-2 text-sm text-surface-700 hover:bg-surface-50 disabled:opacity-50">
            {busy === 'test' && <Loader2 className="h-3.5 w-3.5 animate-spin" />}只测试
          </button>
          <button type="button" onClick={onLater} className="ml-auto text-xs text-surface-400 hover:text-surface-700">稍后再说</button>
        </div>
      </div>
    </div>
  )
}
