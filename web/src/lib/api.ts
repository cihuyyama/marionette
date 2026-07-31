import { apiUrl, loadSettings, type Settings } from "./settings";

export type PoolStats = {
  total: number;
  bound: number;
  sealed: number;
  cut: number;
  fallen: number;
  by_provider?: Record<
    string,
    { total: number; bound: number; sealed: number; cut: number; fallen: number }
  >;
};

export type Account = {
  id: string;
  provider: string;
  email: string | null;
  name: string | null;
  is_active: boolean;
  priority: number;
  data: Record<string, unknown>;
  cooldown_until: string | null;
  last_error: string | null;
  last_used_at: string | null;
  created_at: string;
  updated_at: string;
  status: string;
  quota_limit: number;
  quota_remaining: number;
  quota_kind: "tokens" | "credits" | "none" | string;
};

export type ImportResult = {
  inserted: number;
  updated: number;
  skipped: number;
  deleted?: number;
  parsed?: number;
  source?: string;
};

export type ModelObject = {
  id: string;
  object: string;
  owned_by: string;
  model_key?: string | null;
  display_name?: string | null;
  credit_usage_rate?: string | null;
  max_input?: string | null;
  reasoning?: boolean;
  vision?: boolean;
  is_default?: boolean;
};

export class ApiError extends Error {
  status: number;
  body: unknown;

  constructor(message: string, status: number, body: unknown) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.body = body;
  }
}

async function parseBody(res: Response): Promise<unknown> {
  const text = await res.text();
  if (!text) return null;
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return text;
  }
}

function errMessage(body: unknown, fallback: string): string {
  if (typeof body === "string" && body) return body;
  if (body && typeof body === "object") {
    const o = body as Record<string, unknown>;
    if (typeof o.error === "string") return o.error;
    if (o.error && typeof o.error === "object") {
      const e = o.error as Record<string, unknown>;
      if (typeof e.message === "string") return e.message;
    }
    if (typeof o.message === "string") return o.message;
  }
  return fallback;
}

async function request<T>(
  path: string,
  init: RequestInit & { auth?: "admin" | "pool" | "none" } = {},
  settings?: Settings,
): Promise<T> {
  const s = settings ?? loadSettings();
  const auth = init.auth ?? "admin";
  const headers = new Headers(init.headers);
  if (!headers.has("Content-Type") && init.body) {
    headers.set("Content-Type", "application/json");
  }
  if (auth === "admin" && s.adminKey) {
    headers.set("Authorization", `Bearer ${s.adminKey}`);
  }
  if (auth === "pool" && s.poolKey) {
    headers.set("Authorization", `Bearer ${s.poolKey}`);
  }

  const { auth: _a, ...rest } = init;
  const res = await fetch(apiUrl(path, s.baseUrl), { ...rest, headers });
  const body = await parseBody(res);
  if (!res.ok) {
    if (res.status === 401 && auth === "admin" && typeof window !== "undefined") {
      window.dispatchEvent(new Event("marionette-unauthorized"));
    }
    throw new ApiError(
      errMessage(body, `${res.status} ${res.statusText}`),
      res.status,
      body,
    );
  }
  return body as T;
}

export function getHealth(settings?: Settings, signal?: AbortSignal) {
  return request<{ status: string; service?: string; version?: string }>(
    "/health",
    { auth: "none", signal },
    settings,
  );
}

export function getStats(settings?: Settings, signal?: AbortSignal) {
  return request<PoolStats>("/admin/stats", { auth: "admin", signal }, settings);
}

export type ConnectionInfo = {
  host: string;
  port: number;
  base_url: string;
  pool_key: string;
  cors_origin: string;
};

export function getConnection(settings?: Settings) {
  return request<ConnectionInfo>("/admin/connection", { auth: "admin" }, settings);
}

export function listAccounts(
  params?: { provider?: string; status?: string },
  settings?: Settings,
  signal?: AbortSignal,
) {
  const q = new URLSearchParams();
  if (params?.provider) q.set("provider", params.provider);
  if (params?.status) q.set("status", params.status);
  const qs = q.toString();
  return request<{ accounts: Account[] }>(
    `/admin/accounts${qs ? `?${qs}` : ""}`,
    { auth: "admin", signal },
    settings,
  );
}

