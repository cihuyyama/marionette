export const PROVIDERS = ["grok-cli", "qoder", "blackbox"] as const;

export type ProviderId = (typeof PROVIDERS)[number];

export function isProviderId(value: string | undefined): value is ProviderId {
  return value === "grok-cli" || value === "qoder" || value === "blackbox";
}

export function labelProvider(provider: string): string {
  if (provider === "grok-cli") return "Grok CLI";
  if (provider === "qoder") return "Qoder";
  if (provider === "blackbox") return "Blackbox";
  return provider;
}
