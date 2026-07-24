export type StatusKind = "bound" | "sealed" | "cut" | "fallen" | "channeling";

const LABELS: Record<StatusKind, string> = {
  bound: "Bound",
  sealed: "Sealed",
  cut: "Cut",
  fallen: "Fallen",
  channeling: "Channeling",
};

export function normalizeStatus(raw: string | undefined | null): StatusKind {
  const s = (raw ?? "bound").toLowerCase();
  if (s === "bound" || s === "sealed" || s === "cut" || s === "fallen" || s === "channeling") {
    return s;
  }
  return "bound";
}

export function statusLabel(raw: string | undefined | null): string {
  return LABELS[normalizeStatus(raw)];
}

export function statusTooltip(account: {
  status: string;
  is_active: boolean;
  cooldown_until: string | null;
  last_error: string | null;
}): string {
  const parts = [
    `status: ${account.status}`,
    `is_active: ${account.is_active}`,
  ];
  if (account.cooldown_until) {
    parts.push(`cooldown_until: ${account.cooldown_until}`);
  }
  if (account.last_error) {
    parts.push(`last_error: ${account.last_error}`);
  }
  return parts.join("\n");
}
