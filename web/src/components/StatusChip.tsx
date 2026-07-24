import { normalizeStatus, statusLabel, type StatusKind } from "../lib/status";

type Props = {
  status: string;
  title?: string;
  channeling?: boolean;
};

const CLASS: Record<StatusKind, string> = {
  bound: "chip-bound",
  sealed: "chip-sealed",
  cut: "chip-cut",
  fallen: "chip-fallen",
  channeling: "chip-channeling",
};

export function StatusChip({ status, title, channeling }: Props) {
  const kind = channeling ? "channeling" : normalizeStatus(status);
  return (
    <span className={`chip ${CLASS[kind]}`} title={title}>
      <span className="chip-dot" aria-hidden />
      {statusLabel(kind)}
    </span>
  );
}
