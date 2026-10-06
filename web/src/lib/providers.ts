export const PROVIDERS = [
  "grok-cli",
  "qoder",
  "commandcode",
  "cline",
  "antigravity",
  "kiro",
  "byok",
] as const;

export type ProviderId = (typeof PROVIDERS)[number];

export function isProviderId(value: string | undefined): value is ProviderId {
  return (
    value === "grok-cli" ||
    value === "qoder" ||
    value === "commandcode" ||
    value === "cline" ||
    value === "antigravity" ||
    value === "kiro" ||
    value === "byok"
  );
}

export function labelProvider(provider: string): string {
  if (provider === "grok-cli") return "Grok CLI";
  if (provider === "qoder") return "Qoder";
  if (provider === "commandcode") return "Command Code";
  if (provider === "cline") return "Cline";
  if (provider === "antigravity") return "Antigravity";
  if (provider === "kiro") return "Kiro";
  if (provider === "byok") return "Custom (BYOK)";
  return provider;
}
