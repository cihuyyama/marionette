export const PROVIDERS = [
  "grok-cli",
  "qoder",
  "commandcode",
  "cline",
  "antigravity",
  "kiro",
  "cb",
  "cbcn",
  "workbuddy",
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
    value === "cb" ||
    value === "cbcn" ||
    value === "workbuddy" ||
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
  // Display names follow the upstream manifest rather than the bare ids, which
  // are too terse to tell apart in a nav: `cb` and `cbcn` differ only by a
  // suffix.
  if (provider === "cb") return "CodeBuddy";
  if (provider === "cbcn") return "CodeBuddy CN";
  if (provider === "workbuddy") return "WorkBuddy";
  if (provider === "byok") return "Custom (BYOK)";
  return provider;
}
