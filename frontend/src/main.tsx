import React from 'react'
import ReactDOM from 'react-dom/client'
import { BrowserRouter } from 'react-router-dom'
import App from './App'
import { desktopInfo } from '@/lib/desktop'
import { useAuthStore } from '@/stores/auth-store'
import './styles/globals.css'

/** 桌面版：用注入的本机用户令牌自动登录（每次启动重新签发，替换旧令牌） */
async function desktopLogin() {
  const info = desktopInfo()
  if (!info) return
  if (useAuthStore.getState().token === info.token) return
  try {
    const res = await fetch('/api/auth/me', { headers: { Authorization: `Bearer ${info.token}` } })
    if (res.ok) useAuthStore.getState().login(info.token, await res.json())
  } catch (err) {
    console.error('桌面版自动登录失败', err)
  }
}

desktopLogin().finally(() => {
  ReactDOM.createRoot(document.getElementById('root')!).render(
    <React.StrictMode>
      <BrowserRouter>
        <App />
      </BrowserRouter>
    </React.StrictMode>,
  )
})
