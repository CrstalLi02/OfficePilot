import axios from 'axios';
import { useAuthStore } from '@/stores/auth-store';
import type { TokenResponse, ToolKind, Artifact, PersistedSession, AppSettings, MCPServiceConfig, ChatAttachment, AgentMode, PlanDecisionPayload, SkillSummary } from '@/types';

const API_BASE = '/api';

const api = axios.create({
  baseURL: API_BASE,
});

// 请求拦截器：自动添加 token
api.interceptors.request.use((config) => {
  const token = useAuthStore.getState().token;
  if (token) {
    config.headers.Authorization = `Bearer ${token}`;
  }
  return config;
});

// 响应拦截器：处理认证错误
api.interceptors.response.use(
  (res) => res,
  (err) => {
    if (err.response?.status === 401) {
      useAuthStore.getState().logout();
    }
    return Promise.reject(err);
  }
);

// ===== 认证 API =====
export const authApi = {
  login: (username: string, password: string) =>
    api.post<TokenResponse>('/auth/login', { username, password }),
  register: (username: string, email: string, password: string) =>
    api.post<TokenResponse>('/auth/register', { username, email, password }),
  getMe: () => api.get('/auth/me'),
  changePassword: (oldPassword: string, newPassword: string) =>
    api.post('/auth/change-password', { old_password: oldPassword, new_password: newPassword }),
};

// ===== PPT API =====
export const pptApi = {
  listProjects: (params?: { q?: string; page?: number; page_size?: number }) =>
    api.get('/ppt/projects', { params }),
  getProject: (id: string) => api.get(`/ppt/project/${id}`),
  createProject: (title: string) => api.post('/ppt/project', { title }),
  updateProject: (id: string, updates: { title?: string; theme?: string }) =>
    api.patch(`/ppt/project/${id}`, updates),
  deleteProject: (id: string) => api.post(`/ppt/project/${id}/delete`),
  getSlides: (id: string) => api.get(`/ppt/project/${id}/slides`),
  exportPptx: (id: string) => api.post(`/ppt/project/${id}/export`, {}, { responseType: 'blob' }),
};

// ===== 会话 API =====
export const sessionApi = {
  listSessions: (params?: { q?: string; page?: number; page_size?: number }) =>
    api.get('/chat/sessions', { params }),
  getSession: (id: string) => api.get<PersistedSession>(`/chat/session/${id}`),
  updateSession: (id: string, updates: { title?: string; project_id?: string | null; order_col?: number }) => api.patch(`/chat/session/${id}`, updates),
  deleteSession: (id: string) => api.delete(`/chat/session/${id}`),
};

// ===== Excel API =====
export const excelApi = {
  exportXlsx: (artifact: Artifact) => api.post('/excel/export', {
    title: artifact.title,
    tables: artifact.content?.tables || [],
  }, { responseType: 'blob' }),
};

// ===== Word 文档 API =====
export const docApi = {
  exportDocx: (artifact: Artifact) => api.post('/doc/export', {
    title: artifact.title,
    content: artifact.content,
  }, { responseType: 'blob' }),
};

// ===== 项目 API =====
export const projectApi = {
  listProjects: (params?: { q?: string }) =>
    api.get('/projects', { params }),
  getProject: (id: string) => api.get(`/projects/${id}`),
  createProject: (title: string, tool_kind?: string, description?: string) =>
    api.post('/projects', { title, tool_kind, description }),
  updateProject: (id: string, updates: { title?: string; description?: string; tool_kind?: string }) =>
    api.patch(`/projects/${id}`, updates),
  deleteProject: (id: string) => api.delete(`/projects/${id}`),
  getProjectSessions: (id: string) => api.get(`/projects/${id}/sessions`),
};

// ===== 设置 API =====
export const settingsApi = {
  getSettings: () => api.get<AppSettings>('/settings'),
  saveSettings: (payload: AppSettings) => api.put<AppSettings>('/settings', payload),
  testMcp: (payload: MCPServiceConfig) => api.post('/settings/mcp/test', payload),
  /** 测试模型服务（发一次很短的对话请求） */
  testLlm: (payload: { base_url: string; api_key: string; model: string }) =>
    api.post<{ ok: boolean; error?: string; latency_ms?: number }>('/settings/llm/test', payload),
};

