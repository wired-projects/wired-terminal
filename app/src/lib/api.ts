/**
 * Typed client for the Wired backend.
 *
 * Every call goes through `request()` so timeouts, auth and error shaping are
 * handled once. The backend returns its errors as `{ detail: string }`; that
 * gets unwrapped into the thrown `ApiError`'s message, which is what the UI
 * shows.
 *
 * Where the API lives is decided at *runtime*, not at build time. A packaged
 * app is built once and then has to be told its own port and token — the port
 * because 8000 may belong to something else, and the token because it might not
 * exist until the user turns auth on. `VITE_API_BASE` / `VITE_AUTH_TOKEN`
 * survive as the dev-server fallback.
 */

const DEFAULT_PORT = 8000;
/** How far to look for a backend that stepped past a busy port. */
const PORT_SEARCH = 6;
const DEFAULT_TIMEOUT_MS = 10_000;

let apiBase = (import.meta.env.VITE_API_BASE?.trim() || `http://127.0.0.1:${DEFAULT_PORT}`).replace(
  /\/+$/,
  '',
);
let authToken = import.meta.env.VITE_AUTH_TOKEN?.trim() || '';

export const API_BASE = () => apiBase;

interface RuntimeConfig {
  port: number;
  token: string;
}

/** Tauri exposes `invoke` on the window because `withGlobalTauri` is set. */
function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> | null {
  const tauri = (window as unknown as { __TAURI__?: { core?: { invoke?: Function } } }).__TAURI__;
  const fn = tauri?.core?.invoke;
  return fn ? (fn(command, args) as Promise<T>) : null;
}

export const isDesktopApp = () => invoke('') !== null;

async function reachable(base: string): Promise<boolean> {
  try {
    const res = await fetch(`${base}/healthz`, { signal: AbortSignal.timeout(1200) });
    return res.ok;
  } catch {
    return false;
  }
}

/**
 * Resolve the API address before the first request. Called once from `main.tsx`
 * so every later accessor can stay synchronous.
 */
export async function initApiConfig(): Promise<void> {
  const fromShell = await invoke<RuntimeConfig>('runtime_config')?.catch(() => null);
  if (fromShell?.port) {
    apiBase = `http://127.0.0.1:${fromShell.port}`;
    authToken = fromShell.token ?? '';
    return;
  }

  // Browser dev: the backend may have moved off 8000 the same way.
  if (await reachable(apiBase)) return;
  for (let port = DEFAULT_PORT + 1; port < DEFAULT_PORT + PORT_SEARCH; port += 1) {
    const candidate = `http://127.0.0.1:${port}`;
    if (await reachable(candidate)) {
      apiBase = candidate;
      return;
    }
  }
}

/** WebSocket URL for the live terminal, carrying the token if there is one. */
export function websocketUrl(): string {
  const url = new URL(`${apiBase}/ws`);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  // The WebSocket constructor cannot set headers, so the token has to ride
  // in the query string. Same fallback the backend documents for EventSource.
  if (authToken) url.searchParams.set('token', authToken);
  return url.toString();
}

/** SSE URL for the transcript stream — EventSource also cannot set headers. */
export function streamUrl(params: Record<string, string> = {}): string {
  const url = new URL(`${apiBase}/api/agent/output/stream`);
  for (const [k, v] of Object.entries(params)) url.searchParams.set(k, v);
  if (authToken) url.searchParams.set('token', authToken);
  return url.toString();
}

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

/** Thrown when the backend could not be reached at all — see `errors.ts`. */
export const OFFLINE = 'offline';

async function request<T>(
  path: string,
  init: RequestInit & { timeoutMs?: number } = {},
): Promise<T> {
  const { timeoutMs = DEFAULT_TIMEOUT_MS, ...rest } = init;
  const controller = new AbortController();
  const timer = window.setTimeout(() => controller.abort(), timeoutMs);

  const headers = new Headers(rest.headers);
  if (rest.body && !headers.has('Content-Type')) {
    headers.set('Content-Type', 'application/json');
  }
  if (authToken) headers.set('Authorization', `Bearer ${authToken}`);

  try {
    const res = await fetch(`${apiBase}${path}`, {
      ...rest,
      headers,
      signal: controller.signal,
    });

    const body = await res.json().catch(() => ({}) as Record<string, unknown>);
    if (!res.ok) {
      const detail =
        typeof body.detail === 'string' ? body.detail : `Request failed (${res.status})`;
      throw new ApiError(detail, res.status);
    }
    return body as T;
  } catch (err) {
    if (err instanceof ApiError) throw err;
    if (err instanceof DOMException && err.name === 'AbortError') {
      throw new ApiError(`This is taking longer than ${timeoutMs / 1000} seconds.`, 0);
    }
    // No shell command here: inside a packaged app there is no project root to
    // run one from. `errors.ts` turns this into a sentence and a button.
    throw new ApiError(OFFLINE, 0);
  } finally {
    window.clearTimeout(timer);
  }
}

