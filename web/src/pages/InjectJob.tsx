import { useCallback, useEffect, useRef, useState } from "react";
import { Link, Navigate, useParams } from "react-router-dom";
import {
  ApiError,
  cancelInjectJob,
  getInjectEvents,
  getInjectJob,
  refreshAfterInject,
  type InjectEvent,
  type InjectJob,
} from "../lib/api";

function statusTone(status: string): string {
  switch (status) {
    case "running":
    case "queued":
      return "thread";
    case "succeeded":
      return "fog";
    case "failed":
      return "blood";
    case "cancelled":
      return "seal";
    default:
      return "";
  }
}

function isTerminal(status: string): boolean {
  return ["succeeded", "failed", "cancelled"].includes(status);
}

export function InjectJobPage() {
  const { jobId } = useParams<{ jobId: string }>();
  const [job, setJob] = useState<InjectJob | null>(null);
  const [events, setEvents] = useState<InjectEvent[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [cancelling, setCancelling] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const afterRef = useRef(0);
  const logEndRef = useRef<HTMLDivElement | null>(null);
  const didRefreshRef = useRef(false);

  const loadInitial = useCallback(async () => {
    if (!jobId) return;
    try {
      const res = await getInjectJob(jobId);
      setJob(res.job);
      if (res.events?.length) {
        setEvents(res.events);
        afterRef.current = res.events[res.events.length - 1].seq;
      }
      setError(null);
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e.message
          : e instanceof Error
            ? e.message
            : "Failed to load inject job",
      );
    }
  }, [jobId]);

  useEffect(() => {
    void loadInitial();
  }, [loadInitial]);

  useEffect(() => {
    if (!jobId) return;
    let cancelled = false;
    let id = 0;
    const isTerminal = (s?: string) =>
      s === "succeeded" || s === "failed" || s === "cancelled";
    const tick = async () => {
      try {
        const res = await getInjectEvents(jobId, afterRef.current);
        if (cancelled) return;
        setJob(res.job);
        if (res.events.length) {
          setEvents((prev) => {
            const next = [...prev, ...res.events];
            return next.length > 1500 ? next.slice(-1500) : next;
          });
          afterRef.current = res.events[res.events.length - 1].seq;
        }
        if (isTerminal(res.job?.status) && id) {
          window.clearInterval(id);
          id = 0;
        }
      } catch {
        /* ignore poll errors while job ends */
      }
    };
    void tick();
    id = window.setInterval(() => void tick(), 1500);
    return () => {
      cancelled = true;
      if (id) window.clearInterval(id);
    };
  }, [jobId]);

  useEffect(() => {
    logEndRef.current?.scrollIntoView({ behavior: "smooth", block: "end" });
  }, [events.length]);

  useEffect(() => {
    if (!job || !jobId) return;
    if (job.status !== "succeeded" || !job.refresh) return;
    if (didRefreshRef.current) return;
    didRefreshRef.current = true;
    setRefreshing(true);
    void (async () => {
      try {
        const res = await refreshAfterInject(jobId);
        if (res.skipped) {
          setNotice("Inject succeeded (auth refresh skipped)");
        } else if (
          typeof res.accounts_refreshed === "number" &&
          (res.accounts_targeted ?? 0) > 1
        ) {
          setNotice(
            `Inject done · refreshed ${res.accounts_refreshed}/${res.accounts_targeted}` +
              (res.accounts_failed
                ? ` · ${res.accounts_failed} refresh fail`
                : ""),
          );
        } else if (res.refreshed) {
          const rem = res.account.quota_remaining;
          const lim = res.account.quota_limit;
          setNotice(
            lim
              ? `Inject succeeded · credits ${rem}/${lim}`
              : "Inject succeeded · auth refreshed",
          );
        } else {
          setNotice(
            res.refresh_error
              ? `Inject succeeded · refresh: ${res.refresh_error}`
              : "Inject succeeded",
          );
        }
      } catch (e) {
        setNotice(
          e instanceof ApiError
            ? `Inject ok · refresh failed: ${e.message}`
            : "Inject ok · refresh failed",
        );
      } finally {
        setRefreshing(false);
      }
    })();
  }, [job, jobId]);

  if (!jobId) {
    return <Navigate to="/accounts/qoder" replace />;
  }

  async function onCancel() {
    if (!jobId) return;
    setCancelling(true);
    setError(null);
    try {
      const res = await cancelInjectJob(jobId);
      setJob(res.job);
      setNotice("Cancel requested");
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e.message
          : e instanceof Error
            ? e.message
            : "Cancel failed",
      );
    } finally {
      setCancelling(false);
    }
  }

  const isBulk = Boolean(job?.bulk) || (job?.bulk_total ?? 0) > 1;
  const label = isBulk
    ? `${job?.bulk_ok ?? 0}/${job?.bulk_total ?? "?"} ok · ${job?.bulk_fail ?? 0} fail`
    : job?.email?.trim() || job?.account_id?.slice(0, 8) || "…";
  const running = job ? !isTerminal(job.status) : true;

  return (
    <div>
      <header className="page-header">
        <p className="breadcrumb muted">
          <Link to="/accounts">Accounts</Link>
          <span aria-hidden> / </span>
          <Link to="/accounts/qoder">Qoder</Link>
          <span aria-hidden> / </span>
          <span>Inject</span>
        </p>
        <h1>{isBulk ? "Dudul inject (bulk)" : "Dudul inject"}</h1>
        <p className="subtitle">
          Live log for <span className="mono">{label}</span>
          {job?.headless === false ? " · headed" : " · headless"}
          {isBulk ? " · one browser, sequential PATs" : ""}
        </p>
      </header>

      {error && (
        <div className="alert alert-error" role="alert">
          {error}
        </div>
      )}
      {notice && (
        <div className="alert alert-ok" role="status">
          {notice}
        </div>
      )}

      <div className="panel" style={{ marginBottom: "var(--space-4)" }}>
        <div
          style={{
            display: "flex",
            flexWrap: "wrap",
            gap: "var(--space-3)",
            alignItems: "center",
            justifyContent: "space-between",
          }}
        >
          <div>
            {job ? (
              <>
                <span
                  className={`chip chip-${statusTone(job.status) || "muted"}`}
                >
                  {job.status}
                </span>{" "}
                <span className="mono muted" style={{ fontSize: 12 }}>
                  {job.id}
                </span>
              </>
            ) : (
              <span className="muted">Loading job…</span>
            )}
          </div>
          <div className="muted" style={{ fontSize: 12 }}>
            {job?.current_step ? `step ${job.current_step}` : "—"}
            {refreshing ? " · refreshing credits…" : ""}
          </div>
        </div>
        {job?.error && (
          <p
            className="muted"
            style={{ marginTop: "var(--space-2)", color: "var(--blood)" }}
          >
            {job.error}
          </p>
        )}
        {job?.inject_result && (
          <p
            className="muted mono"
            style={{ marginTop: "var(--space-2)", fontSize: 12 }}
          >
            result {JSON.stringify(job.inject_result)}
          </p>
        )}
        <div className="btn-row" style={{ marginTop: "var(--space-3)" }}>
          {running && (
            <button
              type="button"
              className="btn"
              disabled={cancelling}
              onClick={() => void onCancel()}
            >
              {cancelling ? "Cancelling…" : "Cancel"}
            </button>
          )}
          <Link to="/accounts/qoder" className="btn">
            ← Qoder accounts
          </Link>
        </div>
      </div>

      <div className="panel">
        <h2 style={{ margin: "0 0 var(--space-3)", fontSize: 15 }}>
          Process log
        </h2>
        <div
          className="log-box mono"
          style={{
            maxHeight: "min(60vh, 520px)",
            overflow: "auto",
            background: "var(--void)",
            border: "1px solid var(--border)",
            borderRadius: "var(--radius)",
            padding: "var(--space-3)",
            fontSize: 12,
            lineHeight: 1.4,
            whiteSpace: "pre-wrap",
            wordBreak: "break-word",
          }}
        >
          {events.length === 0 && (
            <span className="muted">Waiting for progress…</span>
          )}
          {events.map((ev) => {
            const p = ev.parsed;
            const level = p && typeof p.level === "string" ? p.level : "…";
            const email = p && typeof p.email === "string" ? p.email : "";
            const step = p && typeof p.step === "string" ? p.step : "";
            const msg =
              p && typeof p.msg === "string"
                ? p.msg
                : p && typeof p.reason === "string"
                  ? p.reason
                  : ev.line;
            return (
              <div key={ev.seq}>
                {p ? (
                  <span>
                    <span className="muted">[{level}]</span> {email} {step}{" "}
                    {msg}
                  </span>
                ) : (
                  ev.line
                )}
              </div>
            );
          })}
          <div ref={logEndRef} />
        </div>
      </div>
    </div>
  );
}
