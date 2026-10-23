// 用编辑器套件（OfficePilot Suite）打开文件：只在桌面版提供。服务端把文件复制成工作副本并启动编辑器，
// 编辑器里保存后自动同步回「我的文件」（可撤销）。
import { editorApi } from '@/api'
import { isDesktop } from '@/lib/desktop'

const EDITABLE = ['docx', 'xlsx', 'pptx', 'pdf', 'md', 'markdown', 'html', 'htm']
const EDITABLE_ARTIFACTS = ['document', 'markdown', 'sheet', 'ppt']

const extension = (name: string) => (name.includes('.') ? name.split('.').pop()!.toLowerCase() : '')

/** 桌面版里这个文件能不能用编辑器打开 */
export const canEditFile = (name: string) => isDesktop() && EDITABLE.includes(extension(name))

/** 桌面版里这个会话产物能不能导出后用编辑器打开 */
export const canEditArtifact = (kind: string | undefined) => isDesktop() && !!kind && EDITABLE_ARTIFACTS.includes(kind)

const errorText = (err: any) => err?.response?.data?.detail?.replace(/^参数错误:\s*/, '') || err?.message || '用编辑器打开失败'

export async function openFileInEditor(fileId: string) {
  try {
    await editorApi.openFile(fileId)
  } catch (err) {
    window.alert(errorText(err))
  }
}

/** 先保存到「我的文件」根目录，再用编辑器打开；返回保存后的路径 */
export async function openArtifactInEditor(artifactId: string, sessionId: string): Promise<string | null> {
  try {
    const res = await editorApi.openArtifact(artifactId, sessionId)
    return res.data.file_path
  } catch (err) {
    window.alert(errorText(err))
    return null
  }
}

/** 窗口重新获得焦点时回调（从编辑器切回来，文件可能已经同步回来了） */
export function onWindowFocus(callback: () => void) {
  if (!isDesktop()) return () => {}
  window.addEventListener('focus', callback)
  return () => window.removeEventListener('focus', callback)
}
