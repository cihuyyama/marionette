const KEY = "marionette.admin.settings.v1";

export type Settings = {
  baseUrl: string;
  adminKey: string;
  poolKey: string;
};

const DEFAULTS: Settings = {
  baseUrl: "http://127.0.0.1:1940",
  adminKey: "",
  poolKey: "",
};

export function loadSettings(): Settings {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return { ...DEFAULTS };
    const parsed = JSON.parse(raw) as Partial<Settings>;
    return {
      baseUrl: parsed.baseUrl?.trim() || DEFAULTS.baseUrl,
      adminKey: parsed.adminKey ?? "",
      poolKey: parsed.poolKey ?? "",
    };
  } catch {
    return { ...DEFAULTS };
  }
}

export function saveSettings(s: Settings): void {
  localStorage.setItem(
    KEY,
    JSON.stringify({
      baseUrl: s.baseUrl.trim() || DEFAULTS.baseUrl,
      adminKey: s.adminKey,
      poolKey: s.poolKey,
    }),
  );
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
