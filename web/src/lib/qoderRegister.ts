const KEY = "marionette.qoder.register.preset.v1";

export type QoderRegisterMethod = "imap" | "plus_trick";
export type QoderCaptchaMode = "auto" | "manual" | "auto-then-manual";

export type QoderRegisterPreset = {
  method: QoderRegisterMethod;
  count: number;
  concurrency: number;
  headless: boolean;
  autoImport: boolean;
  inject: boolean;
  captchaMode: QoderCaptchaMode;
  domain: string;
  gmailBase: string;
  imapHost: string;
  imapUser: string;
  savePasswords: boolean;
  password: string;
  imapPass: string;
};

export const QODER_REGISTER_DEFAULTS: QoderRegisterPreset = {
  method: "imap",
  count: 5,
  concurrency: 1,
  headless: false,
  autoImport: true,
  inject: false,
  captchaMode: "auto",
  domain: "",
  gmailBase: "",
  imapHost: "imap.gmail.com",
  imapUser: "",
  savePasswords: true,
  password: "",
  imapPass: "",
};

function isCaptchaMode(v: unknown): v is QoderCaptchaMode {
  return v === "auto" || v === "manual" || v === "auto-then-manual";
}

export function loadQoderRegisterPreset(): QoderRegisterPreset {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return { ...QODER_REGISTER_DEFAULTS };
    const p = JSON.parse(raw) as Partial<QoderRegisterPreset>;
    return {
      method: p.method === "plus_trick" ? "plus_trick" : "imap",
      count: clampInt(p.count, QODER_REGISTER_DEFAULTS.count, 1, 100),
      concurrency: clampInt(p.concurrency, QODER_REGISTER_DEFAULTS.concurrency, 1, 64),
      headless: p.headless ?? QODER_REGISTER_DEFAULTS.headless,
      autoImport: p.autoImport ?? QODER_REGISTER_DEFAULTS.autoImport,
      inject: p.inject ?? QODER_REGISTER_DEFAULTS.inject,
      captchaMode: isCaptchaMode(p.captchaMode) ? p.captchaMode : QODER_REGISTER_DEFAULTS.captchaMode,
      domain: p.domain ?? "",
      gmailBase: p.gmailBase ?? "",
      imapHost: p.imapHost?.trim() || QODER_REGISTER_DEFAULTS.imapHost,
      imapUser: p.imapUser ?? "",
      savePasswords: p.savePasswords ?? QODER_REGISTER_DEFAULTS.savePasswords,
      password: p.password ?? "",
      imapPass: p.imapPass ?? "",
    };
  } catch {
    return { ...QODER_REGISTER_DEFAULTS };
  }
}

export function saveQoderRegisterPreset(p: QoderRegisterPreset): void {
  const body = {
    method: p.method,
    count: p.count,
    concurrency: p.concurrency,
    headless: p.headless,
    autoImport: p.autoImport,
    inject: p.inject,
    captchaMode: p.captchaMode,
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

export function clearQoderRegisterPreset(): void {
  localStorage.removeItem(KEY);
}

function clampInt(v: unknown, fallback: number, min: number, max: number): number {
  const n = typeof v === "number" ? Math.floor(v) : Number(v);
  if (!Number.isFinite(n)) return fallback;
  return Math.max(min, Math.min(max, n));
}