export function getAccount(id: string, settings?: Settings) {
  return request<Account>(`/admin/accounts/${encodeURIComponent(id)}`, {
    auth: "admin",
  }, settings);
}

export function patchAccount(
  id: string,
  body: {
    is_active?: boolean;
    priority?: number;
    clear_cooldown?: boolean;
    name?: string;
    email?: string;
    quota_limit?: number;
    quota_remaining?: number;
    reset_quota?: boolean;
  },
  settings?: Settings,
) {
  return request<Account>(
    `/admin/accounts/${encodeURIComponent(id)}`,
    {
      method: "PATCH",
      body: JSON.stringify(body),
      auth: "admin",
    },
    settings,
  );
}

export function deleteAccount(id: string, settings?: Settings) {
  return request<{ ok: boolean; id: string }>(
    `/admin/accounts/${encodeURIComponent(id)}`,
    { method: "DELETE", auth: "admin" },
    settings,
  );
}

export function refreshAccount(id: string, settings?: Settings) {
  return request<Account>(
    `/admin/accounts/${encodeURIComponent(id)}/refresh`,
    { method: "POST", auth: "admin" },
    settings,
  );
}

export type RefreshJob = {
  id: string;
  provider: string;
  status: "running" | "succeeded" | "cancelled";
  force: boolean;
  concurrency: number;
  created_at: string;
  started_at?: string | null;
  finished_at?: string | null;
  total: number;
  processed: number;
  ok: number;
  failed: number;
  cut: number;
  error?: string | null;
  last_email?: string | null;
};

export function refreshAllAccounts(
  opts?: { provider?: string; force?: boolean; concurrency?: number },
  settings?: Settings,
) {
  return request<RefreshJob>(
    `/admin/accounts/refresh-all`,
    {
      method: "POST",
      auth: "admin",
      body: JSON.stringify({
        provider: opts?.provider ?? "grok-cli",
        force: opts?.force ?? true,
        concurrency: opts?.concurrency,
      }),
    },
    settings,
  );
}

export function getRefreshJob(id: string, settings?: Settings, signal?: AbortSignal) {
  return request<RefreshJob>(
    `/admin/refresh/jobs/${encodeURIComponent(id)}`,
    { auth: "admin", signal },
    settings,
  );
}

export function cancelRefreshAll(settings?: Settings) {
  return request<{ cancelling: boolean; id: string }>(
    `/admin/refresh/cancel`,
    { method: "POST", auth: "admin" },
    settings,
  );
}

export function getRefreshStatus(settings?: Settings, signal?: AbortSignal) {
  return request<{ current: RefreshJob | null; history: RefreshJob[] }>(
    `/admin/refresh`,
    { auth: "admin", signal },
    settings,
  );
}

export type ClaimTrialResult = {
  ok: boolean;
  http_status: number;
  message: string;
  upstream?: Record<string, unknown> | unknown;
  quota_synced?: boolean;
  sync_error?: string | null;
  account?: Account;
  account_id?: string;
  path?: string;
};

export function claimProTrial(id: string, settings?: Settings) {
  return request<ClaimTrialResult>(
    `/admin/accounts/${encodeURIComponent(id)}/claim-trial`,
    { method: "POST", auth: "admin" },
    settings,
  );
}

export type InjectJobStatus =
  | "queued"
  | "running"
  | "succeeded"
  | "failed"
  | "cancelled"
  | string;

export type InjectJob = {
  id: string;
  kind: string;
  status: string;
  created_at?: string;
  started_at?: string | null;
  finished_at?: string | null;
  exit_code?: number | null;
  error?: string | null;
  account_id: string;
  account_ids?: string[];
  bulk?: boolean;
  bulk_total?: number;
  bulk_ok?: number;
  bulk_fail?: number;
  email?: string | null;
  headless: boolean;
  refresh: boolean;
  work_dir?: string;
  log_path?: string;
  current_step?: string | null;
  inject_result?: Record<string, unknown> | null;
  log_count: number;
};

export type InjectEvent = {
  seq: number;
  ts: string;
  line: string;
  parsed?: Record<string, unknown> | null;
};

