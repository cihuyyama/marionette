const KEY = "marionette.grok.register.preset.v1";

export type GrokRegisterMethod = "imap" | "plus_trick" | "temp_mail";

export type GrokRegisterPreset = {
  method: GrokRegisterMethod;
  count: number;
  concurrency: number;
  headless: boolean;
  autoImport: boolean;
  domain: string;
  gmailBase: string;
  imapHost: string;
  imapUser: string;
  savePasswords: boolean;
  // Only persisted when savePasswords is true.
  password: string;
  imapPass: string;
};

export const GROK_REGISTER_DEFAULTS: GrokRegisterPreset = {
  method: "temp_mail",
  count: 5,
  concurrency: 1,
  headless: false,
  autoImport: true,
  domain: "",
  gmailBase: "",
  imapHost: "imap.gmail.com",
  imapUser: "",
  savePasswords: true,
  password: "",
  imapPass: "",
};

export function loadGrokRegisterPreset(): GrokRegisterPreset {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return { ...GROK_REGISTER_DEFAULTS };
    const p = JSON.parse(raw) as Partial<GrokRegisterPreset>;
    return {
      method:
        p.method === "temp_mail"
          ? "temp_mail"
          : p.method === "plus_trick"
            ? "plus_trick"
            : "imap",
      count: clampInt(p.count, GROK_REGISTER_DEFAULTS.count, 1, 100),
      concurrency: clampInt(p.concurrency, GROK_REGISTER_DEFAULTS.concurrency, 1, 64),
      headless: p.headless ?? GROK_REGISTER_DEFAULTS.headless,
      autoImport: p.autoImport ?? GROK_REGISTER_DEFAULTS.autoImport,
      domain: p.domain ?? "",
      gmailBase: p.gmailBase ?? "",
      imapHost: p.imapHost?.trim() || GROK_REGISTER_DEFAULTS.imapHost,
      imapUser: p.imapUser ?? "",
      savePasswords: p.savePasswords ?? GROK_REGISTER_DEFAULTS.savePasswords,
      password: p.password ?? "",
      imapPass: p.imapPass ?? "",
    };
  } catch {
    return { ...GROK_REGISTER_DEFAULTS };
  }
}

export function saveGrokRegisterPreset(p: GrokRegisterPreset): void {
  // Secrets persist only when the user opted in via savePasswords.
  const body = {
    method: p.method,
    count: p.count,
    concurrency: p.concurrency,
    headless: p.headless,
    autoImport: p.autoImport,
    domain: p.domain.trim(),
    gmailBase: p.gmailBase.trim(),
    imapHost: p.imapHost.trim(),
    imapUser: p.imapUser.trim(),
    savePasswords: p.savePasswords,
    password: p.savePasswords ? p.password : "",
    imapPass: p.savePasswords ? p.imapPass : "",
  };
  localStorage.setItem(KEY, JSON.stringify(body));
}

export function clearGrokRegisterPreset(): void {
  localStorage.removeItem(KEY);
}

function clampInt(v: unknown, fallback: number, min: number, max: number): number {
  const n = typeof v === "number" ? Math.floor(v) : Number(v);
  if (!Number.isFinite(n)) return fallback;
  return Math.max(min, Math.min(max, n));
}
