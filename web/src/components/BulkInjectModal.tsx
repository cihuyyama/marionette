import { useEffect, useState, type FormEvent } from "react";
import { ApiError, startBulkInjectJob } from "../lib/api";

type Props = {
  open: boolean;
  needCount: number;
  activeCount: number;
  onClose: () => void;
  onStarted: (jobId: string, summary: string) => void;
};

export function BulkInjectModal({
  open,
  needCount,
  activeCount,
  onClose,
  onStarted,
}: Props) {
  const [headless, setHeadless] = useState(true);
  const [refresh, setRefresh] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!open) return;
    setHeadless(true);
    setRefresh(true);
    setError(null);
    setLoading(false);
  }, [open, needCount]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !loading) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose, loading]);

  if (!open) return null;

  const skippedCredit = Math.max(0, activeCount - needCount);

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    if (needCount <= 0) {
      setError("No accounts need inject (all have credits or inactive)");
      return;
    }
    setError(null);
    setLoading(true);
    try {
      const res = await startBulkInjectJob({
        includeInactive: false,
        headless,
        refresh,
      });
      const skipped =
        (res.skipped_has_credit ?? 0) +
        (res.skipped_no_pat ?? 0) +
        (res.skipped_inactive ?? 0);
      const summary =
        `Bulk inject · ${res.total ?? res.job.bulk_total ?? needCount} accounts` +
        (skipped ? ` · skipped ${skipped}` : "");
      onStarted(res.job.id, summary);
      onClose();
    } catch (err) {
      setError(
        err instanceof ApiError
          ? err.message
          : err instanceof Error
            ? err.message
            : "Failed to start bulk inject",
      );
    } finally {
      setLoading(false);
    }
  }

  return (
    <>
      <div
        className="drawer-backdrop"
        onClick={() => {
          if (!loading) onClose();
        }}
      />
      <div
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-label="Bulk dudul inject"
      >
        <div className="modal-head">
          <div>
            <h2>Bulk dudul inject</h2>
            <p className="muted" style={{ margin: 0 }}>
              <span className="mono">{needCount}</span> account
              {needCount === 1 ? "" : "s"} without credit / not synced
            </p>
          </div>
          <button
            type="button"
            className="btn btn-ghost btn-sm"
            onClick={onClose}
            disabled={loading}
          >
            Close
          </button>
        </div>

        {error && (
          <div className="alert alert-error" role="alert">
            {error}
          </div>
        )}

        <form className="stack-gap" onSubmit={(e) => void onSubmit(e)}>
          <div
            className="panel"
            style={{
              margin: 0,
              padding: "var(--space-3)",
              background: "var(--ink)",
              border: "1px solid var(--border)",
            }}
          >
            <p className="muted" style={{ margin: 0, fontSize: 13 }}>
              One browser session for the whole job:
            </p>
            <ol
              className="muted"
              style={{
                margin: "var(--space-2) 0 0",
                paddingLeft: "1.25rem",
                fontSize: 13,
                lineHeight: 1.5,
              }}
            >
              <li>Open dudul.dev · pass Turnstile</li>
              <li>Inject each PAT in order (retry ×5)</li>
              <li>No Camoufox relaunch between accounts</li>
            </ol>
          </div>

          <ul
            className="muted"
            style={{
              margin: 0,
              paddingLeft: "1.1rem",
              fontSize: 13,
              lineHeight: 1.55,
            }}
          >
            <li>
              Queue: <strong style={{ color: "var(--parchment)" }}>{needCount}</strong>{" "}
              (not synced or 0 credit)
            </li>
            {skippedCredit > 0 && (
              <li>
                Skip: <span className="mono">{skippedCredit}</span> already have
                credits
              </li>
            )}
            <li>Inactive accounts are not included</li>
          </ul>

          <fieldset
            style={{
              border: "1px solid var(--border)",
              borderRadius: "var(--radius)",
              padding: "var(--space-3)",
              margin: 0,
            }}
          >
            <legend className="muted" style={{ fontSize: 12, padding: "0 6px" }}>
              Browser mode
            </legend>
            <div className="stack-gap" style={{ gap: "var(--space-2)" }}>
              <label className="check">
                <input
                  type="radio"
                  name="bulk-inject-mode"
                  checked={headless}
                  onChange={() => setHeadless(true)}
                  disabled={loading}
                />
                Headless — no window (default)
              </label>
              <label className="check">
                <input
                  type="radio"
                  name="bulk-inject-mode"
                  checked={!headless}
                  onChange={() => setHeadless(false)}
                  disabled={loading}
                />
                Headed — show browser (debug Turnstile / CF)
              </label>
            </div>
          </fieldset>

          <label className="check">
            <input
              type="checkbox"
              checked={refresh}
              onChange={(e) => setRefresh(e.target.checked)}
              disabled={loading}
            />
            Refresh auth + sync credits after inject succeeds
          </label>

          <div className="btn-row" style={{ marginTop: "var(--space-2)" }}>
            <button
              type="button"
              className="btn"
              onClick={onClose}
              disabled={loading}
            >
              Cancel
            </button>
            <button
              type="submit"
              className="btn btn-primary"
              disabled={loading || needCount <= 0}
            >
              {loading
                ? "Starting…"
                : `Start bulk inject (${needCount})`}
            </button>
          </div>
        </form>
      </div>
    </>
  );
}
