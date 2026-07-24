import { useCallback, useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { ApiError, getStats, listAccounts, type Account, type PoolStats } from "../lib/api";
import { StatusChip } from "../components/StatusChip";
import { statusTooltip } from "../lib/status";

export function Overview() {
  const [stats, setStats] = useState<PoolStats | null>(null);
  const [errors, setErrors] = useState<Account[]>([]);
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