// ===== 文件 API =====
export const fileApi = {
  list: (folderId?: string) => api.get('/files', { params: { folder_id: folderId } }),
  search: (q: string) => api.get('/files/search', { params: { q } }),
  stats: () => api.get('/files/stats'),
  upload: (file: File, folderId?: string, description?: string) => {
    const formData = new FormData();
    formData.append('file', file);
    const headers: Record<string, string> = {};
    headers['x-filename'] = file.name;
    if (folderId) headers['x-folder-id'] = folderId;
    if (description) headers['x-description'] = description;
    return api.post('/files/upload', formData, { headers });
  },
  saveBlob: (blob: Blob, filename: string, description?: string, folderId?: string) => {
    const file = new File([blob], filename, { type: blob.type || 'application/octet-stream' });
    return fileApi.upload(file, folderId, description);
  },
  extract: (file: File) => {
    const formData = new FormData();
    formData.append('file', file);
    return api.post('/files/extract', formData, { headers: { 'x-filename': file.name } });
  },
  get: (id: string) => api.get(`/files/${id}`),
  content: (id: string) => api.get(`/files/${id}/content`),
  thumbnail: (id: string) => api.get(`/files/${id}/thumbnail`, { responseType: 'blob' }),
  preview: (id: string) => api.get(`/files/${id}/preview`),
  download: (id: string) => api.get(`/files/${id}/download`, { responseType: 'blob' }),
  delete: (id: string) => api.delete(`/files/${id}`),
};

// ===== 文件夹 API =====
export const folderApi = {
  list: (parentId?: string) => api.get('/files/folders/list', { params: { parent_id: parentId } }),
  create: (name: string, parentId?: string) => api.post('/folders', { name, parent_id: parentId }),
  delete: (id: string) => api.delete(`/folders/${id}`),
};

// ===== 通知 API =====
export const notificationApi = {
  list: (params?: { unread_only?: boolean; page?: number; page_size?: number }) =>
    api.get('/notifications', { params }),
  unreadCount: () => api.get('/notifications/unread'),
  markAsRead: (id: string) => api.post(`/notifications/${id}/read`),
  markAllAsRead: () => api.post('/notifications/read-all'),
  delete: (id: string) => api.delete(`/notifications/${id}`),
};

// ===== 技能 API =====
export const skillsApi = {
  list: () => api.get<{ skills: SkillSummary[] }>('/skills'),
  detail: (name: string) => api.get<SkillSummary>(`/skills/${name}`),
  setEnabled: (name: string, enabled: boolean) => api.post(`/skills/${name}/enabled`, { enabled }),
  /** 上传 .zip 或 SKILL.md */
  importFile: (file: File) => {
    const formData = new FormData();
    formData.append('file', file);
    return api.post<{ name: string; description: string }>('/skills/import', formData);
  },
  /** 用 SKILL.md 文本创建（同名用户技能会被覆盖） */
  create: (skillMd: string) => api.post<{ name: string; description: string }>('/skills', { skill_md: skillMd }),
  remove: (name: string) => api.delete(`/skills/${name}`),
  /** 从会话生成技能草稿（不保存） */
  draft: (sessionId: string, hint?: string) => api.post<{ skill_md: string; error?: string | null }>('/skills/draft', { session_id: sessionId, hint }),
};

// ===== 工作区操作 API =====
export const workspaceApi = {
  /** 撤销 Agent 的一次写入 / 整理 */
  undo: (opId: string) => api.post<{ undone: number; skipped: string[] }>(`/workspace/ops/${opId}/undo`),
};

// ===== 产物版本 API =====
export interface ArtifactVersionMeta {
  version: number;
  title: string;
  change_summary?: string | null;
  author: 'agent' | 'user' | string;
  created_at: string;
}

export const artifactApi = {
  versions: (artifactId: string, sessionId: string) =>
    api.get<{ current_version: number; versions: ArtifactVersionMeta[] }>(`/artifacts/${artifactId}/versions`, { params: { session_id: sessionId } }),
  version: (artifactId: string, version: number, sessionId: string) =>
    api.get<{ artifact: Artifact }>(`/artifacts/${artifactId}/versions/${version}`, { params: { session_id: sessionId } }),
  restore: (artifactId: string, sessionId: string, version: number) =>
    api.post<{ artifact: Artifact }>(`/artifacts/${artifactId}/restore`, { session_id: sessionId, version }),
  /** 保存用户手动修改，生成新版本 */
  save: (artifactId: string, sessionId: string, payload: { content: any; title?: string; change_summary?: string }) =>
    api.put<{ artifact: Artifact; unchanged?: boolean }>(`/artifacts/${artifactId}`, { session_id: sessionId, ...payload }),
};

