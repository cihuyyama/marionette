import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import { Link, Navigate, useParams } from "react-router-dom";
import {
  ApiError,
  cancelFarmJob,
  getFarmEvents,
  getFarmStatus,
  importFarmJob,
  listAccounts,
  retryFailedFarmJob,
  startFarmJob,
  type Account,
  type FarmEvent,
  type FarmJob,
  type FarmStatus,
} from "../lib/api";
import {
  getAutomationMethod,
  isReadyFarm,
  methodLabel,
} from "../lib/automation";

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

export function FarmPage() {
  const { provider, method } = useParams<{
    provider: string;
    method: string;
  }>();
  const route = getAutomationMethod(provider, method);

  if (!route) {
    return <Navigate to="/automation" replace />;
  }

  if (!isReadyFarm(provider, method)) {
    return (
      <ComingSoonFarm
        providerLabel={route.provider.label}
        methodLabel={route.method.label}
        description={route.method.description}
      />
    );
  }

  if (provider === "grok-cli") {
    return <GrokReloginFarm />;
  }

  return <QoderGoogleSsoFarm />;
}

function ComingSoonFarm({
  providerLabel,
  methodLabel: methodName,
  description,
}: {
  providerLabel: string;
  methodLabel: string;
  description: string;
}) {
  return (
    <div>
      <header className="page-header">
        <p className="breadcrumb muted">
          <Link to="/automation">Automation</Link>
          <span aria-hidden> / </span>
          <span>{providerLabel}</span>
          <span aria-hidden> / </span>
          <span>{methodName}</span>
        </p>
        <h1>
          {providerLabel} · {methodName}
        </h1>
        <p className="subtitle">{description}</p>
      </header>
      <div className="panel empty-state">
        <p className="muted" style={{ margin: 0 }}>
          This path is not wired in the control room yet. Use the catalog to
          pick a Ready method, or run the CLI package when available.
        </p>
        <Link to="/automation" className="btn btn-sm" style={{ marginTop: 12 }}>
          ← Back to Automation
        </Link>
      </div>
    </div>
  );
}

