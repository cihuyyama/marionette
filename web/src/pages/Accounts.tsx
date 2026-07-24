import { useCallback, useEffect, useState } from "react";
import { Link } from "react-router-dom";
import {
  ApiError,
  deleteAccount,
  listAccounts,
  patchAccount,
  refreshAccount,
  type Account,
} from "../lib/api";
import { StatusChip } from "../components/StatusChip";
import { statusTooltip } from "../lib/status";

export function Accounts() {
  const [accounts, setAccounts] = useState<Account[]>([]);
  const [provider, setProvider] = useState("");
  const [status, setStatus] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [detail, setDetail] = useState<Account | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await listAccounts({
        provider: provider || undefined,
        status: status || undefined,
      });
      setAccounts(res.accounts);
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e.message
          : e instanceof Error
            ? e.message
            : "Failed to load accounts",
      );
      setAccounts([]);
    } finally {
      setLoading(false);
    }
  }, [provider, status]);

  useEffect(() => {
    void load();
  }, [load]);

  async function withBusy(id: string, fn: () => Promise<void>) {
    setBusyId(id);
    setError(null);
    try {
      await fn();
      await load();
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e.message
          : e instanceof Error
            ? e.message
            : "Action failed",
      );
    } finally {
      setBusyId(null);
    }
  }

  return (
    <div>
      <header className="page-header">
        <h1>Accounts</h1>
        <p className="subtitle">Bound marionettes</p>
      </header>

      {error && (
        <div className="alert alert-error" role="alert">
          {error}
        </div>
      )}

      <div className="toolbar">
        <div className="field" style={{ margin: 0, minWidth: 140 }}>
          <label htmlFor="f-provider">Provider</label>
          <select
            id="f-provider"
            className="select"
            value={provider}
            onChange={(e) => setProvider(e.target.value)}
          >
            <option value="">All</option>
            <option value="grok-cli">grok-cli</option>
            <option value="qoder">qoder</option>
          </select>
        </div>
        <div className="field" style={{ margin: 0, minWidth: 140 }}>
          <label htmlFor="f-status">Status</label>
          <select
            id="f-status"
            className="select"
            value={status}
            onChange={(e) => setStatus(e.target.value)}
          >
            <option value="">All</option>
            <option value="bound">Bound</option>
            <option value="sealed">Sealed</option>
            <option value="cut">Cut</option>
            <option value="fallen">Fallen</option>
          </select>
        </div>
        <div style={{ flex: 1 }} />
        <button type="button" className="btn btn-sm" onClick={() => void load()} disabled={loading}>
          {loading ? <span className="spinner inline-spinner" /> : null}
          Refresh
        </button>
      </div>

      {!loading && accounts.length === 0 ? (
        <div className="panel empty">
          <p className="flavor">No marionettes bound yet.</p>
          <p>Import from 9Router or farm JSON.</p>
          <Link to="/import" className="btn btn-primary">
            Open Import
          </Link>
        </div>
      ) : (
        <div className="table-wrap">
          <table className="data">
            <thead>
              <tr>
                <th>Email</th>
                <th>Provider</th>
                <th>Status</th>
                <th>Cooldown</th>
                <th>Last used</th>
                <th>Error</th>
                <th>Actions</th>
              </tr>
            </thead>
            <tbody>
              {accounts.map((a) => {
                const busy = busyId === a.id;
                return (
                  <tr key={a.id}>
                    <td>
                      <button
                        type="button"
                        className="btn btn-ghost btn-sm"
                        style={{ padding: 0, color: "var(--parchment)" }}
                        onClick={() => setDetail(a)}
                        title={a.id}
                      >
                        {a.email ?? a.name ?? (
                          <span className="mono muted">{a.id.slice(0, 8)}…</span>
                        )}
                      </button>
                    </td>
                    <td className="mono muted">{a.provider}</td>
                    <td>
                      <StatusChip
                        status={a.status}
                        channeling={busy}
                        title={statusTooltip(a)}
                      />
                    </td>
                    <td className="mono muted" style={{ whiteSpace: "nowrap" }}>
                      {a.cooldown_until ? formatShort(a.cooldown_until) : "—"}
                    </td>
                    <td className="mono muted" style={{ whiteSpace: "nowrap" }}>
                      {a.last_used_at ? formatShort(a.last_used_at) : "—"}
                    </td>
                    <td className="truncate muted" title={a.last_error ?? undefined}>
                      {a.last_error ?? "—"}
                    </td>
                    <td>
                      <div className="actions-cell">
                        <button
                          type="button"
                          className="btn btn-sm"
                          disabled={busy}
                          onClick={() =>
                            void withBusy(a.id, async () => {
                              await patchAccount(a.id, { is_active: !a.is_active });
                            })
                          }
                          title={a.is_active ? "is_active → false" : "is_active → true"}
                        >
                          {a.is_active ? "Disable" : "Enable"}
                        </button>
                        <button
                          type="button"
                          className="btn btn-sm"
                          disabled={busy}
                          onClick={() =>
                            void withBusy(a.id, async () => {
                              await refreshAccount(a.id);
                            })
                          }
                        >
                          Refresh auth
                        </button>
                        <button
                          type="button"
                          className="btn btn-sm btn-danger"
                          disabled={busy}
                          onClick={() => {
                            if (
                              !window.confirm(
                                `Delete account ${a.email ?? a.id}? This cannot be undone.`,
                              )
                            ) {
                              return;
                            }
                            void withBusy(a.id, async () => {
                              await deleteAccount(a.id);
                              if (detail?.id === a.id) setDetail(null);
                            });
                          }}
                        >
                          Delete
                        </button>
                      </div>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}

      {detail && (
        <>
          <div className="drawer-backdrop" onClick={() => setDetail(null)} />
          <aside className="drawer" role="dialog" aria-label="Account detail">
            <div style={{ display: "flex", justifyContent: "space-between", gap: 12 }}>
              <div>
                <h2>{detail.email ?? detail.name ?? "Account"}</h2>
                <p className="meta mono">{detail.id}</p>
              </div>
              <button type="button" className="btn btn-ghost btn-sm" onClick={() => setDetail(null)}>
                Close
              </button>
            </div>
            <StatusChip status={detail.status} title={statusTooltip(detail)} />
            <dl className="kv" style={{ marginTop: 16 }}>
              <dt>Provider</dt>
              <dd className="mono">{detail.provider}</dd>
              <dt>Active</dt>
              <dd>{detail.is_active ? "true" : "false"}</dd>
              <dt>Priority</dt>
              <dd>{detail.priority}</dd>
              <dt>Cooldown</dt>
              <dd className="mono">{detail.cooldown_until ?? "—"}</dd>
              <dt>Last used</dt>
              <dd className="mono">{detail.last_used_at ?? "—"}</dd>
              <dt>Last error</dt>
              <dd>{detail.last_error ?? "—"}</dd>
              <dt>Created</dt>
              <dd className="mono">{detail.created_at}</dd>
              <dt>Updated</dt>
              <dd className="mono">{detail.updated_at}</dd>
            </dl>
            <p className="muted" style={{ fontSize: 12, marginBottom: 8 }}>
              Token fields (masked by API)
            </p>
            <pre className="response">{JSON.stringify(detail.data, null, 2)}</pre>
            {detail.cooldown_until && (
              <div className="btn-row" style={{ marginTop: 16 }}>
                <button
                  type="button"
                  className="btn btn-sm"
                  onClick={() =>
                    void withBusy(detail.id, async () => {
                      const updated = await patchAccount(detail.id, {
                        clear_cooldown: true,
                      });
                      setDetail(updated);
                    })
                  }
                >
                  Clear cooldown
                </button>
              </div>
            )}
          </aside>
        </>
      )}
    </div>
  );
}

function formatShort(iso: string): string {
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