/** 逐行解析 SSE 响应，把每个事件交给 onEvent */
async function readSse(response: Response, onEvent: (event: string, data: any) => void) {
  const reader = response.body?.getReader();
  if (!reader) return;
  const decoder = new TextDecoder();
  let buffer = '';
  let currentEvent = 'message';
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      const lines = buffer.split('\n');
      buffer = lines.pop() || '';
      for (const line of lines) {
        if (line.startsWith('event:')) {
          currentEvent = line.slice(6).trim();
        } else if (line.startsWith('data:')) {
          const dataStr = line.slice(5).trim();
          if (dataStr) {
            try {
              onEvent(currentEvent, JSON.parse(dataStr));
            } catch {
              onEvent(currentEvent, dataStr);
            }
          }
        } else if (line.trim() === '') {
          currentEvent = 'message';
        }
      }
    }
  } finally {
    reader.releaseLock();
  }
}

/** 浏览器时区（IANA 名称），用于定时任务与“今天 / 下周五”这类时间理解 */
export function browserTimezone(): string | undefined {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || undefined;
  } catch {
    return undefined;
  }
}

// ===== 定时任务 API =====
export interface Schedule {
  id: string;
  name: string;
  prompt: string;
  cron: string;
  timezone: string;
  description: string;
  mode: AgentMode;
  skills: string[];
  session_strategy: 'new' | 'same';
  session_id?: string | null;
  enabled: boolean;
  next_run_at?: string | null;
  last_run_at?: string | null;
  last_task_id?: string | null;
  created_at: string;
  /** 运行结果推送到的 IM 会话 */
  notify?: (ImTarget & { label: string }) | null;
}

export interface ImTarget {
  channel_id: string;
  chat_id: string;
}

export interface ScheduleInput {
  name: string;
  prompt: string;
  cron: string;
  timezone?: string;
  mode?: AgentMode;
  skills?: string[];
  session_strategy?: 'new' | 'same';
  enabled?: boolean;
  /** 不传或 null 表示不推送 */
  notify?: ImTarget | null;
}

export const schedulesApi = {
  list: () => api.get<{ schedules: Schedule[] }>('/schedules'),
  create: (input: ScheduleInput) => api.post<Schedule>('/schedules', input),
  update: (id: string, input: ScheduleInput) => api.put<Schedule>(`/schedules/${id}`, input),
  remove: (id: string) => api.delete(`/schedules/${id}`),
  runNow: (id: string) => api.post<{ status: string; message?: string; task_id?: string; session_id?: string }>(`/schedules/${id}/run`),
  runs: (id: string) => api.get<{ runs: Array<{ task_id?: string | null; status: string; message?: string | null; created_at: string }> }>(`/schedules/${id}/runs`),
  preview: (cron: string, timezone?: string) => api.post<{ description: string; timezone: string; next_runs: string[] }>('/schedules/preview', { cron, timezone }),
};

// ===== IM 接入 API =====
export interface ImChannelConfig {
  app_id: string;
  domain: 'feishu' | 'lark';
  connection: 'ws' | 'webhook';
  default_mode: 'craft' | 'plan' | 'ask';
  timezone: string;
}

export interface ImChannel {
  id: string;
  platform: 'feishu' | string;
  name: string;
  config: ImChannelConfig;
  has_app_secret: boolean;
  has_verification_token: boolean;
  has_encrypt_key: boolean;
  secrets_broken: boolean;
  enabled: boolean;
  created_at: string;
  status: { state: string; error?: string | null; bot_name?: string | null; last_event_at?: string | null };
}

export interface ImChannelInput {
  platform?: string;
  name: string;
  config: ImChannelConfig;
  /** 留空表示不修改 */
  app_secret?: string;
  verification_token?: string;
  encrypt_key?: string;
  enabled?: boolean;
}

export interface ImBinding {
  id: string;
  im_user_id: string;
  display_name?: string | null;
  created_at: string;
}

export const imApi = {
  list: () => api.get<{ channels: ImChannel[] }>('/im/channels'),
  create: (input: ImChannelInput) => api.post<ImChannel>('/im/channels', input),
  update: (id: string, input: ImChannelInput) => api.put<ImChannel>(`/im/channels/${id}`, input),
  remove: (id: string) => api.delete(`/im/channels/${id}`),
  test: (id: string) => api.post<{ ok: boolean; bot_name?: string; error?: string }>(`/im/channels/${id}/test`),
  bindCode: (id: string) => api.post<{ code: string; expires_in: number }>(`/im/channels/${id}/bind-code`),
  bindings: (id: string) => api.get<{ bindings: ImBinding[] }>(`/im/channels/${id}/bindings`),
  unbind: (id: string, bindingId: string) => api.delete(`/im/channels/${id}/bindings/${bindingId}`),
  /** 可以推送定时任务结果的 IM 会话（给机器人发过消息的） */
  targets: () => api.get<{ targets: Array<ImTarget & { label: string }> }>('/im/targets'),
};

