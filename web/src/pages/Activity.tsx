import { useCallback, useEffect, useMemo, useState } from "react";
import {
  ApiError,
  getUsage,
  listRequests,
  type RequestLog,
  type UsageRange,
  type UsageSummary,
} from "../lib/api";
import { labelProvider } from "../lib/providers";

const PER_PAGE = 25;

const RANGE_PILLS: { id: UsageRange; label: string; hint: string }[] = [
  { id: "day", label: "24h", hint: "Rolling last 24 hours" },
  { id: "week", label: "7d", hint: "Rolling last 7 days" },
  { id: "month", label: "30d", hint: "Rolling last 30 days" },
  { id: "all", label: "All", hint: "Entire request log history" },
];

export function ActivityPage() {
  const [usage, setUsage] = useState<UsageSummary | null>(null);
  const [requests, setRequests] = useState<RequestLog[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [search, setSearch] = useState("");
  const [provider, setProvider] = useState("");
  const [range, setRange] = useState<UsageRange>("week");
  const [page, setPage] = useState(1);
  const [detail, setDetail] = useState<RequestLog | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [u, r] = await Promise.all([
        getUsage({ range }),
        listRequests({
          provider: provider || undefined,
          limit: 200,
          range,
        }),
      ]);
      setUsage(u);
      setRequests(r.requests || []);
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e.message
          : e instanceof Error
            ? e.message
            : "Failed to load activity",
      );
      setUsage(null);
      setRequests([]);
    } finally {
      setLoading(false);
    }
  }, [provider, range]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    setPage(1);
  }, [search, provider, range]);

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    if (!q) return requests;
    return requests.filter((r) => {
      return (
        (r.model ?? "").toLowerCase().includes(q) ||
        r.provider.toLowerCase().includes(q) ||
        (r.account_email ?? "").toLowerCase().includes(q) ||
        (r.error_message ?? "").toLowerCase().includes(q) ||
        r.status.toLowerCase().includes(q)
      );
    });
  }, [requests, search]);

  const pageCount = Math.max(1, Math.ceil(filtered.length / PER_PAGE));
  const pageSafe = Math.min(page, pageCount);
  const pageRows = filtered.slice(
    (pageSafe - 1) * PER_PAGE,
    pageSafe * PER_PAGE,
  );

  const rangeMeta = RANGE_PILLS.find((p) => p.id === range) ?? RANGE_PILLS[1];
  const successRate =
    usage && usage.requests > 0
      ? Math.round((usage.success / usage.requests) * 1000) / 10
      : null;

  return (
    <div>
      <header className="page-header">
        <div className="page-header-row">
          <div>
            <h1>Activity</h1>
            <p className="subtitle">
              Usage summary · recent requests (in / out)
            </p>
          </div>
          <button
            type="button"
            className="btn btn-sm"
            onClick={() => void load()}
            disabled={loading}
          >
            {loading ? <span className="spinner inline-spinner" /> : null}
            Refresh
          </button>
        </div>
      </header>

      {error && (
        <div className="alert alert-error" role="alert">
          {error}
        </div>
      )}

      <div className="activity-range-bar">
        <div className="activity-range-copy">
          <p className="activity-range-kicker">Window</p>
          <p className="activity-range-desc" title={rangeMeta.hint}>
            {rangeLabelLong(range)}
            {usage?.since ? (
              <span className="muted">
                {" "}
                · since {formatSince(usage.since)}
              </span>
            ) : null}
            {successRate != null ? (
              <span className="muted"> · {successRate}% ok</span>
            ) : null}
          </p>
        </div>
        <div
          className="status-pills activity-range-pills"
          role="tablist"
          aria-label="Usage time range"
        >
          {RANGE_PILLS.map((p) => (
            <button
              key={p.id}
              type="button"
              role="tab"
              aria-selected={range === p.id}
              title={p.hint}
              className={`status-pill${range === p.id ? " active" : ""}`}
              onClick={() => setRange(p.id)}
            >
              {p.label}
            </button>
          ))}
        </div>
      </div>

      <div
        className={`activity-stats${loading ? " activity-stats-loading" : ""}`}
        aria-busy={loading}
      >
        <Stat label="Requests" value={usage?.requests} />
        <Stat label="Success" value={usage?.success} tone="thread" />
        <Stat label="Errors" value={usage?.errors} tone="blood" />
        <Stat label="In (prompt)" value={usage?.prompt_tokens} tone="fog" />
        <Stat
          label="Out (completion)"
          value={usage?.completion_tokens}
          tone="seal"
        />
        <Stat label="Total tokens" value={usage?.total_tokens} />
      </div>

      {usage && usage.by_model.length > 0 && (
        <section className="panel" style={{ marginBottom: "var(--space-4)" }}>
          <div className="activity-section-head">
            <h2 className="section-title">By model</h2>
            <span className="muted activity-section-meta">
              Top models in {rangeLabelShort(range)}
            </span>
          </div>
          <div className="table-wrap" style={{ border: "none" }}>
            <table className="data">
              <thead>
                <tr>
                  <th>Model</th>
                  <th>Provider</th>
                  <th>Reqs</th>
                  <th>In</th>
                  <th>Out</th>
                  <th>Total</th>
                </tr>
              </thead>
              <tbody>
                {usage.by_model.slice(0, 12).map((m) => (
                  <tr key={`${m.provider}:${m.model}`}>
                    <td className="mono">{m.model}</td>
                    <td className="muted">{labelProvider(m.provider)}</td>
                    <td className="mono">{fmt(m.requests)}</td>
                    <td className="mono token-in">{fmt(m.prompt_tokens)}</td>
                    <td className="mono token-out">
                      {fmt(m.completion_tokens)}
                    </td>
                    <td className="mono">{fmt(m.total_tokens)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </section>
      )}

      <div className="toolbar list-toolbar">
        <div className="search-field">
          <input
            type="search"
            className="input"
            placeholder="Search model, account, error…"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            aria-label="Search requests"
          />
        </div>
        <div className="field" style={{ margin: 0, minWidth: 140 }}>
          <label htmlFor="act-provider">Provider</label>
          <select
            id="act-provider"
            className="select"
            value={provider}
            onChange={(e) => setProvider(e.target.value)}
          >
            <option value="">All</option>
            <option value="grok-cli">grok-cli</option>
            <option value="qoder">qoder</option>
          </select>
        </div>
      </div>

      <div className="activity-section-head">
        <h2 className="section-title">Recent requests</h2>
        <span className="muted activity-section-meta">
          Same window as summary · max 200
        </span>
      </div>

      {filtered.length === 0 && !loading ? (
        <div className="panel empty">
          <p className="flavor">No threads pulled yet.</p>
          <p>Chat completions will appear here after the first request.</p>
        </div>
      ) : (
        <div className="table-wrap">
          <table className="data">
            <thead>
              <tr>
                <th></th>
                <th>Model</th>
                <th>In / Out</th>
                <th>TPS</th>
                <th>Credits</th>
                <th>When</th>
                <th>Provider</th>
                <th>Duration</th>
                <th>Account</th>
              </tr>
            </thead>
            <tbody>
              {pageRows.map((r) => {
                const ok = r.status === "success";
                return (
                  <tr
                    key={r.id}
                    className="row-click"
                    onClick={() => setDetail(r)}
                  >
                    <td>
                      <span
                        className={`req-dot${ok ? " ok" : " err"}`}
                        title={r.status}
                        aria-label={r.status}
                      />
                    </td>
                    <td className="mono truncate" title={r.model ?? undefined}>
                      {r.model ?? "—"}
                      {r.stream ? (
                        <span className="muted" style={{ marginLeft: 6 }}>
                          sse
                        </span>
                      ) : null}
                    </td>
                    <td className="mono nowrap">
                      <span className="token-in">
                        {fmt(r.prompt_tokens)}↑
                      </span>{" "}
                      <span className="token-out">
                        {fmt(r.completion_tokens)}↓
                      </span>
                    </td>
                    <td className="mono muted nowrap">{fmtTps(r)}</td>
                    <td className="mono muted nowrap">{fmtCredits(r)}</td>
                    <td className="muted nowrap">{timeAgo(r.created_at)}</td>
                    <td className="muted">{labelProvider(r.provider)}</td>
                    <td className="mono muted">
                      {r.duration_ms != null
                        ? `${(r.duration_ms / 1000).toFixed(1)}s`
                        : "—"}
                    </td>
                    <td className="truncate muted" title={r.account_email ?? r.account_id ?? undefined}>
                      {r.account_email ??
                        (r.account_id ? `${r.account_id.slice(0, 8)}…` : "—")}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          {filtered.length > PER_PAGE && (
            <div className="pagination">
              <span className="muted">
                {(pageSafe - 1) * PER_PAGE + 1}–
                {Math.min(pageSafe * PER_PAGE, filtered.length)} of{" "}
                {filtered.length}
              </span>
              <div className="btn-row">
                <button
                  type="button"
                  className="btn btn-sm"
                  disabled={pageSafe <= 1}
                  onClick={() => setPage((p) => Math.max(1, p - 1))}
                >
                  Prev
                </button>
                <span className="muted">
                  {pageSafe}/{pageCount}
                </span>
                <button
                  type="button"
                  className="btn btn-sm"
                  disabled={pageSafe >= pageCount}
                  onClick={() => setPage((p) => Math.min(pageCount, p + 1))}
                >
                  Next
                </button>
              </div>
            </div>
          )}
        </div>
      )}

      {detail && (
        <>
          <div className="drawer-backdrop" onClick={() => setDetail(null)} />
          <aside className="drawer" role="dialog" aria-label="Request detail">
            <div
              style={{
                display: "flex",
                justifyContent: "space-between",
                gap: 12,
              }}
            >
              <div>
                <h2 className="mono" style={{ fontSize: "1.1rem" }}>
                  {detail.model ?? "Request"}
                </h2>
                <p className="meta mono">{detail.created_at}</p>
              </div>
              <button
                type="button"
                className="btn btn-ghost btn-sm"
                onClick={() => setDetail(null)}
              >
                Close
              </button>
            </div>
            <dl className="kv" style={{ marginTop: 16 }}>
              <dt>Status</dt>
              <dd>{detail.status}</dd>
              <dt>Provider</dt>
              <dd className="mono">{detail.provider}</dd>
              <dt>Stream</dt>
              <dd>{detail.stream ? "yes" : "no"}</dd>
              <dt>Duration</dt>
              <dd className="mono">
                {detail.duration_ms != null
                  ? `${detail.duration_ms} ms`
                  : "—"}
              </dd>
              <dt>In (prompt)</dt>
              <dd className="mono token-in">{fmt(detail.prompt_tokens)}</dd>
              <dt>Out (completion)</dt>
              <dd className="mono token-out">
                {fmt(detail.completion_tokens)}
              </dd>
              <dt>Total</dt>
              <dd className="mono">{fmt(detail.total_tokens)}</dd>
              <dt>TPS</dt>
              <dd className="mono">{fmtTps(detail)}</dd>
              <dt>Credits</dt>
              <dd className="mono">{fmtCredits(detail)}</dd>
              <dt>Account</dt>
              <dd>
                {detail.account_email ?? detail.account_id ?? "—"}
              </dd>
              <dt>Error</dt>
              <dd>{detail.error_message ?? "—"}</dd>
            </dl>
          </aside>
        </>
      )}
    </div>
  );
}

function Stat({
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
      <p className="value">{value === undefined ? "—" : fmt(value)}</p>
    </div>
  );
}

function rangeLabelShort(r: UsageRange): string {
  switch (r) {
    case "day":
      return "24h";
    case "week":
      return "7d";
    case "month":
      return "30d";
    default:
      return "all time";
  }
}

function rangeLabelLong(r: UsageRange): string {
  switch (r) {
    case "day":
      return "Last 24 hours";
    case "week":
      return "Last 7 days";
    case "month":
      return "Last 30 days";
    default:
      return "All recorded traffic";
  }
}

function formatSince(iso: string): string {
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

function fmt(n: number | null | undefined): string {
  if (n == null || Number.isNaN(n)) return "—";
  return new Intl.NumberFormat().format(n);
}

function fmtTps(r: RequestLog): string {
  const out = r.completion_tokens;
  const ms = r.duration_ms;
  if (out == null || out <= 0 || ms == null || ms <= 0) return "—";
  const tps = out / (ms / 1000);
  if (!Number.isFinite(tps)) return "—";
  return tps >= 100 ? tps.toFixed(0) : tps.toFixed(1);
}

function fmtCredits(r: RequestLog): string {
  if (r.credits_used != null && r.credits_used > 0) {
    return fmt(r.credits_used);
  }
  if (r.account_quota_after != null) {
    return `${fmt(r.account_quota_after)} left`;
  }
  return "—";
}

function timeAgo(iso: string): string {
  try {
    const t = new Date(iso).getTime();
    if (Number.isNaN(t)) return iso;
    const diff = Math.floor((Date.now() - t) / 1000);
    if (diff < 60) return `${Math.max(0, diff)}s ago`;
    if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
    if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
    return new Date(iso).toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return iso;
  }
}
