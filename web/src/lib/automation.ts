import { labelProvider, type ProviderId } from "./providers";

export type AutomationMethodId = "google-sso" | "imap" | "relogin" | "register";

export type AutomationAvailability = "ready" | "coming_soon";

export type AutomationMethod = {
  id: AutomationMethodId;
  label: string;
  description: string;
  status: AutomationAvailability;
};

export type AutomationProvider = {
  id: ProviderId;
  label: string;
  blurb: string;
  status: AutomationAvailability;
  methods: AutomationMethod[];
};

export const AUTOMATION_PROVIDERS: AutomationProvider[] = [
  {
    id: "qoder",
    label: labelProvider("qoder"),
    blurb: "GSuite → PAT → inject → pool import",
    status: "ready",
    methods: [
      {
        id: "google-sso",
        label: "Google SSO",
        description:
          "GSuite password login (no OTP). Browser farm under scripts/automation/qoder_farm.",
        status: "ready",
      },
      {
        id: "register",
        label: "Register",
        description:
          "Signup new accounts: email + Aliyun slide captcha → IMAP OTP → PAT → optional inject → pool. Camoufox under scripts/automation/qoder_farm.",
        status: "ready",
      },
      {
        id: "imap",
        label: "IMAP",
        description:
          "CF Email Routing domain mailboxes for verification mail. Not wired yet.",
        status: "coming_soon",
      },
    ],
  },
  {
    id: "grok-cli",
    label: labelProvider("grok-cli"),
    blurb: "Manual thin/mass OAuth relogin → pool import",
    status: "ready",
    methods: [
      {
        id: "relogin",
        label: "Relogin",
        description:
          "Email+password → OAuth PKCE → verify chat → grok-cli pool. Camoufox under scripts/automation/grok_farm.",
        status: "ready",
      },
      {
        id: "register",
        label: "Register",
        description:
          "Signup new accounts: Castle + Turnstile → Device Flow → tokens → pool import. Camoufox under scripts/automation/grok_farm.",
        status: "ready",
      },
    ],
  },
  {
    id: "blackbox",
    label: labelProvider("blackbox"),
    blurb: "Signup → temp-mail OTP → sk- API key → pool import",
    status: "ready",
    methods: [
      {
        id: "register",
        label: "Register",
        description:
          "Register + harvest API keys: signup via temp-mail (Cloudflare worker) → OTP → create sk- API key → pool. Playwright Chromium under scripts/automation/blackbox_farm.",
        status: "ready",
      },
    ],
  },
];

export function getAutomationProvider(
  id: string | undefined,
): AutomationProvider | undefined {
  return AUTOMATION_PROVIDERS.find((p) => p.id === id);
}

export function getAutomationMethod(
  providerId: string | undefined,
  methodId: string | undefined,
): { provider: AutomationProvider; method: AutomationMethod } | undefined {
  const provider = getAutomationProvider(providerId);
  if (!provider) return undefined;
  const method = provider.methods.find((m) => m.id === methodId);
  if (!method) return undefined;
  return { provider, method };
}

export function isReadyFarm(
  providerId: string | undefined,
  methodId: string | undefined,
): boolean {
  const hit = getAutomationMethod(providerId, methodId);
  return Boolean(
    hit &&
      hit.provider.status === "ready" &&
      hit.method.status === "ready",
  );
}

export function methodLabel(id: AutomationMethodId | string): string {
  if (id === "google-sso") return "Google SSO";
  if (id === "imap") return "IMAP";
  if (id === "relogin") return "Relogin";
  if (id === "register") return "Register";
  return id;
}

export function farmLivePath(provider?: string | null): string {
  if (provider === "grok-cli") return "/automation/grok-cli/relogin";
  if (provider === "blackbox") return "/automation/blackbox/register";
  return "/automation/qoder/google-sso";
}