// ===== MCP 插件 API =====
export interface McpServer {
  id: string;
  slug: string;
  name: string;
  transport: 'http' | 'sse' | 'stdio';
  url?: string | null;
  command?: string | null;
  args: string[];
  description: string;
  disabled_tools: string[];
  /** 调用前确认：writes 写操作需要确认 / always 每次都确认 / never 无需确认 */
  approval: 'writes' | 'always' | 'never';
  /** 所有工具都只读取数据 */
  assume_read_only: boolean;
  catalog_id?: string | null;
  query_names: string[];
  header_names: string[];
  env_names: string[];
  secrets_broken: boolean;
  enabled: boolean;
  created_at: string;
  updated_at: string;
  status: { state: string; error?: string | null; tool_count?: number; server_name?: string; checked_at?: string };
}

export interface McpServerInput {
  name: string;
  slug?: string;
  transport: McpServer['transport'];
  url?: string;
  command?: string;
  args?: string[];
  description?: string;
  /** 值留空表示沿用已保存的值；列表里没有的会被删除 */
  headers?: Array<{ name: string; value: string }>;
  env?: Array<{ name: string; value: string }>;
  enabled?: boolean;
  approval?: McpServer['approval'];
  assume_read_only?: boolean;
}

export interface McpTool {
  name: string;
  title?: string | null;
  description: string;
  read_only: boolean;
  enabled: boolean;
  exposed_name: string;
}

export type McpToolsResult = { ok: true; tools: McpTool[]; status?: McpServer['status'] } | { ok: false; error: string };

export const mcpApi = {
  capabilities: () => api.get<{ stdio: boolean; allow_private: boolean; max_tools_per_run: number }>('/mcp/capabilities'),
  list: () => api.get<{ servers: McpServer[] }>('/mcp/servers'),
  create: (input: McpServerInput) => api.post<McpServer>('/mcp/servers', input),
  update: (id: string, input: McpServerInput) => api.put<McpServer>(`/mcp/servers/${id}`, input),
  remove: (id: string) => api.delete(`/mcp/servers/${id}`),
  test: (id: string) => api.post<McpToolsResult>(`/mcp/servers/${id}/test`),
  tools: (id: string) => api.get<McpToolsResult>(`/mcp/servers/${id}/tools`),
  setDisabledTools: (id: string, disabled: string[]) => api.put<McpServer>(`/mcp/servers/${id}/tools`, { disabled }),
  importConfig: (config: string) => api.post<{ created: McpServer[]; errors: string[] }>('/mcp/import', { config }),
  /** 所有已启用插件的资源（@ 菜单） */
  resources: () => api.get<{ resources: McpResource[]; errors: string[] }>('/mcp/resources'),
  /** 所有已启用插件的提示词（/ 菜单） */
  prompts: () => api.get<{ prompts: McpPrompt[]; errors: string[] }>('/mcp/prompts'),
  catalog: () => api.get<{ entries: McpCatalogEntry[] }>('/mcp/catalog'),
  install: (id: string, fields: Record<string, string>) => api.post<McpServer>(`/mcp/catalog/${id}/install`, { fields }),
  getPrompt: (serverId: string, name: string, args: Record<string, string>) =>
    api.post<{ text: string }>(`/mcp/servers/${serverId}/prompts/get`, { name, arguments: args }),
};

export interface McpCatalogEntry {
  id: string;
  name: string;
  slug: string;
  category: string;
  description: string;
  docs: string;
  transport: McpServer['transport'];
  url?: string | null;
  command?: string | null;
  args: string[];
  fields: Array<{ key: string; label: string; help: string; required: boolean; secret: boolean }>;
  available: boolean;
  installed: boolean;
}

export interface McpResource {
  server_id: string;
  server_name: string;
  uri: string;
  name: string;
  title?: string | null;
  description?: string | null;
  mime_type?: string | null;
}

export interface McpPrompt {
  server_id: string;
  server_name: string;
  name: string;
  title?: string | null;
  description?: string | null;
  arguments: Array<{ name: string; description?: string; required?: boolean }>;
}