export type InjectStartResult = {
  ok: boolean;
  job: InjectJob;
};

export type InjectRefreshResult = {
  ok: boolean;
  refreshed: boolean;
  refresh_error?: string | null;
  account: Account;
  skipped?: boolean;
  accounts_refreshed?: number;
  accounts_failed?: number;
  accounts_targeted?: number;
};

export function startInjectJob(
  accountId: string,
  opts?: { headless?: boolean; refresh?: boolean },
  settings?: Settings,
) {
  const qs = new URLSearchParams();
  if (opts?.headless === false) qs.set("headless", "false");
  if (opts?.headless === true) qs.set("headless", "true");
  if (opts?.refresh === false) qs.set("refresh", "false");
  if (opts?.refresh === true) qs.set("refresh", "true");
  const q = qs.toString();
  return request<InjectStartResult>(
    `/admin/accounts/${encodeURIComponent(accountId)}/inject${q ? `?${q}` : ""}`,
    { method: "POST", auth: "admin" },
    settings,
  );
}

export type BulkInjectStartResult = InjectStartResult & {
  total?: number;
  only_no_credit?: boolean;
  skipped_no_pat?: number;
  skipped_inactive?: number;
  skipped_has_credit?: number;
};

export function startBulkInjectJob(
  opts?: {
    accountIds?: string[];
    includeInactive?: boolean;
    onlyNoCredit?: boolean;
    headless?: boolean;
    refresh?: boolean;
  },
  settings?: Settings,
) {
  return request<BulkInjectStartResult>(
    `/admin/providers/qoder/inject`,
    {
      method: "POST",
      auth: "admin",
      body: JSON.stringify({
        account_ids: opts?.accountIds,
        include_inactive: opts?.includeInactive ?? false,
        only_no_credit: opts?.onlyNoCredit ?? true,
        headless: opts?.headless ?? true,
        refresh: opts?.refresh ?? true,
      }),
    },
    settings,
  );
}

export function getInjectJob(id: string, settings?: Settings) {
  return request<{ job: InjectJob; events: InjectEvent[] }>(
    `/admin/inject/jobs/${encodeURIComponent(id)}`,
    { auth: "admin" },
    settings,
  );
}

export function getInjectEvents(id: string, after = 0, settings?: Settings) {
  return request<{ job: InjectJob; events: InjectEvent[]; after: number }>(
    `/admin/inject/jobs/${encodeURIComponent(id)}/events?after=${after}`,
    { auth: "admin" },
    settings,
  );
}

export function cancelInjectJob(id: string, settings?: Settings) {
  return request<{ ok: boolean; job: InjectJob }>(
    `/admin/inject/jobs/${encodeURIComponent(id)}/cancel`,
    { method: "POST", auth: "admin" },
    settings,
  );
}

export function refreshAfterInject(id: string, settings?: Settings) {
  return request<InjectRefreshResult>(
    `/admin/inject/jobs/${encodeURIComponent(id)}/refresh`,
    { method: "POST", auth: "admin" },
    settings,
  );
}

export type WarmupResultRow = {
  id: string;
  email?: string | null;
  ok: boolean;
  cut?: boolean;
  error?: string;
  quota_limit?: number;
  quota_remaining?: number;
};

export type WarmupResult = {
  provider: string;
  total: number;
  ok: number;
  failed: number;
  cut: number;
  results: WarmupResultRow[];
};

export function warmupQoderAccounts(
  opts?: { include_inactive?: boolean; concurrency?: number },
  settings?: Settings,
) {
  const qs = new URLSearchParams();
  if (opts?.include_inactive) qs.set("include_inactive", "true");
  if (opts?.concurrency != null) qs.set("concurrency", String(opts.concurrency));
  const q = qs.toString();
  return request<WarmupResult>(
    `/admin/providers/qoder/warmup${q ? `?${q}` : ""}`,
    { method: "POST", auth: "admin" },
    settings,
  );
}

export function importAccounts(body: unknown, replace = false, settings?: Settings) {
  const qs = replace ? "?replace=true" : "";
  return request<ImportResult>(
    `/admin/accounts${qs}`,
    {
      method: "POST",
      body: JSON.stringify(body),
      auth: "admin",
    },
    settings,
  );
}

