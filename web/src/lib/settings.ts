const KEY = "marionette.admin.settings.v1";

export type Settings = {
  baseUrl: string;
  adminKey: string;
  poolKey: string;
  adminKeyExpiresAt?: number;
};

const DEFAULTS: Settings = {
  baseUrl: "http://127.0.0.1:1940",
  adminKey: "change-me-admin",
  poolKey: "change-me",
};

export function loadSettings(): Settings {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return { ...DEFAULTS };
    const parsed = JSON.parse(raw) as Partial<Settings>;
    let adminKey = parsed.adminKey || DEFAULTS.adminKey;
    if (parsed.adminKeyExpiresAt && Date.now() > parsed.adminKeyExpiresAt) {
      adminKey = "";
    }
    return {
      baseUrl: parsed.baseUrl?.trim() || DEFAULTS.baseUrl,
      adminKey,
      poolKey: parsed.poolKey || DEFAULTS.poolKey,
      adminKeyExpiresAt: parsed.adminKeyExpiresAt,
    };
  } catch {
    return { ...DEFAULTS };
  }
}

export function saveSettings(s: Settings): void {
  const expiresAt = s.adminKey ? Date.now() + 24 * 60 * 60 * 1000 : undefined;
  localStorage.setItem(
    KEY,
    JSON.stringify({
      baseUrl: s.baseUrl.trim() || DEFAULTS.baseUrl,
      adminKey: s.adminKey,
      poolKey: s.poolKey,
      adminKeyExpiresAt: expiresAt,
    }),
  );
}

export function clearAdminKey(): void {
  const s = loadSettings();
  saveSettings({
    ...s,
    adminKey: "",
  });
}

export function applyPoolKeyFromServer(poolKey: string, baseUrl?: string): Settings {
  const s = loadSettings();
  const next: Settings = {
    ...s,
    poolKey: poolKey || s.poolKey,
  };
  if (baseUrl?.trim()) {
    next.baseUrl = baseUrl.trim();
  }
  saveSettings(next);
  return next;
}

/** Resolve request URL: empty base uses Vite proxy (same origin). */
export function apiUrl(path: string, baseUrl: string): string {
  const p = path.startsWith("/") ? path : `/${path}`;
  const base = baseUrl.trim().replace(/\/$/, "");
  // Dev proxy: leave relative so /admin /v1 /health hit Vite → 1940
  if (!base || base === "http://127.0.0.1:1940" || base === "http://localhost:1940") {
    return p;
  }
  return `${base}${p}`;
}
