import { useEffect, useState } from "react";
import {
  ApiError,
  exportQoderPats,
  type ExportPatItem,
} from "../lib/api";

type Props = {
  open: boolean;
  accountIds: string[];
  onClose: () => void;
};

export function ExportPatModal({ open, accountIds, onClose }: Props) {
  const [items, setItems] = useState<ExportPatItem[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setItems([]);
    setError(null);
    setCopied(null);
    setLoading(true);
    let cancelled = false;
    void (async () => {
      try {
        const res = await exportQoderPats(accountIds);
        if (!cancelled) setItems(res.items);
      } catch (err) {
        if (!cancelled)
          setError(
            err instanceof ApiError
              ? err.message
              : err instanceof Error
                ? err.message
                : "Failed to export PATs",
          );
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [open, accountIds]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  const withPat = items.filter((i) => i.pat);
  const allText = withPat.map((i) => i.pat).join("\n");

  async function copy(text: string, tag: string) {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(tag);
      window.setTimeout(() => setCopied((c) => (c === tag ? null : c)), 1500);
    } catch {
      setError("Clipboard blocked — select the text and copy manually");
    }
  }

  return (
    <>
      <div className="drawer-backdrop" onClick={onClose} />
      <div
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-label="Export Qoder PATs"
      >
        <div className="modal-head">
          <div>
            <h2>Export PAT</h2>
            <p className="muted" style={{ margin: 0 }}>
              {loading
                ? "Fetching…"
                : `${withPat.length} PAT${withPat.length === 1 ? "" : "s"} from ${accountIds.length} selected`}
            </p>
          </div>
          <button type="button" className="btn btn-ghost btn-sm" onClick={onClose}>
            Close
          </button>
        </div>

        {error && (
          <div className="alert alert-error" role="alert">
            {error}
          </div>
        )}

        {!loading && withPat.length > 0 && (
          <div className="btn-row" style={{ marginBottom: "var(--space-2)" }}>
            <button
              type="button"
              className="btn btn-sm btn-primary"
              onClick={() => void copy(allText, "__all__")}
            >
              {copied === "__all__" ? "Copied all!" : `Copy all (${withPat.length})`}
            </button>
          </div>
        )}

        <div
          className="mono"
          style={{
            maxHeight: "min(55vh, 460px)",
            overflow: "auto",
            background: "var(--void)",
            border: "1px solid var(--border)",
            borderRadius: "var(--radius)",
            padding: "var(--space-3)",
            fontSize: 12,
            lineHeight: 1.5,
          }}
        >
          {loading && <span className="muted">Loading PATs…</span>}
          {!loading && items.length === 0 && (
            <span className="muted">No accounts.</span>
          )}
          {items.map((it) => (
            <div
              key={it.id}
              style={{
                display: "flex",
                alignItems: "baseline",
                gap: 8,
                padding: "2px 0",
              }}
            >
              <span
                className="muted"
                style={{ flexShrink: 0, minWidth: 180 }}
                title={it.email ?? it.id}
              >
                {it.email || it.id.slice(0, 8)}
              </span>
              {it.pat ? (
                <>
                  <span
                    style={{
                      flex: 1,
                      wordBreak: "break-all",
                      color: "var(--parchment)",
                    }}
                  >
                    {it.pat}
                  </span>
                  <button
                    type="button"
                    className="btn btn-sm"
                    style={{ flexShrink: 0 }}
                    onClick={() => void copy(it.pat!, it.id)}
                  >
                    {copied === it.id ? "Copied" : "Copy"}
                  </button>
                </>
              ) : (
                <span style={{ flex: 1, color: "var(--blood)" }}>
                  {it.error || "no PAT"}
                </span>
              )}
            </div>
          ))}
        </div>
      </div>
    </>
  );
}