export function importAccountsFile(
  file: Blob,
  replace = false,
  settings?: Settings,
) {
  const qs = replace ? "?replace=true" : "";
  return request<ImportResult>(
    `/admin/accounts${qs}`,
    {
      method: "POST",
      body: file,
      headers: { "Content-Type": "application/json" },
      auth: "admin",
    },
    settings,
  );
}

export function listModels(settings?: Settings, signal?: AbortSignal) {
  return request<{ object: string; data: ModelObject[] }>(
    "/v1/models",
    { auth: "pool", signal },
    settings,
  );
}

export function listAdminModels(settings?: Settings) {
  return request<{ object: string; data: ModelObject[] }>(
    "/admin/models",
    { auth: "admin" },
    settings,
  );
}

export type RequestLog = {
  id: string;
  created_at: string;
  provider: string;
  model: string | null;
  status: string;
  stream: boolean;
  duration_ms: number | null;
  prompt_tokens: number | null;
  completion_tokens: number | null;
  total_tokens: number | null;
  credits_used: number | null;
  account_quota_before: number | null;
  account_quota_after: number | null;
  account_id: string | null;
  account_email: string | null;
  error_message: string | null;
  request_body?: unknown;
  response_body?: unknown;
};

export type UsageRange = "day" | "week" | "month" | "all";

export type UsageSummary = {
  requests: number;
  success: number;
  errors: number;
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  range?: UsageRange | string;
  since?: string | null;
  by_model: {
    model: string;
    provider: string;
    requests: number;
    prompt_tokens: number;
    completion_tokens: number;
    total_tokens: number;
  }[];
};

export function listRequests(
  params?: { provider?: string; limit?: number; range?: UsageRange | string },
  settings?: Settings,
  signal?: AbortSignal,
) {
  const q = new URLSearchParams();
  if (params?.provider) q.set("provider", params.provider);
  if (params?.limit != null) q.set("limit", String(params.limit));
  if (params?.range && params.range !== "all") q.set("range", params.range);
  const qs = q.toString();
  return request<{
    requests: RequestLog[];
    range?: string;
    since?: string | null;
  }>(`/admin/requests${qs ? `?${qs}` : ""}`, { auth: "admin", signal }, settings);
}

export function getRequestDetail(id: string, settings?: Settings) {
  return request<RequestLog>(
    `/admin/requests/${encodeURIComponent(id)}`,
    { auth: "admin" },
    settings,
  );
}

export function getUsage(
  params?: { range?: UsageRange | string },
  settings?: Settings,
  signal?: AbortSignal,
) {
  const q = new URLSearchParams();
  if (params?.range && params.range !== "all") q.set("range", params.range);
  const qs = q.toString();
  return request<UsageSummary>(
    `/admin/usage${qs ? `?${qs}` : ""}`,
    { auth: "admin", signal },
    settings,
  );
}

export type LoadBalanceStrategy =
  | "round_robin"
  | "sequential"
  | "least_used"
  | "priority"
  | "random";

export type QoderPickMode = "normal" | "ultimate_free";

export type ProviderSetting = {
  provider: string;
  load_balance: LoadBalanceStrategy | string;
  load_balance_label: string;
  pick_mode?: QoderPickMode | string;
  pick_mode_label?: string;
  sticky_account_id: string | null;
  rr_cursor: string | null;
  updated_at: string;
};

export type LoadBalanceOption = {
  id: LoadBalanceStrategy | string;
  label: string;
  hint: string;
};

export type PickModeOption = {
  id: QoderPickMode | string;
  label: string;
  hint: string;
};

export function listProviderSettings(settings?: Settings) {
  return request<{
    providers: ProviderSetting[];
    strategies: LoadBalanceOption[];
    pick_modes?: PickModeOption[];
  }>("/admin/providers", { auth: "admin" }, settings);
}

export function patchProviderLoadBalance(
  provider: string,
  load_balance: string,
  settings?: Settings,
) {
  return request<ProviderSetting>(
    `/admin/providers/${encodeURIComponent(provider)}`,
    {
      method: "PATCH",
      body: JSON.stringify({ load_balance }),
      auth: "admin",
    },
    settings,
  );
}

