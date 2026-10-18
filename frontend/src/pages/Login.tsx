import { useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { useAuthStore } from '@/stores/auth-store'
import { authApi } from '@/api'
import { Loader2 } from 'lucide-react'

const LOGO_URL = '/logo.png'

type Mode = 'login' | 'register'

const input =
  'h-11 w-full rounded-xl border border-surface-200 bg-white px-4 text-sm outline-none transition placeholder:text-surface-300 focus:border-primary-500 focus:ring-4 focus:ring-primary-100'

export default function Login() {
  const navigate = useNavigate()
  const login = useAuthStore((s) => s.login)
  const [mode, setMode] = useState<Mode>('login')
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [confirm, setConfirm] = useState('')
  const [email, setEmail] = useState('')
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState('')

  const switchMode = (next: Mode) => {
    setMode(next)
    setError('')
    setConfirm('')
  }

  const validate = () => {
    if (!username.trim()) return '请输入用户名'
    if (!password) return '请输入密码'
    if (mode === 'register') {
      if (username.trim().length < 3) return '用户名至少 3 个字符'
      if (password.length < 6) return '密码至少 6 个字符'
      if (password !== confirm) return '两次输入的密码不一致'
    }
    return ''
  }

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    const problem = validate()
    setError(problem)
    if (problem) return
    setLoading(true)
    try {
      const { data } =
        mode === 'login'
          ? await authApi.login(username.trim(), password)
          : await authApi.register(username.trim(), email.trim(), password)
      login(data.access_token, data.user)
      navigate('/')
    } catch (err: any) {
      const detail: string | undefined = err.response?.data?.detail
      setError(detail?.replace(/^参数错误:\s*/, '') || (mode === 'login' ? '登录失败，请稍后重试' : '注册失败，请稍后重试'))
    } finally {
      setLoading(false)
    }
  }

  return (
    <div className="min-h-screen bg-[#f6f3ee] px-4 py-8 text-surface-900">
      <main className="mx-auto flex min-h-[calc(100vh-4rem)] w-full max-w-sm flex-col items-center justify-center">
        <div className="mb-4 flex h-14 w-14 items-center justify-center overflow-hidden rounded-2xl bg-white shadow-sm ring-1 ring-black/[0.06]">
          <img src={LOGO_URL} alt="" className="h-full w-full object-cover" />
        </div>
        <h1 className="text-2xl font-extrabold tracking-tight text-surface-900">WaLiOffice</h1>
        <p className="mt-2 text-sm text-surface-500">文档、表格、PPT、数据分析，一句话交给 AI 办公助手</p>

        <div className="mt-8 w-full rounded-3xl bg-white p-6 shadow-[0_18px_60px_rgba(15,23,42,0.08)] ring-1 ring-black/[0.04]">
          <div role="tablist" className="mb-5 grid grid-cols-2 rounded-xl bg-surface-100 p-1 text-sm font-semibold">
            {([['login', '登录'], ['register', '注册']] as const).map(([value, label]) => (
              <button
                key={value}
                type="button"
                role="tab"
                aria-selected={mode === value}
                onClick={() => switchMode(value)}
                className={`rounded-lg py-2 transition ${mode === value ? 'bg-white text-surface-900 shadow-sm' : 'text-surface-500 hover:text-surface-800'}`}
              >
                {label}
              </button>
            ))}
          </div>

          <form onSubmit={handleSubmit} className="space-y-3" noValidate>
            <input
              className={input}
              placeholder="用户名"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              autoComplete="username"
              autoFocus
            />
            {mode === 'register' && (
              <input
                className={input}
                type="email"
                placeholder="邮箱（可选）"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                autoComplete="email"
              />
            )}
            <input
              className={input}
              type="password"
              placeholder={mode === 'register' ? '密码（至少 6 位）' : '密码'}
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              autoComplete={mode === 'login' ? 'current-password' : 'new-password'}
            />
            {mode === 'register' && (
              <input
                className={input}
                type="password"
                placeholder="再次输入密码"
                value={confirm}
                onChange={(e) => setConfirm(e.target.value)}
                autoComplete="new-password"
              />
            )}

            {error && <div className="rounded-lg bg-red-50 px-3 py-2 text-sm text-red-600">{error}</div>}

            <button
              type="submit"
              disabled={loading}
              className="flex h-11 w-full items-center justify-center rounded-xl bg-surface-950 text-sm font-semibold text-white shadow-sm transition hover:bg-surface-800 disabled:cursor-not-allowed disabled:opacity-60"
            >
              {loading ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  {mode === 'login' ? '登录中…' : '注册中…'}
                </>
              ) : mode === 'login' ? '登录' : '注册并登录'}
            </button>
          </form>

          <p className="mt-4 text-center text-xs text-surface-500">
            {mode === 'login' ? '还没有账号？' : '已有账号？'}
            <button type="button" onClick={() => switchMode(mode === 'login' ? 'register' : 'login')} className="ml-1 font-semibold text-surface-900 hover:underline">
              {mode === 'login' ? '注册一个' : '去登录'}
            </button>
          </p>
        </div>

        <button type="button" onClick={() => navigate('/')} className="mt-6 text-sm text-surface-500 hover:text-surface-900">
          回到首页
        </button>
      </main>
    </div>
  )
}
