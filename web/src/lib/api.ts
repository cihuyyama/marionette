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
  quota_kind: "tokens" | "none" | string;
};

export type ImportResult = {
  inserted: number;
  updated: number;
  skipped: number;
};

export type ModelObject = {
  id: string;
  object: string;
  owned_by: string;
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

export function getHealth(settings?: Settings) {
  return request<{ status: string; service?: string; version?: string }>(
    "/health",
    { auth: "none" },
    settings,
  );
}

export function getStats(settings?: Settings) {
  return request<PoolStats>("/admin/stats", { auth: "admin" }, settings);
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
) {
  const q = new URLSearchParams();
  if (params?.provider) q.set("provider", params.provider);
  if (params?.status) q.set("status", params.status);
  const qs = q.toString();
  return request<{ accounts: Account[] }>(
    `/admin/accounts${qs ? `?${qs}` : ""}`,
    { auth: "admin" },
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

export function importAccounts(body: unknown, settings?: Settings) {
  return request<ImportResult>(
    "/admin/accounts",
    {
      method: "POST",
      body: JSON.stringify(body),
      auth: "admin",
    },
    settings,
  );
}

export function listModels(settings?: Settings) {
  return request<{ object: string; data: ModelObject[] }>(
    "/v1/models",
    { auth: "pool" },
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
};

export type UsageSummary = {
  requests: number;
  success: number;
  errors: number;
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
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
  params?: { provider?: string; limit?: number },
  settings?: Settings,
) {
  const q = new URLSearchParams();
  if (params?.provider) q.set("provider", params.provider);
  if (params?.limit != null) q.set("limit", String(params.limit));
  const qs = q.toString();
  return request<{ requests: RequestLog[] }>(
    `/admin/requests${qs ? `?${qs}` : ""}`,
    { auth: "admin" },
    settings,
  );
}

export function getUsage(settings?: Settings) {
  return request<UsageSummary>("/admin/usage", { auth: "admin" }, settings);
}

export type LoadBalanceStrategy =
  | "round_robin"
  | "sequential"
  | "least_used"
  | "priority"
  | "random";

export type ProviderSetting = {
  provider: string;
  load_balance: LoadBalanceStrategy | string;
  load_balance_label: string;
  sticky_account_id: string | null;
  rr_cursor: string | null;
  updated_at: string;
};

export type LoadBalanceOption = {
  id: LoadBalanceStrategy | string;
  label: string;
  hint: string;
};

export function listProviderSettings(settings?: Settings) {
  return request<{
    providers: ProviderSetting[];
    strategies: LoadBalanceOption[];
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
