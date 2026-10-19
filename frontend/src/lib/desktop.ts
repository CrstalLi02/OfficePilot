// 桌面版（Tauri）环境：窗口初始化脚本注入 window.__OFFICEPILOT_DESKTOP__（本机用户令牌等），
// 并提供几个原生能力：在访达中显示、用默认应用打开、选择文件夹。网页版里这些都不可用。

interface DesktopInfo {
  token: string
  version: string
  dataDir: string
}

declare global {
  interface Window {
    __OFFICEPILOT_DESKTOP__?: DesktopInfo
    __TAURI_INTERNALS__?: { invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown> }
  }
}

export function desktopInfo(): DesktopInfo | null {
  return typeof window !== 'undefined' ? window.__OFFICEPILOT_DESKTOP__ ?? null : null
}

export const isDesktop = () => desktopInfo() !== null

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const internals = window.__TAURI_INTERNALS__
  if (!internals) throw new Error('只有桌面版支持这个操作')
  return internals.invoke(cmd, args) as Promise<T>
}

/** 在访达 / 资源管理器中显示 */
export const revealPath = (path: string) => invoke<void>('reveal_path', { path })

/** 用默认应用打开 */
export const openPath = (path: string) => invoke<void>('open_path', { path })

/** 选择文件夹；取消时返回 null */
export const pickFolder = () => invoke<string | null>('pick_folder')

/** 系统里的文件管理器名称 */
export const fileManagerName = () => (/Mac/i.test(navigator.platform) ? '访达' : '资源管理器')