export function patchProviderPickMode(
  provider: string,
  pick_mode: string,
  settings?: Settings,
) {
  return request<ProviderSetting>(
    `/admin/providers/${encodeURIComponent(provider)}`,
    {
      method: "PATCH",
      body: JSON.stringify({ pick_mode }),
      auth: "admin",
    },
    settings,
  );
}

export function chatCompletion(
  body: {
    model: string;
    messages: { role: string; content: string }[];
    stream?: boolean;
  },
  settings?: Settings,
) {
  return request<Record<string, unknown>>(
    "/v1/chat/completions",
    {
      method: "POST",
      body: JSON.stringify({ ...body, stream: false }),
      auth: "pool",
    },
    settings,
  );
}

export type FarmJobStatus =
  | "queued"
  | "running"
  | "succeeded"
  | "failed"
  | "cancelled"
  | string;

export type FarmJob = {
  id: string;
  provider?: string;
  status: FarmJobStatus;
  created_at: string;
  started_at?: string | null;
  finished_at?: string | null;
  exit_code?: number | null;
  error?: string | null;
  accounts_count: number;
  inject: boolean;
  headless: boolean;
  device_auth: boolean;
  concurrency?: number;
  settle_secs?: number | null;
  auto_import: boolean;
  skip_existing?: boolean;
  account_delay?: number;
  output_path: string;
  work_dir: string;
  ok: number;
  fail: number;
  total: number;
  current_step?: string | null;
  current_email?: string | null;
  import_result?: Record<string, unknown> | null;
  log_count: number;
};

export type FarmEvent = {
  seq: number;
  ts: string;
  line: string;
  parsed?: Record<string, unknown> | null;
};

export type FarmPackageInfo = {
  package_dir: string;
  package_parent: string;
  package_present: boolean;
  module?: string;
  run_hint?: string;
};

export type FarmInfo = {
  provider: string;
  package_dir: string;
  package_parent: string;
  python: string;
  data_dir: string;
  package_present: boolean;
  max_concurrency?: number;
  run_hint: string;
  packages?: {
    qoder?: FarmPackageInfo;
    "grok-cli"?: FarmPackageInfo;
  };
};

export type FarmStatus = {
  info: FarmInfo;
  current: FarmJob | null;
  history: FarmJob[];
  busy: boolean;
};

export function getFarmStatus(settings?: Settings) {
  return request<FarmStatus>("/admin/farm", { auth: "admin" }, settings);
}

export function startFarmJob(
  body: {
    accounts: string;
    default_password?: string | null;
    provider?: string;
    inject?: boolean;
    headless?: boolean;
    device_auth?: boolean;
    skip_exchange?: boolean;
    settle_secs?: number | null;
    auto_import?: boolean;
    concurrency?: number;
    account_retries?: number;
    skip_existing?: boolean;
    account_delay?: number;
    proxy_file?: string | null;
  },
  settings?: Settings,
) {
  return request<{ ok: boolean; job: FarmJob }>(
    "/admin/farm/start",
    {
      method: "POST",
      body: JSON.stringify(body),
      auth: "admin",
    },
    settings,
  );
}

export function retryFailedFarmJob(
  id: string,
  body?: {
    provider?: string;
    inject?: boolean;
    headless?: boolean;
    device_auth?: boolean;
    skip_exchange?: boolean;
    settle_secs?: number | null;
    auto_import?: boolean;
    concurrency?: number;
    account_retries?: number;
    skip_existing?: boolean;
    account_delay?: number;
    proxy_file?: string | null;
  },
  settings?: Settings,
) {
  return request<{
    ok: boolean;
    job: FarmJob;
    retried_from?: string;
    failed_count?: number;
  }>(
    `/admin/farm/jobs/${encodeURIComponent(id)}/retry-failed`,
    {
      method: "POST",
      body: JSON.stringify(body ?? {}),
      auth: "admin",
    },
    settings,
  );
}

export function getFarmJob(id: string, settings?: Settings) {
  return request<{ job: FarmJob; events: FarmEvent[] }>(
    `/admin/farm/jobs/${encodeURIComponent(id)}`,
    { auth: "admin" },
    settings,
  );
}