const post = <T>(path: string, body?: unknown, timeoutMs?: number) =>
  request<T>(path, {
    method: 'POST',
    body: body === undefined ? undefined : JSON.stringify(body),
    timeoutMs,
  });

// ── response shapes ───────────────────────────────────────────────────

export type ProviderId = 'claude' | 'grok' | 'codex' | 'gemini' | 'shell';

export interface PendingPrompt {
  seq: number;
  ts: number;
  kind: string;
  text: string;
}

export interface HealthResponse {
  ok: boolean;
  version: string;
  providers: Record<string, { available: boolean; path: string | null }>;
  assistant: AssistantStatus;
  pending_prompt: PendingPrompt | null;
  onboarded: boolean;
  folder: string;
  chat: { enabled: boolean; connected: boolean; pending_pairings: number };
  security: {
    auth_required: boolean;
    loopback_only: boolean;
    agent_auto_approve: boolean;
  };
}

export interface AssistantStatus {
  enabled: boolean;
  keep_alive: boolean;
  auto_start: boolean;
  provider: ProviderId;
  session_running: boolean;
  session_provider: ProviderId | null;
  generation: number;
  last_error: string | null;
  restarts_last_hour: number;
}

export interface AgentStatusResponse {
  providers: Record<string, { available: boolean; path: string | null; label: string }>;
  assistant: AssistantStatus;
  session: {
    running: boolean;
    provider: ProviderId | null;
    generation: number | null;
  };
}

export interface StartSessionResponse {
  status: string;
  provider: ProviderId;
  generation: number;
  assistant: AssistantStatus;
}

export interface Settings {
  assistant: ProviderId;
  folder: string;
  always_on: boolean;
  auto_start: boolean;
  ask_before_acting: boolean;
  start_at_login: boolean;
  notifications: boolean;
  onboarded: boolean;
  port: number;
  auth_required: boolean;
  secrets: 'keychain' | 'file';
  config_file: string;
  log_file: string;
  env_overrides: string[];
}

export interface SetupProvider {
  id: ProviderId;
  label: string;
  available: boolean;
  path: string | null;
  detail: string;
  signed_in: boolean | null;
  installable: boolean;
}

export interface NodeStatus {
  found: boolean;
  node: string | null;
  npm: string | null;
  version: string | null;
  supported: boolean;
  download: string;
}

export interface InstallStatus {
  status: 'idle' | 'running' | 'done' | 'failed';
  provider: string;
  message: string;
  log: string[];
}

export interface SetupState {
  onboarded: boolean;
  node: NodeStatus;
  providers: SetupProvider[];
  install: InstallStatus;
  folder: { current: string; chosen: string | null; suggested: string };
  ask_before_acting: boolean;
}

export interface Pairing {
  platform: string;
  chat: number;
  display: string;
  expires_in: number;
  code: string;
}

export interface GatewayStatus {
  platform: string;
  enabled: boolean;
  configured: boolean;
  connected: boolean;
  muted: boolean;
  bot: string | null;
  paired_chats: number;
  pending: Pairing[];
  locked_for: number | null;
  last_error: string | null;
}

export interface Schedule {
  id: string;
  name: string;
  task: string;
  when: string;
  enabled: boolean;
  quiet_when_nothing: boolean;
  last_run: number | null;
  last_result: string | null;
  next_run: number | null;
  when_readable: string;
  next_readable: string | null;
}

export interface HistoryEvent {
  seq: number;
  ts: number;
  kind: string;
  text: string;
  generation: number;
}

export interface Check {
  id: string;
  label: string;
  ok: boolean | null;
  detail: string;
  fix: string | null;
}

export interface DiagnosticsReport {
  version: string;
  os: string;
  arch: string;
  host: string;
  port: number;
  log_file: string;
  config_dir: string;
  data_dir: string;
  folder: string;
  checks: Check[];
  recent_log: string[];
  [key: string]: unknown;
}

// ── endpoints ─────────────────────────────────────────────────────────