function QoderGoogleSsoFarm() {
  const [status, setStatus] = useState<FarmStatus | null>(null);
  const [accounts, setAccounts] = useState("");
  const [inject, setInject] = useState(true);
  const [headless, setHeadless] = useState(false);
  const [deviceAuth, setDeviceAuth] = useState(false);
  const [autoImport, setAutoImport] = useState(true);
  const [skipExisting, setSkipExisting] = useState(false);
  const [settle, setSettle] = useState(5);
  const [concurrency, setConcurrency] = useState(1);
  const [accountDelay, setAccountDelay] = useState(0);
  const [accountRetries, setAccountRetries] = useState(2);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [events, setEvents] = useState<FarmEvent[]>([]);
  const [job, setJob] = useState<FarmJob | null>(null);
  const afterRef = useRef(0);
  const logEndRef = useRef<HTMLDivElement | null>(null);
  const jobIdRef = useRef<string | null>(null);

  const refreshStatus = useCallback(async () => {
    try {
      const s = await getFarmStatus();
      setStatus(s);
      if (s.current && (s.current.provider ?? "qoder") === "qoder") {
        setJob(s.current);
        jobIdRef.current = s.current.id;
      }
      setError(null);
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e.message
          : e instanceof Error
            ? e.message
            : "Failed to load farm status",
      );
    }
  }, []);

  useEffect(() => {
    void refreshStatus();
    const id = window.setInterval(() => void refreshStatus(), 4000);
    return () => window.clearInterval(id);
  }, [refreshStatus]);

  useEffect(() => {
    const jid = jobIdRef.current ?? job?.id;
    if (!jid) return;
    let cancelled = false;
    const tick = async () => {
      try {
        const res = await getFarmEvents(jid, afterRef.current);
        if (cancelled) return;
        setJob(res.job);
        if (res.events.length) {
          setEvents((prev) => {
            const next = [...prev, ...res.events];
            return next.length > 1500 ? next.slice(-1500) : next;
          });
          afterRef.current = res.events[res.events.length - 1].seq;
        }
      } catch {
        void 0;
      }
    };
    void tick();
    const id = window.setInterval(() => void tick(), 1500);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [job?.id, status?.busy]);

  useEffect(() => {
    logEndRef.current?.scrollIntoView({ behavior: "smooth", block: "end" });
  }, [events.length]);

  async function onStart(e: FormEvent) {
    e.preventDefault();
    setError(null);
    setNotice(null);
    setStarting(true);
    try {
      const workers = Math.max(1, Math.floor(concurrency) || 1);
      const res = await startFarmJob({
        provider: "qoder",
        accounts,
        inject,
        headless,
        device_auth: deviceAuth,
        auto_import: autoImport,
        settle_secs: settle,
        concurrency: workers,
        account_retries: Math.max(1, Math.floor(accountRetries) || 1),
        skip_existing: skipExisting,
        account_delay: Math.max(0, accountDelay || 0),
      });
      setJob(res.job);
      jobIdRef.current = res.job.id;
      afterRef.current = 0;
      setEvents([]);
      setNotice(
        `Job ${res.job.id.slice(0, 8)}… started · ${res.job.concurrency ?? workers} worker(s)`,
      );
      await refreshStatus();
    } catch (err) {
      setError(
        err instanceof ApiError
          ? err.message
          : err instanceof Error
            ? err.message
            : "Start failed",
      );
    } finally {
      setStarting(false);
    }
  }

  async function onCancel() {
    if (!job) return;
    setError(null);
    try {
      const res = await cancelFarmJob(job.id);
      setJob(res.job);
      setNotice("Cancel requested");
      await refreshStatus();
    } catch (err) {
      setError(
        err instanceof ApiError
          ? err.message
          : err instanceof Error
            ? err.message
            : "Cancel failed",
      );
    }
  }

  async function onImport() {
    if (!job) return;
    setError(null);
    try {
      const res = await importFarmJob(job.id);
      setNotice(
        `Import: inserted ${res.inserted ?? 0}, skipped ${res.skipped ?? 0}`,
      );
      await refreshStatus();
    } catch (err) {
      setError(
        err instanceof ApiError
          ? err.message
          : err instanceof Error
            ? err.message
            : "Import failed",
      );
    }
  }

  async function onRetryFailedFor(jobId: string) {
    setError(null);
    setNotice(null);
    setStarting(true);
    try {
      const workers = Math.max(1, Math.floor(concurrency) || 1);
      const res = await retryFailedFarmJob(jobId, {
        provider: "qoder",
        inject,
        headless,
        device_auth: deviceAuth,
        auto_import: autoImport,
        settle_secs: settle,
        concurrency: workers,
        account_retries: Math.max(1, Math.floor(accountRetries) || 1),
        skip_existing: false,
        account_delay: Math.max(0, accountDelay || 0),
      });
      setJob(res.job);
      jobIdRef.current = res.job.id;
      afterRef.current = 0;
      setEvents([]);
      setNotice(
        `Retry job ${res.job.id.slice(0, 8)}… · ${res.failed_count ?? "?"} failed from ${jobId.slice(0, 8)}…`,
      );
      await refreshStatus();
    } catch (err) {
      setError(
        err instanceof ApiError
          ? err.message
          : err instanceof Error
            ? err.message
            : "Retry failed",
      );
    } finally {
      setStarting(false);
    }
  }

  async function onRetryFailed() {
    if (!job) return;
    await onRetryFailedFor(job.id);
  }

  const busy =
    (status?.busy ?? false) &&
    (status?.current?.provider ?? "qoder") === "qoder";
  const packageOk =
    status?.info.packages?.qoder?.package_present ??
    status?.info.package_present ??
    false;
  const maxWorkers = Math.max(1, status?.info.max_concurrency ?? 8);
  const progressPct =
    job && job.total > 0
      ? Math.min(100, Math.round(((job.ok + job.fail) / job.total) * 100))
      : 0;
  const qoderHistory = (status?.history ?? []).filter(
    (h) => (h.provider ?? "qoder") === "qoder",
  );

  return (
    <div>
      <header className="page-header">
        <p className="breadcrumb muted">
          <Link to="/automation">Automation</Link>
          <span aria-hidden> / </span>
          <span>Qoder</span>
          <span aria-hidden> / </span>
          <span>{methodLabel("google-sso")}</span>
        </p>
        <h1>Qoder · Google SSO</h1>
        <p className="subtitle">
          GSuite → PAT → inject (Python under{" "}
          <code className="mono">scripts/automation/qoder_farm</code>)
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
        <div className="row-fields" style={{ gap: "var(--space-4)" }}>
          <div>
            <div className="muted" style={{ fontSize: 11 }}>
              Package
            </div>
            <div className="mono" style={{ fontSize: 12 }}>
              {packageOk ? "present" : "missing"} ·{" "}
              {status?.info.package_dir ?? "…"}
            </div>
          </div>
          <div>
            <div className="muted" style={{ fontSize: 11 }}>
              Python
            </div>
            <div className="mono" style={{ fontSize: 12 }}>
              {status?.info.python ?? "python"}
            </div>
          </div>
          <div>
            <div className="muted" style={{ fontSize: 11 }}>
              Data dir
            </div>
            <div className="mono" style={{ fontSize: 12 }}>
              {status?.info.data_dir ?? "…"}
            </div>
          </div>
        </div>
        {!packageOk && (
          <p className="muted" style={{ marginTop: "var(--space-3)", marginBottom: 0 }}>
            Install deps:{" "}
            <code className="mono">
              cd scripts/automation/qoder_farm && pip install -r requirements.txt
              && python -m camoufox fetch
            </code>
          </p>
        )}
      </div>

      <form className="panel farm-form" onSubmit={(e) => void onStart(e)}>
        <div className="field" style={{ marginBottom: 0 }}>
          <label htmlFor="farm-accounts">Accounts (email|password per line)</label>
          <textarea
            id="farm-accounts"
            className="input mono"
            rows={8}
            value={accounts}
            onChange={(e) => setAccounts(e.target.value)}
            placeholder={"user@gsuite.com|password\n# comments ok"}
            disabled={busy || starting}
            required
            style={{ width: "100%", resize: "vertical" }}
          />
        </div>

        <div className="farm-controls">
          <section className="farm-control-group" aria-labelledby="farm-opts-label">
            <h2 id="farm-opts-label" className="farm-control-title">
              Options
            </h2>
            <div className="farm-check-grid">
              <label className="check">
                <input
                  type="checkbox"
                  checked={inject}
                  onChange={(e) => setInject(e.target.checked)}
                  disabled={busy || starting}
                />
                <span className="check-body">
                  <span className="check-label">dudul inject</span>
                  <span className="check-hint">Write tokens into browser session after PAT</span>
                </span>
              </label>
              <label className="check">
                <input
                  type="checkbox"
                  checked={headless}
                  onChange={(e) => setHeadless(e.target.checked)}
                  disabled={busy || starting}
                />
                <span className="check-body">
                  <span className="check-label">headless</span>
                  <span className="check-hint">Hidden browser; SSO often flakier</span>
                </span>
              </label>
              <label className="check">
                <input
                  type="checkbox"
                  checked={deviceAuth}
                  onChange={(e) => setDeviceAuth(e.target.checked)}
                  disabled={busy || starting}
                />
                <span className="check-body">
                  <span className="check-label">device auth</span>
                  <span className="check-hint">Optional device exchange after PAT</span>
                </span>
              </label>
              <label className="check">
                <input
                  type="checkbox"
                  checked={autoImport}
                  onChange={(e) => setAutoImport(e.target.checked)}
                  disabled={busy || starting}
                />
                <span className="check-body">
                  <span className="check-label">auto-import</span>
                  <span className="check-hint">Each success + final upsert into pool</span>
                </span>
              </label>
              <label className="check">
                <input
                  type="checkbox"
                  checked={skipExisting}
                  onChange={(e) => setSkipExisting(e.target.checked)}
                  disabled={busy || starting}
                />
                <span className="check-body">
                  <span className="check-label">skip already farmed</span>
                  <span className="check-hint">Skip emails already in Qoder pool / output</span>
                </span>
              </label>
            </div>
          </section>

          <section className="farm-control-group" aria-labelledby="farm-timing-label">
            <h2 id="farm-timing-label" className="farm-control-title">
              Timing & workers
            </h2>
            <div className="farm-num-grid">
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="farm-settle">Settle (s)</label>
                <input
                  id="farm-settle"
                  className="input"
                  type="number"
                  min={0}
                  value={settle}
                  onChange={(e) => setSettle(Number(e.target.value) || 0)}
                  disabled={busy || starting}
                />
                <span className="hint">Pause after browser ready</span>
              </div>
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="farm-workers">Workers</label>
                <input
                  id="farm-workers"
                  className="input"
                  type="number"
                  min={1}
                  max={maxWorkers}
                  step={1}
                  value={concurrency}
                  onChange={(e) => {
                    const n = Number(e.target.value);
                    if (!Number.isFinite(n)) {
                      setConcurrency(1);
                      return;
                    }
                    setConcurrency(
                      Math.min(maxWorkers, Math.max(1, Math.floor(n))),
                    );
                  }}
                  disabled={busy || starting}
                  title={`Parallel accounts in this job (1–${maxWorkers}). Default 1.`}
                />
                <span className="hint">1–{maxWorkers} parallel accounts</span>
              </div>
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="farm-retries">Retries</label>
                <input
                  id="farm-retries"
                  className="input"
                  type="number"
                  min={1}
                  max={5}
                  step={1}
                  value={accountRetries}
                  onChange={(e) => {
                    const n = Number(e.target.value);
                    if (!Number.isFinite(n)) {
                      setAccountRetries(2);
                      return;
                    }
                    setAccountRetries(Math.min(5, Math.max(1, Math.floor(n))));
                  }}
                  disabled={busy || starting}
                  title="Attempts per account (SSO→PAT pipeline)"
                />
                <span className="hint">Attempts per account (1–5)</span>
              </div>
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="farm-delay">Delay (s)</label>
                <input
                  id="farm-delay"
                  className="input"
                  type="number"
                  min={0}
                  step={0.5}
                  value={accountDelay}
                  onChange={(e) => {
                    const n = Number(e.target.value);
                    setAccountDelay(!Number.isFinite(n) || n < 0 ? 0 : n);
                  }}
                  disabled={busy || starting}
                  title="Seconds between accounts (serial) or worker stagger"
                />
                <span className="hint">Between accounts / worker stagger</span>
              </div>
            </div>
            {concurrency > 1 && inject && !headless && (
              <p className="farm-warn muted">
                Workers &gt; 1 opens multiple headed browsers. Inject may flake —
                try headless or lower workers if inject fails.
              </p>
            )}
          </section>
        </div>

        <div className="btn-row">
          <button
            type="submit"
            className="btn btn-primary"
            disabled={busy || starting || !accounts.trim() || !packageOk}
          >
            {starting ? "Starting…" : busy ? "Job running…" : "Start farm"}
          </button>
          {job && !["succeeded", "failed", "cancelled"].includes(job.status) && (
            <button
              type="button"
              className="btn"
              onClick={() => void onCancel()}
            >
              Cancel
            </button>
          )}
          {job && (
            <button
              type="button"
              className="btn"
              onClick={() => void onImport()}
            >
              Import results
            </button>
          )}
          {job &&
            job.fail > 0 &&
            ["succeeded", "failed", "cancelled"].includes(job.status) && (
              <button
                type="button"
                className="btn btn-primary"
                disabled={busy || starting}
                onClick={() => void onRetryFailed()}
                title="Re-run only accounts that failed in this job (passwords from job accounts.txt)"
              >
                {starting ? "Starting…" : `Retry failed (${job.fail})`}
              </button>
            )}
          <Link to="/accounts/qoder" className="btn">
            Qoder accounts
          </Link>
        </div>
      </form>

      {job && (
        <div className="panel" style={{ marginTop: "var(--space-4)" }}>
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
              <span className={`chip chip-${statusTone(job.status) || "muted"}`}>
                {job.status}
              </span>{" "}
              <span className="mono muted" style={{ fontSize: 12 }}>
                {job.id}
              </span>
            </div>
            <div className="muted" style={{ fontSize: 12 }}>
              ok {job.ok} · fail {job.fail} · total {job.total}
              {job.current_step ? ` · step ${job.current_step}` : ""}
              {job.current_email ? ` · ${job.current_email}` : ""}
            </div>
          </div>
          <div
            className="farm-progress"
            style={{
              marginTop: "var(--space-3)",
              height: 6,
              background: "var(--surface-hover)",
              borderRadius: 999,
              overflow: "hidden",
            }}
          >
            <div
              style={{
                width: `${progressPct}%`,
                height: "100%",
                background: "var(--thread)",
                transition: "width 0.3s ease",
              }}
            />
          </div>
          {job.error && (
            <p className="muted" style={{ marginTop: "var(--space-2)", color: "var(--blood)" }}>
              {job.error}
            </p>
          )}
          {job.import_result && (
            <p className="muted mono" style={{ marginTop: "var(--space-2)", fontSize: 12 }}>
              import {JSON.stringify(job.import_result)}
            </p>
          )}
          <div
            className="farm-log mono"
            style={{
              marginTop: "var(--space-3)",
              maxHeight: 360,
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
      )}

      {qoderHistory.length > 0 && (
        <div className="panel" style={{ marginTop: "var(--space-4)" }}>
          <h2 style={{ margin: "0 0 var(--space-3)", fontSize: 15 }}>Recent jobs</h2>
          <div className="table-wrap">
            <table className="table">
              <thead>
                <tr>
                  <th>ID</th>
                  <th>Status</th>
                  <th>Accounts</th>
                  <th>Ok/Fail</th>
                  <th>Finished</th>
                  <th></th>
                </tr>
              </thead>
              <tbody>
                {qoderHistory.map((h) => (
                  <tr key={h.id}>
                    <td className="mono">{h.id.slice(0, 8)}</td>
                    <td>{h.status}</td>
                    <td>{h.accounts_count}</td>
                    <td>
                      {h.ok}/{h.fail}
                    </td>
                    <td className="muted">{h.finished_at ?? "—"}</td>
                    <td>
                      {h.fail > 0 &&
                        ["succeeded", "failed", "cancelled"].includes(
                          h.status,
                        ) && (
                          <button
                            type="button"
                            className="btn btn-sm"
                            disabled={busy || starting}
                            onClick={() => {
                              setJob(h);
                              jobIdRef.current = h.id;
                              void onRetryFailedFor(h.id);
                            }}
                          >
                            Retry failed
                          </button>
                        )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </div>
  );
}

function needsGrokRelogin(acc: Account): boolean {
  if (!acc.email?.trim()) return false;
  const status = (acc.status || "").toLowerCase();
  if (status === "cut") return true;
  const err = (acc.last_error || "").toLowerCase();
  if (!err) return false;
  return (
    err.includes("invalid_grant") ||
    err.includes("authinvalid") ||
    err.includes("auth_invalid") ||
    err.includes("auth expired") ||
    err.includes("authexpired") ||
    err.includes("access denied") ||
    err.includes("accessdenied") ||
    err.includes("401") ||
    err.includes("403")
  );
}

function GrokReloginFarm() {
  const [status, setStatus] = useState<FarmStatus | null>(null);
  const [accounts, setAccounts] = useState("");
  const [sharedPassword, setSharedPassword] = useState("");
  const [headless, setHeadless] = useState(false);
  const [autoImport, setAutoImport] = useState(true);
  const [skipExisting, setSkipExisting] = useState(false);
  const [concurrency, setConcurrency] = useState(1);
  const [accountDelay, setAccountDelay] = useState(0);
  const [accountRetries, setAccountRetries] = useState(2);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [loadingPool, setLoadingPool] = useState(false);
  const [events, setEvents] = useState<FarmEvent[]>([]);
  const [job, setJob] = useState<FarmJob | null>(null);
  const afterRef = useRef(0);
  const logEndRef = useRef<HTMLDivElement | null>(null);
  const jobIdRef = useRef<string | null>(null);

  const packageOk =
    status?.info.packages?.["grok-cli"]?.package_present ??
    false;
  const packageDir =
    status?.info.packages?.["grok-cli"]?.package_dir ??
    "scripts/automation/grok_farm";
  const maxWorkers = Math.max(1, status?.info.max_concurrency ?? 8);

  const refreshStatus = useCallback(async () => {
    try {
      const s = await getFarmStatus();
      setStatus(s);
      if (s.current && (s.current.provider ?? "qoder") === "grok-cli") {
        setJob(s.current);
        jobIdRef.current = s.current.id;
      }
      setError(null);
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e.message
          : e instanceof Error
            ? e.message
            : "Failed to load farm status",
      );
    }
  }, []);

  useEffect(() => {
    void refreshStatus();
    const id = window.setInterval(() => void refreshStatus(), 4000);
    return () => window.clearInterval(id);
  }, [refreshStatus]);

  useEffect(() => {
    const jid = jobIdRef.current ?? job?.id;
    if (!jid) return;
    let cancelled = false;
    const tick = async () => {
      try {
        const res = await getFarmEvents(jid, afterRef.current);
        if (cancelled) return;
        setJob(res.job);
        if (res.events.length) {
          setEvents((prev) => {
            const next = [...prev, ...res.events];
            return next.length > 1500 ? next.slice(-1500) : next;
          });
          afterRef.current = res.events[res.events.length - 1].seq;
        }
      } catch {
        void 0;
      }
    };
    void tick();
    const id = window.setInterval(() => void tick(), 1500);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [job?.id, status?.busy]);

  useEffect(() => {
    logEndRef.current?.scrollIntoView({ behavior: "smooth", block: "end" });
  }, [events.length]);

  async function onLoadFromPool() {
    setError(null);
    setNotice(null);
    setLoadingPool(true);
    try {
      const res = await listAccounts({ provider: "grok-cli" });
      const emails = res.accounts
        .filter(needsGrokRelogin)
        .map((a) => a.email!.trim())
        .filter(Boolean);
      const seen = new Set<string>();
      const unique: string[] = [];
      for (const e of emails) {
        const key = e.toLowerCase();
        if (seen.has(key)) continue;
        seen.add(key);
        unique.push(e);
      }
      if (unique.length === 0) {
        setNotice("No cut / invalid_grant / auth-dead grok-cli accounts with email");
        return;
      }
      setAccounts(unique.join("\n"));
      setNotice(
        `Loaded ${unique.length} email(s) from pool (cut + auth errors). Use shared password if lines are email-only.`,
      );
    } catch (err) {
      setError(
        err instanceof ApiError
          ? err.message
          : err instanceof Error
            ? err.message
            : "Failed to load pool accounts",
      );
    } finally {
      setLoadingPool(false);
    }
  }

  async function onStart(e: FormEvent) {
    e.preventDefault();
    setError(null);
    setNotice(null);
    setStarting(true);
    try {
      const workers = Math.max(1, Math.floor(concurrency) || 1);
      const defaultPw = sharedPassword.trim();
      const res = await startFarmJob({
        provider: "grok-cli",
        accounts,
        default_password: defaultPw || null,
        inject: false,
        headless,
        device_auth: false,
        auto_import: autoImport,
        concurrency: workers,
        account_retries: Math.max(1, Math.floor(accountRetries) || 1),
        skip_existing: skipExisting,
        account_delay: Math.max(0, accountDelay || 0),
      });
      setJob(res.job);
      jobIdRef.current = res.job.id;
      afterRef.current = 0;
      setEvents([]);
      setNotice(
        `Job ${res.job.id.slice(0, 8)}… started · ${res.job.concurrency ?? workers} worker(s)`,
      );
      await refreshStatus();
    } catch (err) {
      setError(
        err instanceof ApiError
          ? err.message
          : err instanceof Error
            ? err.message
            : "Start failed",
      );
    } finally {
      setStarting(false);
    }
  }

  async function onCancel() {
    if (!job) return;
    setError(null);
    try {
      const res = await cancelFarmJob(job.id);
      setJob(res.job);
      setNotice("Cancel requested");
      await refreshStatus();
    } catch (err) {
      setError(
        err instanceof ApiError
          ? err.message
          : err instanceof Error
            ? err.message
            : "Cancel failed",
      );
    }
  }

  async function onImport() {
    if (!job) return;
    setError(null);
    try {
      const res = await importFarmJob(job.id);
      setNotice(
        `Import: inserted ${res.inserted ?? 0}, skipped ${res.skipped ?? 0}`,
      );
      await refreshStatus();
    } catch (err) {
      setError(
        err instanceof ApiError
          ? err.message
          : err instanceof Error
            ? err.message
            : "Import failed",
      );
    }
  }

  async function onRetryFailedFor(jobId: string) {
    setError(null);
    setNotice(null);
    setStarting(true);
    try {
      const workers = Math.max(1, Math.floor(concurrency) || 1);
      const res = await retryFailedFarmJob(jobId, {
        provider: "grok-cli",
        inject: false,
        headless,
        device_auth: false,
        auto_import: autoImport,
        concurrency: workers,
        account_retries: Math.max(1, Math.floor(accountRetries) || 1),
        skip_existing: false,
        account_delay: Math.max(0, accountDelay || 0),
      });
      setJob(res.job);
      jobIdRef.current = res.job.id;
      afterRef.current = 0;
      setEvents([]);
      setNotice(
        `Retry job ${res.job.id.slice(0, 8)}… · ${res.failed_count ?? "?"} failed from ${jobId.slice(0, 8)}…`,
      );
      await refreshStatus();
    } catch (err) {
      setError(
        err instanceof ApiError
          ? err.message
          : err instanceof Error
            ? err.message
            : "Retry failed",
      );
    } finally {
      setStarting(false);
    }
  }

  async function onRetryFailed() {
    if (!job) return;
    await onRetryFailedFor(job.id);
  }

  const busy =
    (status?.busy ?? false) &&
    (status?.current?.provider ?? "qoder") === "grok-cli";
  const progressPct =
    job && job.total > 0
      ? Math.min(100, Math.round(((job.ok + job.fail) / job.total) * 100))
      : 0;
  const grokHistory = (status?.history ?? []).filter(
    (h) => (h.provider ?? "") === "grok-cli",
  );

  return (
    <div>
      <header className="page-header">
        <p className="breadcrumb muted">
          <Link to="/automation">Automation</Link>
          <span aria-hidden> / </span>
          <span>Grok CLI</span>
          <span aria-hidden> / </span>
          <span>{methodLabel("relogin")}</span>
        </p>
        <h1>Grok CLI · Relogin</h1>
        <p className="subtitle">
          Email+password → OAuth PKCE → verify chat (Python under{" "}
          <code className="mono">scripts/automation/grok_farm</code>)
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
        <div className="row-fields" style={{ gap: "var(--space-4)" }}>
          <div>
            <div className="muted" style={{ fontSize: 11 }}>
              Package
            </div>
            <div className="mono" style={{ fontSize: 12 }}>
              {packageOk ? "present" : "missing"} · {packageDir}
            </div>
          </div>
          <div>
            <div className="muted" style={{ fontSize: 11 }}>
              Python
            </div>
            <div className="mono" style={{ fontSize: 12 }}>
              {status?.info.python ?? "python"}
            </div>
          </div>
          <div>
            <div className="muted" style={{ fontSize: 11 }}>
              Data dir
            </div>
            <div className="mono" style={{ fontSize: 12 }}>
              {status?.info.data_dir ?? "…"}
            </div>
          </div>
        </div>
        {!packageOk && (
          <p
            className="muted"
            style={{ marginTop: "var(--space-3)", marginBottom: 0 }}
          >
            Install deps:{" "}
            <code className="mono">
              cd scripts/automation/grok_farm && pip install -r requirements.txt
              && python -m camoufox fetch
            </code>
          </p>
        )}
      </div>

      <form className="panel farm-form" onSubmit={(e) => void onStart(e)}>
        <div className="field" style={{ marginBottom: "var(--space-3)" }}>
          <div
            className="row-fields"
            style={{
              justifyContent: "space-between",
              alignItems: "center",
              gap: "var(--space-3)",
              marginBottom: "var(--space-2)",
            }}
          >
            <label htmlFor="grok-farm-accounts" style={{ marginBottom: 0 }}>
              Accounts (email|password, or bare email + shared password)
            </label>
            <button
              type="button"
              className="btn"
              onClick={() => void onLoadFromPool()}
              disabled={busy || starting || loadingPool}
              title="Fill emails from grok-cli pool: cut + invalid_grant / auth death"
            >
              {loadingPool ? "Loading…" : "Load from pool"}
            </button>
          </div>
          <textarea
            id="grok-farm-accounts"
            className="input mono"
            rows={8}
            value={accounts}
            onChange={(e) => setAccounts(e.target.value)}
            placeholder={
              "user@example.com|password\nuser2@example.com\n# bare email uses shared password"
            }
            disabled={busy || starting}
            required
            style={{ width: "100%", resize: "vertical" }}
          />
        </div>

        <div className="field" style={{ marginBottom: "var(--space-3)" }}>
          <label htmlFor="grok-farm-shared-password">Shared password</label>
          <input
            id="grok-farm-shared-password"
            className="input mono"
            type="password"
            autoComplete="off"
            value={sharedPassword}
            onChange={(e) => setSharedPassword(e.target.value)}
            placeholder="Used when a line has no password"
            disabled={busy || starting}
            style={{ width: "100%", maxWidth: 420 }}
          />
          <span className="hint">
            Per-line <code className="mono">email|password</code> wins. Bare
            emails need this field.
          </span>
        </div>

        <div className="farm-controls">
          <section
            className="farm-control-group"
            aria-labelledby="grok-farm-opts-label"
          >
            <h2 id="grok-farm-opts-label" className="farm-control-title">
              Options
            </h2>
            <div className="farm-check-grid">
              <label className="check">
                <input
                  type="checkbox"
                  checked={headless}
                  onChange={(e) => setHeadless(e.target.checked)}
                  disabled={busy || starting}
                />
                <span className="check-body">
                  <span className="check-label">headless</span>
                  <span className="check-hint">
                    Hidden browser; OTP needs headed or IMAP
                  </span>
                </span>
              </label>
              <label className="check">
                <input
                  type="checkbox"
                  checked={autoImport}
                  onChange={(e) => setAutoImport(e.target.checked)}
                  disabled={busy || starting}
                />
                <span className="check-body">
                  <span className="check-label">auto-import</span>
                  <span className="check-hint">
                    Each success + final upsert into grok-cli pool
                  </span>
                </span>
              </label>
              <label className="check">
                <input
                  type="checkbox"
                  checked={skipExisting}
                  onChange={(e) => setSkipExisting(e.target.checked)}
                  disabled={busy || starting}
                />
                <span className="check-body">
                  <span className="check-label">skip already farmed</span>
                  <span className="check-hint">
                    Skip emails already in grok-cli pool / output
                  </span>
                </span>
              </label>
            </div>
          </section>

          <section
            className="farm-control-group"
            aria-labelledby="grok-farm-timing-label"
          >
            <h2 id="grok-farm-timing-label" className="farm-control-title">
              Timing & workers
            </h2>
            <div className="farm-num-grid">
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="grok-farm-workers">Workers</label>
                <input
                  id="grok-farm-workers"
                  className="input"
                  type="number"
                  min={1}
                  max={maxWorkers}
                  step={1}
                  value={concurrency}
                  onChange={(e) => {
                    const n = Number(e.target.value);
                    if (!Number.isFinite(n)) {
                      setConcurrency(1);
                      return;
                    }
                    setConcurrency(
                      Math.min(maxWorkers, Math.max(1, Math.floor(n))),
                    );
                  }}
                  disabled={busy || starting}
                  title={`Parallel accounts (1–${maxWorkers}). Default 1.`}
                />
                <span className="hint">1–{maxWorkers} parallel accounts</span>
              </div>
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="grok-farm-retries">Retries</label>
                <input
                  id="grok-farm-retries"
                  className="input"
                  type="number"
                  min={1}
                  max={5}
                  step={1}
                  value={accountRetries}
                  onChange={(e) => {
                    const n = Number(e.target.value);
                    if (!Number.isFinite(n)) {
                      setAccountRetries(2);
                      return;
                    }
                    setAccountRetries(Math.min(5, Math.max(1, Math.floor(n))));
                  }}
                  disabled={busy || starting}
                  title="Attempts per account (login→OAuth→verify)"
                />
                <span className="hint">Attempts per account (1–5)</span>
              </div>
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="grok-farm-delay">Delay (s)</label>
                <input
                  id="grok-farm-delay"
                  className="input"
                  type="number"
                  min={0}
                  step={0.5}
                  value={accountDelay}
                  onChange={(e) => {
                    const n = Number(e.target.value);
                    setAccountDelay(!Number.isFinite(n) || n < 0 ? 0 : n);
                  }}
                  disabled={busy || starting}
                  title="Seconds between accounts or worker stagger"
                />
                <span className="hint">Between accounts / worker stagger</span>
              </div>
            </div>
            {concurrency > 1 && !headless && (
              <p className="farm-warn muted">
                Workers &gt; 1 opens multiple headed browsers. Prefer headless
                or lower workers if the host is constrained.
              </p>
            )}
          </section>
        </div>

        <div className="btn-row">
          <button
            type="submit"
            className="btn btn-primary"
            disabled={busy || starting || !accounts.trim() || !packageOk}
          >
            {starting ? "Starting…" : busy ? "Job running…" : "Start relogin"}
          </button>
          {job &&
            !["succeeded", "failed", "cancelled"].includes(job.status) && (
              <button
                type="button"
                className="btn"
                onClick={() => void onCancel()}
              >
                Cancel
              </button>
            )}
          {job && (
            <button
              type="button"
              className="btn"
              onClick={() => void onImport()}
            >
              Import results
            </button>
          )}
          {job &&
            job.fail > 0 &&
            ["succeeded", "failed", "cancelled"].includes(job.status) && (
              <button
                type="button"
                className="btn btn-primary"
                disabled={busy || starting}
                onClick={() => void onRetryFailed()}
                title="Re-run only accounts that failed in this job"
              >
                {starting ? "Starting…" : `Retry failed (${job.fail})`}
              </button>
            )}
          <Link to="/accounts/grok-cli" className="btn">
            Grok accounts
          </Link>
        </div>
      </form>

      {job && (
        <div className="panel" style={{ marginTop: "var(--space-4)" }}>
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
              <span className={`chip chip-${statusTone(job.status) || "muted"}`}>
                {job.status}
              </span>{" "}
              <span className="mono muted" style={{ fontSize: 12 }}>
                {job.id}
              </span>
            </div>
            <div className="muted" style={{ fontSize: 12 }}>
              ok {job.ok} · fail {job.fail} · total {job.total}
              {job.current_step ? ` · step ${job.current_step}` : ""}
              {job.current_email ? ` · ${job.current_email}` : ""}
            </div>
          </div>
          <div
            className="farm-progress"
            style={{
              marginTop: "var(--space-3)",
              height: 6,
              background: "var(--surface-hover)",
              borderRadius: 999,
              overflow: "hidden",
            }}
          >
            <div
              style={{
                width: `${progressPct}%`,
                height: "100%",
                background: "var(--thread)",
                transition: "width 0.3s ease",
              }}
            />
          </div>
          {job.error && (
            <p
              className="muted"
              style={{ marginTop: "var(--space-2)", color: "var(--blood)" }}
            >
              {job.error}
            </p>
          )}
          {job.import_result && (
            <p
              className="muted mono"
              style={{ marginTop: "var(--space-2)", fontSize: 12 }}
            >
              import {JSON.stringify(job.import_result)}
            </p>
          )}
          <div
            className="farm-log mono"
            style={{
              marginTop: "var(--space-3)",
              maxHeight: 360,
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
                p && typeof p.msg === "string" ? p.msg : ev.line;
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
      )}

      {grokHistory.length > 0 && (
        <div className="panel" style={{ marginTop: "var(--space-4)" }}>
          <h2 style={{ margin: "0 0 var(--space-3)", fontSize: 15 }}>
            Recent jobs
          </h2>
          <div className="table-wrap">
            <table className="table">
              <thead>
                <tr>
                  <th>ID</th>
                  <th>Status</th>
                  <th>Accounts</th>
                  <th>Ok/Fail</th>
                  <th>Finished</th>
                  <th></th>
                </tr>
              </thead>
              <tbody>
                {grokHistory.map((h) => (
                  <tr key={h.id}>
                    <td className="mono">{h.id.slice(0, 8)}</td>
                    <td>{h.status}</td>
                    <td>{h.accounts_count}</td>
                    <td>
                      {h.ok}/{h.fail}
                    </td>
                    <td className="muted">{h.finished_at ?? "—"}</td>
                    <td>
                      {h.fail > 0 &&
                        ["succeeded", "failed", "cancelled"].includes(
                          h.status,
                        ) && (
                          <button
                            type="button"
                            className="btn btn-sm"
                            disabled={busy || starting}
                            onClick={() => {
                              setJob(h);
                              jobIdRef.current = h.id;
                              void onRetryFailedFor(h.id);
                            }}
                          >
                            Retry failed
                          </button>
                        )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </div>
  );
}