// ===== 后台任务 API =====
export interface AgentTask {
  id: string;
  session_id: string;
  origin: 'chat' | 'schedule' | 'im' | string;
  title: string;
  status: 'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled' | 'awaiting_approval' | 'interrupted' | string;
  summary?: string | null;
  error?: string | null;
  created_at: string;
  started_at?: string | null;
  finished_at?: string | null;
  /** 仍在运行，可订阅进度 */
  live: boolean;
}

export const tasksApi = {
  list: () => api.get<{ tasks: AgentTask[] }>('/tasks'),
  cancel: (taskId: string) => api.post<{ cancelled: boolean }>(`/tasks/${taskId}/cancel`),
  retry: (taskId: string) => api.post<{ task_id: string; session_id: string }>(`/tasks/${taskId}/retry`),
  /** 处理工具调用前的确认；remember 表示本次对话里这个工具都允许 */
  approve: (approvalId: string, decision: 'approve' | 'reject', remember = false) =>
    api.post<{ ok: boolean }>(`/approvals/${approvalId}`, { decision, remember }),
};

// ===== 对话 API (SSE) =====
export const chatApi = {
  /** 重新订阅后台任务的进度（after 为已收到的最后一个事件序号） */
  attach: async (taskId: string, after: number, onEvent: (event: string, data: any) => void, token: string, signal?: AbortSignal) => {
    const response = await fetch(`${API_BASE}/tasks/${taskId}/stream?after=${after}`, {
      headers: { Authorization: `Bearer ${token}` },
      signal,
    });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    await readSse(response, onEvent);
  },
  /** 停止某个会话正在运行的 Agent（后端会中断 LLM 与工具，并保存已生成的产物） */
  cancel: (sessionId: string) => api.post('/chat/cancel', { session_id: sessionId }),
  stream: async (
    message: string,
    projectId: string | null,
    sessionId: string | null,
    theme: string | null,
    toolKind: ToolKind,
    model: string | null,
    attachments: ChatAttachment[],
    onEvent: (event: string, data: any) => void,
    token: string,
    signal?: AbortSignal,
    toolConfig?: Record<string, any>,
    options?: {
      mode?: AgentMode;
      resume?: PlanDecisionPayload;
      fileRefs?: string[];
      skills?: string[];
      mcpResources?: Array<{ server_id: string; uri: string; name: string }>;
    }
  ) => {
    const response = await fetch(`${API_BASE}/chat/stream`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        Authorization: `Bearer ${token}`,
      },
      body: JSON.stringify({
        message,
        project_id: projectId,
        session_id: sessionId,
        theme,
        tool_kind: toolKind,
        model,
        attachments,
        tool_config: toolConfig,
        mode: options?.mode,
        resume: options?.resume,
        file_refs: options?.fileRefs?.length ? options.fileRefs : undefined,
        skills: options?.skills?.length ? options.skills : undefined,
        mcp_resources: options?.mcpResources?.length ? options.mcpResources : undefined,
        timezone: browserTimezone(),
      }),
      signal,
    });

    if (!response.ok) {
      let detail = `HTTP ${response.status}`;
      try {
        const err = await response.json();
        detail = err.detail || err.message || detail;
      } catch {
        try {
          detail = await response.text();
        } catch {
          detail = `HTTP ${response.status}`;
        }
      }
      if (response.status === 401) {
        useAuthStore.getState().logout();
      }
      throw new Error(detail);
    }

    await readSse(response, onEvent);
  },
};

export interface IntegrationAgent {
  id: string
  name: string
  detected: boolean
  dir?: string | null
  installed: boolean
  up_to_date: boolean
  /** 已有同名 skill，但不是 OfficePilot 安装的 */
  foreign?: boolean
}

export interface IntegrationsOverview {
  /** 能否写入本机目录（桌面版） */
  local: boolean
  skill_version: string
  agents: IntegrationAgent[]
  cli: { path?: string | null; link?: string | null; linked: boolean }
  mcp: { claude_code: string; json: string; http: string }
}

export const integrationsApi = {
  overview: () => api.get<IntegrationsOverview>('/integrations'),
  install: (agent: string) => api.post<{ installed: boolean; dir: string }>(`/integrations/skill/${agent}`),
  uninstall: (agent: string) => api.delete(`/integrations/skill/${agent}`),
  skillZip: () => api.get<Blob>('/integrations/skill.zip', { responseType: 'blob' }),
  linkCli: (replace = false) => api.post<{ linked: boolean; exists?: boolean; path: string; on_path?: boolean }>('/integrations/cli/link', { replace }),
  unlinkCli: () => api.delete('/integrations/cli/link'),
}
