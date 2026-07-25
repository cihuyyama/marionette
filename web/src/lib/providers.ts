export const PROVIDERS = ["grok-cli", "qoder"] as const;

export type ProviderId = (typeof PROVIDERS)[number];

export function isProviderId(value: string | undefined): value is ProviderId {
  return value === "grok-cli" || value === "qoder";
}

export function labelProvider(provider: string): string {
  if (provider === "grok-cli") return "Grok CLI";
  if (provider === "qoder") return "Qoder";
  return provider;
}
