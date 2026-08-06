import { useCallback, useEffect, useState } from "react";
import { Link } from "react-router-dom";
import {
  ApiError,
  getProcessUsage,
  getStats,
  listAccounts,
  type Account,
  type PoolStats,
  type ProcessUsage,
} from "../lib/api";
import { StatusChip } from "../components/StatusChip";
import { statusTooltip } from "../lib/status";

export function Overview() {
  const [stats, setStats] = useState<PoolStats | null>(null);
  const [errors, setErrors] = useState<Account[]>([]);
  const [usage, setUsage] = useState<ProcessUsage | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [s, acc] = await Promise.all([getStats(), listAccounts()]);
      setStats(s);
      const withErr = acc.accounts
        .filter((a) => a.last_error)
        .sort((a, b) => (b.updated_at || "").localeCompare(a.updated_at || ""))
        .slice(0, 8);
      setErrors(withErr);
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e.message
          : e instanceof Error
            ? e.message
            : "Failed to load stats",
      );
      setStats(null);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // Resource usage polls independently — cheap, no auth, every 2s.
  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      try {
        const u = await getProcessUsage();
        if (!cancelled) setUsage(u);
      } catch {
        // non-fatal: keep last snapshot
      }
    };
    void tick();
    const id = window.setInterval(() => void tick(), 2000);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, []);

  return (
    <div>
      <header className="page-header">
        <h1>Overview</h1>
        <p className="subtitle">Pool health at a glance</p>
      </header>

      {error && (
        <div className="alert alert-error" role="alert">
          {error}
          {error.toLowerCase().includes("401") || error.toLowerCase().includes("unauthor") ? (
            <>
              {" "}
              — set admin key in <Link to="/settings">Settings</Link>.
            </>
          ) : null}
        </div>
      )}

      <div className="toolbar" style={{ justifyContent: "flex-end" }}>
        <button type="button" className="btn btn-sm" onClick={() => void load()} disabled={loading}>
          {loading ? <span className="spinner inline-spinner" /> : null}
          Refresh
        </button>
      </div>

      <div className="stat-grid">
        <StatCard label="Total" value={stats?.total} />
        <StatCard label="Bound" value={stats?.bound} tone="thread" />
        <StatCard label="Sealed" value={stats?.sealed} tone="fog" />
        <StatCard label="Cut" value={stats?.cut} tone="seal" />
        <StatCard label="Fallen" value={stats?.fallen} tone="blood" />
      </div>

      <section className="panel" style={{ marginTop: 24 }}>
        <div style={{ display: "flex", alignItems: "baseline", gap: 12, marginBottom: 12 }}>
          <h2
            style={{
              margin: 0,
              fontFamily: "var(--font-display)",
              fontWeight: 400,
              fontSize: "1.25rem",
            }}
          >
            Health
          </h2>
          <span className="muted" style={{ fontSize: 12 }}>
            Runtime resource usage · refreshed every 2s
          </span>
        </div>

        {!usage || usage.pid === undefined ? (
          <p className="muted">
            <span className="spinner inline-spinner" /> First sample in…
          </p>
        ) : (
          <div className="health-cards">
            <div className="health-card">
              <p className="label">CPU usage</p>
              <div className="health-value-row">
                <span className="value">{(usage.cpu_percent ?? 0).toFixed(1)}%</span>
                <span className="chip chip-bound" title="Sampler is running">
                  <span className="chip-dot" />
                  Live
                </span>
              </div>
              <div className="health-row">
                <span className="health-tag">proc</span>
                <div className="health-track">
                  <div
                    className={`health-fill health-${cpuTone(usage.cpu_percent ?? 0)}`}
                    style={{
                      width: `${Math.min(100, ((usage.cpu_percent ?? 0) / Math.max(1, usage.logical_cores ?? 1)) * 100)}%`,
                    }}
                  />
                </div>
                <span className="mono muted">{(usage.cpu_percent ?? 0).toFixed(1)}%</span>
              </div>
              {usage.automation_running && (
                <div className="health-row">
                  <span className="health-tag">auto</span>
                  <div className="health-track">
                    <div
                      className={`health-fill health-${cpuTone(usage.automation_cpu_percent ?? 0)}`}
                      style={{
                        width: `${Math.min(100, ((usage.automation_cpu_percent ?? 0) / Math.max(1, usage.logical_cores ?? 1)) * 100)}%`,
                      }}
                    />
                  </div>
                  <span className="mono muted">
                    {(usage.automation_cpu_percent ?? 0).toFixed(1)}%
                  </span>
                </div>
              )}
              <p className="muted" style={{ fontSize: 12, margin: "8px 0 0" }}>
                PID {usage.pid} · {usage.logical_cores} logical core
                {usage.logical_cores === 1 ? "" : "s"}
              </p>
            </div>

            <div className="health-card">
              <p className="label">RAM usage</p>
              <div className="health-value-row">
                <span className="value">{fmtBytes(usage.mem_bytes ?? 0)}</span>
                <span className="mono muted" style={{ fontSize: 12 }}>RSS</span>
              </div>
              <div className="health-row">
                <span className="health-tag">proc</span>
                <div className="health-track">
                  <div
                    className={`health-fill health-${memTone(usage.mem_bytes ?? 0, usage.total_mem_bytes ?? 0)}`}
                    style={{
                      width: `${Math.min(100, ((usage.mem_bytes ?? 0) / Math.max(1, usage.total_mem_bytes ?? 1)) * 100)}%`,
                    }}
                  />
                </div>
                <span className="mono muted">{fmtBytes(usage.mem_bytes ?? 0)}</span>
              </div>
              {usage.automation_running && (
                <div className="health-row">
                  <span className="health-tag">auto</span>
                  <div className="health-track">
                    <div
                      className={`health-fill health-${memTone(usage.automation_mem_bytes ?? 0, usage.total_mem_bytes ?? 0)}`}
                      style={{
                        width: `${Math.min(100, ((usage.automation_mem_bytes ?? 0) / Math.max(1, usage.total_mem_bytes ?? 1)) * 100)}%`,
                      }}
                    />
                  </div>
                  <span className="mono muted">{fmtBytes(usage.automation_mem_bytes ?? 0)}</span>
                </div>
              )}
              <p className="muted" style={{ fontSize: 12, margin: "8px 0 0" }}>
                System: {fmtBytes(usage.used_mem_bytes ?? 0)} / {fmtBytes(usage.total_mem_bytes ?? 0)}
                {usage.automation_running
                  ? ` · automation: ${usage.automation_procs} process${usage.automation_procs === 1 ? "" : "es"}`
                  : ""}
              </p>
            </div>
          </div>
        )}
      </section>

      <section className="panel" style={{ marginTop: 24 }}>
        <h2
          style={{
            margin: "0 0 12px",
            fontFamily: "var(--font-display)",
            fontWeight: 400,
            fontSize: "1.25rem",
          }}
        >
          Recent errors
        </h2>
        {loading && !stats ? (
          <p className="muted">Loading…</p>
        ) : errors.length === 0 ? (
          <div className="empty" style={{ padding: "32px 16px" }}>
            <p className="flavor">All threads quiet.</p>
            <p>No recent account errors.</p>
          </div>
        ) : (
          <div className="table-wrap">
            <table className="data">
              <thead>
                <tr>
                  <th>Email</th>
                  <th>Provider</th>
                  <th>Status</th>
                  <th>Error</th>
                  <th>Updated</th>
                </tr>
              </thead>
              <tbody>
                {errors.map((a) => (
                  <tr key={a.id}>
                    <td className="truncate" title={a.email ?? undefined}>
                      {a.email ?? a.name ?? "—"}
                    </td>
                    <td className="mono muted">{a.provider}</td>
                    <td>
                      <StatusChip status={a.status} title={statusTooltip(a)} />
                    </td>
                    <td className="truncate muted" title={a.last_error ?? undefined}>
                      {a.last_error}
                    </td>
                    <td className="mono muted" style={{ whiteSpace: "nowrap" }}>
                      {formatShort(a.updated_at)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      {stats?.by_provider && Object.keys(stats.by_provider).length > 0 && (
        <section className="panel" style={{ marginTop: 16 }}>
          <h2
            style={{
              margin: "0 0 12px",
              fontFamily: "var(--font-display)",
              fontWeight: 400,
              fontSize: "1.25rem",
            }}
          >
            By provider
          </h2>
          <div className="table-wrap">
            <table className="data">
              <thead>
                <tr>
                  <th>Provider</th>
                  <th>Total</th>
                  <th>Bound</th>
                  <th>Sealed</th>
                  <th>Cut</th>
                  <th>Fallen</th>
                </tr>
              </thead>
              <tbody>
                {Object.entries(stats.by_provider).map(([p, v]) => (
                  <tr key={p}>
                    <td className="mono">{p}</td>
                    <td>{v.total}</td>
                    <td>{v.bound}</td>
                    <td>{v.sealed}</td>
                    <td>{v.cut}</td>
                    <td>{v.fallen}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </section>
      )}
    </div>
  );
}

function StatCard({
  label,
  value,
  tone,
}: {
  label: string;
  value: number | undefined;
  tone?: string;
}) {
  return (
    <div className="stat-card" data-tone={tone}>
      <p className="label">{label}</p>
      <p className="value">{value === undefined ? "—" : value}</p>
    </div>
  );
}

function formatShort(iso: string | null | undefined): string {
  if (!iso) return "—";
  try {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    return d.toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return iso;
  }
}

function fmtBytes(n: number): string {
  if (n <= 0) return "0 MB";
  const mb = n / (1024 * 1024);
  if (mb < 1024) return `${mb.toFixed(mb < 100 ? 1 : 0)} MB`;
  return `${(mb / 1024).toFixed(2)} GB`;
}

// Process CPU can exceed 100% of one core; warn near one full core, crit at two.
function cpuTone(pct: number): "ok" | "warn" | "crit" {
  if (pct >= 200) return "crit";
  if (pct >= 100) return "warn";
  return "ok";
}

function memTone(bytes: number, total: number): "ok" | "warn" | "crit" {
  if (total <= 0) return "ok";
  const pct = (bytes / total) * 100;
  if (pct >= 35) return "crit";
  if (pct >= 15) return "warn";
  return "ok";
}