export const api = {
  health: (timeoutMs = 2500) => request<HealthResponse>('/api/health', { timeoutMs }),

  agentStatus: () => request<AgentStatusResponse>('/api/agent/status'),

  startAgent: (provider: ProviderId, keepAlive: boolean, cols = 100, rows = 32) =>
    // Launching a CLI can take a moment on a cold start.
    post<StartSessionResponse>(
      '/api/agent/start',
      { provider, keep_alive: keepAlive, cols, rows },
      20_000,
    ),

  stopAgent: () => post<{ status: string }>('/api/agent/stop'),

  sendMessage: (text: string, submit: boolean) =>
    post<{ status: string; bytes: number }>('/api/agent/message', {
      text,
      submit,
      multiline: true,
      ensure_session: true,
    }),

  sendKey: (key: string) => post<{ status: string }>('/api/agent/key', { key }),

  approve: (allow: boolean) => post<{ status: string }>('/api/agent/approve', { allow }),

  startPty: (provider: ProviderId, cols: number, rows: number) =>
    post<{ status: string; provider: ProviderId; generation: number }>('/api/pty/start', {
      provider,
      cols,
      rows,
    }),

  resizePty: (cols: number, rows: number) =>
    post<{ status: string }>('/api/pty/resize', { cols, rows }),

  killPty: () => post<{ status: string }>('/api/pty/kill'),

  // ── settings ──
  settings: () => request<Settings>('/api/settings'),
  saveSettings: (patch: Partial<Record<keyof Settings | 'auth_token', unknown>>) =>
    post<{ settings: Settings; restart_required: boolean; restart_assistant: boolean }>(
      '/api/settings',
      patch,
    ),

  // ── setup ──
  setupState: () => request<SetupState>('/api/setup/state'),
  install: (provider: ProviderId) =>
    post<{ install: InstallStatus }>('/api/setup/install', { provider }),
  login: (provider: ProviderId) =>
    post<{ message: string }>('/api/setup/login', { provider }, 20_000),
  setFolder: (folder: string) => post<{ folder: string }>('/api/setup/folder', { folder }),

  // ── chat bridge ──
  gateway: () => request<GatewayStatus>('/api/gateway/status'),
  configureGateway: (patch: { enabled?: boolean; bot_token?: string; muted?: boolean }) =>
    post<{ gateway: GatewayStatus }>('/api/gateway/configure', patch, 20_000),
  /** Forget the bot token and every phone paired to it. */
  resetGateway: () => post<{ gateway: GatewayStatus }>('/api/gateway/reset'),
  approvePairing: (code: string) => post<{ paired: Pairing }>('/api/gateway/pairings/approve', { code }),
  denyPairing: (code: string) => post<{ denied: Pairing }>('/api/gateway/pairings/deny', { code }),
  unpair: (chat: number) => post<{ gateway: GatewayStatus }>('/api/gateway/unpair', { chat }),

  // ── schedules ──
  schedules: () => request<{ schedules: Schedule[]; running: string | null }>('/api/schedules'),
  saveSchedule: (schedule: Partial<Schedule>) =>
    post<{ schedule: Schedule }>('/api/schedules', schedule),
  deleteSchedule: (id: string) => post<{ status: string }>('/api/schedules/delete', { id }),
  runSchedule: (id: string) =>
    post<{ result: string }>('/api/schedules/run', { id }, 320_000),

  // ── history ──
  historyDays: () => request<{ days: string[] }>('/api/history/days'),
  historyDay: (day?: string) =>
    request<{ day: string; events: HistoryEvent[] }>(
      `/api/history/day${day ? `?day=${encodeURIComponent(day)}` : ''}`,
    ),
  historySearch: (query: string) =>
    request<{ hits: { day: string; event: HistoryEvent }[] }>(
      `/api/history/search?query=${encodeURIComponent(query)}`,
    ),

  // ── diagnostics ──
  diagnostics: () => request<DiagnosticsReport>('/api/diagnostics'),
  eraseEverything: () => post<{ removed: string[] }>('/api/diagnostics/reset', { confirm: 'erase' }),
};

// ── native bits, when we are running inside the desktop shell ─────────

export const desktop = {
  pickFolder: (start?: string) => invoke<string | null>('pick_folder', { start }) ?? Promise.resolve(null),
  openPath: (path: string) => invoke<void>('open_path', { path }) ?? Promise.resolve(),
  notify: (title: string, body: string) =>
    invoke<void>('notify', { title, body })?.catch(() => {}) ?? Promise.resolve(),
  setLoginItem: (enabled: boolean) =>
    invoke<boolean>('set_login_item', { enabled }) ?? Promise.resolve(enabled),
  loginItemEnabled: () => invoke<boolean>('login_item_enabled') ?? Promise.resolve(false),
};
