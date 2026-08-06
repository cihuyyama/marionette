import { useEffect, useState } from "react";
import {
  ApiError,
  downloadJson,
  exportAccounts,
  type ExportAccountsResult,
} from "../lib/api";

type Props = {
  open: boolean;
  accountIds: string[];
  onClose: () => void;
};

export function ExportAccountsModal({ open, accountIds, onClose }: Props) {
  const [result, setResult] = useState<ExportAccountsResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (!open) return;
    setResult(null);
    setError(null);
    setCopied(false);
    setLoading(true);
    let cancelled = false;
    void (async () => {
      try {
        const res = await exportAccounts(accountIds);
        if (!cancelled) setResult(res);
      } catch (err) {
        if (!cancelled)
          setError(
            err instanceof ApiError
              ? err.message
              : err instanceof Error
                ? err.message
                : "Failed to export accounts",
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

  const jsonText = result
    ? JSON.stringify(result.backup, null, 2)
    : "";

  async function copyAll() {
    try {
      await navigator.clipboard.writeText(jsonText);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
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
        aria-label="Export accounts"
      >
        <div className="modal-head">
          <div>
            <h2>Export accounts</h2>
            <p className="muted" style={{ margin: 0 }}>
              {loading
                ? "Fetching…"
                : `${result?.count ?? 0} exported from ${accountIds.length} selected`}
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

        {!loading && result && result.errors.length > 0 && (
          <div className="alert alert-error" role="alert">
            {result.errors.length} account(s) skipped:{" "}
            {result.errors.map((e) => `${e.email || e.id}: ${e.error}`).join("; ")}
          </div>
        )}

        {!loading && result && result.count > 0 && (
          <div className="btn-row" style={{ marginBottom: "var(--space-2)" }}>
            <button
              type="button"
              className="btn btn-sm btn-primary"
              onClick={() =>
                downloadJson(
                  `marionette-export-${new Date().toISOString().slice(0, 19).replace(/[:T]/g, "-")}.json`,
                  jsonText,
                )
              }
            >
              Download JSON ({result.count})
            </button>
            <button
              type="button"
              className="btn btn-sm"
              onClick={() => void copyAll()}
            >
              {copied ? "Copied!" : "Copy JSON"}
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
            whiteSpace: "pre-wrap",
            wordBreak: "break-all",
          }}
        >
          {loading && <span className="muted">Loading accounts…</span>}
          {!loading && !result && <span className="muted">No data.</span>}
          {!loading && result && result.count === 0 && (
            <span className="muted">No exportable accounts.</span>
          )}
          {!loading && result && result.count > 0 && jsonText}
        </div>
      </div>
    </>
  );
}
