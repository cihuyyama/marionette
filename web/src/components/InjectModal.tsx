import { useEffect, useState, type FormEvent } from "react";
import { ApiError, startInjectJob } from "../lib/api";

type Props = {
  open: boolean;
  accountId: string;
  email: string | null;
  onClose: () => void;
  onStarted: (jobId: string) => void;
};

export function InjectModal({
  open,
  accountId,
  email,
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
  }, [open, accountId]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !loading) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose, loading]);

  if (!open) return null;

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    setError(null);
    setLoading(true);
    try {
      const res = await startInjectJob(accountId, { headless, refresh });
      onStarted(res.job.id);
      onClose();
    } catch (err) {
      setError(
        err instanceof ApiError
          ? err.message
          : err instanceof Error
            ? err.message
            : "Failed to start inject",
      );
    } finally {
      setLoading(false);
    }
  }

  const label = email?.trim() || accountId.slice(0, 8);

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
        aria-label="Dudul inject"
      >
        <div className="modal-head">
          <div>
            <h2>Dudul inject</h2>
            <p className="muted" style={{ margin: 0 }}>
              Activate Pro Trial for <span className="mono">{label}</span>
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
          <p className="muted" style={{ margin: 0, fontSize: 13 }}>
            Runs browser automation against dudul.dev using this account&apos;s
            PAT and the server access key. Live logs open on the next page.
          </p>

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
                  name="inject-mode"
                  checked={headless}
                  onChange={() => setHeadless(true)}
                  disabled={loading}
                />
                Headless — no window (default)
              </label>
              <label className="check">
                <input
                  type="radio"
                  name="inject-mode"
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
            <button type="submit" className="btn btn-primary" disabled={loading}>
              {loading ? "Starting…" : "Start inject"}
            </button>
          </div>
        </form>
      </div>
    </>
  );
}