export function getFarmEvents(id: string, after = 0, settings?: Settings) {
  return request<{ job: FarmJob; events: FarmEvent[]; after: number }>(
    `/admin/farm/jobs/${encodeURIComponent(id)}/events?after=${after}`,
    { auth: "admin" },
    settings,
  );
}

export function cancelFarmJob(id: string, settings?: Settings) {
  return request<{ ok: boolean; job: FarmJob }>(
    `/admin/farm/jobs/${encodeURIComponent(id)}/cancel`,
    { method: "POST", auth: "admin" },
    settings,
  );
}

export function importFarmJob(id: string, settings?: Settings) {
  return request<ImportResult & { ok?: boolean; path?: string; source?: string }>(
    `/admin/farm/jobs/${encodeURIComponent(id)}/import`,
    { method: "POST", auth: "admin" },
    settings,
  );
}

export type ProxyHealth = "ok" | "dead" | "unknown";

export type Proxy = {
  id: string;
  scheme: string;
  host: string;
  port: number;
  username?: string | null;
  has_auth: boolean;
  label?: string | null;
  country?: string | null;
  is_active: boolean;
  health: ProxyHealth;
  latency_ms?: number | null;
  last_check_at?: string | null;
  last_error?: string | null;
  source?: string | null;
  assigned_count: number;
  created_at: string;
  updated_at: string;
};

export type ProxyChatMode = "off" | "follow-account" | "rotating";
export type ProxyAutomationMode = "off" | "sticky" | "rotating";
export type ProxyOnDead = "direct" | "reassign" | "fail";

export type ProxySettings = {
  chat_mode: ProxyChatMode;
  automation_mode: ProxyAutomationMode;
  on_dead: ProxyOnDead;
  updated_at?: string;
};

export type ProxyListResult = {
  proxies: Proxy[];
  total: number;
  healthy: number;
  settings: ProxySettings;
};

export function listProxies(settings?: Settings, signal?: AbortSignal) {
  return request<ProxyListResult>(
    `/admin/proxies`,
    { auth: "admin", signal },
    settings,
  );
}

export function importProxies(
  body: {
    text?: string;
    proxies?: string[];
    scheme?: string;
    country?: string;
    label?: string;
    source?: string;
  },
  settings?: Settings,
) {
  return request<{ inserted: number; updated: number; skipped: number }>(
    `/admin/proxies`,
    { method: "POST", auth: "admin", body: JSON.stringify(body) },
    settings,
  );
}

export function deleteProxy(id: string, settings?: Settings) {
  return request<{ deleted: boolean; id: string }>(
    `/admin/proxies/${encodeURIComponent(id)}`,
    { method: "DELETE", auth: "admin" },
    settings,
  );
}

export function toggleProxy(id: string, isActive: boolean, settings?: Settings) {
  return request<Proxy>(
    `/admin/proxies/${encodeURIComponent(id)}`,
    { method: "PATCH", auth: "admin", body: JSON.stringify({ is_active: isActive }) },
    settings,
  );
}

export function checkAllProxies(settings?: Settings) {
  return request<{ checked: number; healthy: number; dead: number }>(
    `/admin/proxies/check`,
    { method: "POST", auth: "admin" },
    settings,
  );
}

export function checkProxy(id: string, settings?: Settings) {
  return request<Proxy>(
    `/admin/proxies/${encodeURIComponent(id)}/check`,
    { method: "POST", auth: "admin" },
    settings,
  );
}

export function assignProxies(provider: string, settings?: Settings) {
  return request<{ provider: string; assigned: number }>(
    `/admin/proxies/assign`,
    { method: "POST", auth: "admin", body: JSON.stringify({ provider }) },
    settings,
  );
}

export function getProxySettings(settings?: Settings) {
  return request<ProxySettings>(
    `/admin/proxies/settings`,
    { auth: "admin" },
    settings,
  );
}

export function updateProxySettings(
  body: Partial<Pick<ProxySettings, "chat_mode" | "automation_mode" | "on_dead">>,
  settings?: Settings,
) {
  return request<ProxySettings>(
    `/admin/proxies/settings`,
    { method: "PATCH", auth: "admin", body: JSON.stringify(body) },
    settings,
  );
}

