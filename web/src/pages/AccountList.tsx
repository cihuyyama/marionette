import { useCallback, useEffect, useMemo, useState } from "react";
import { Link, Navigate, useNavigate, useParams } from "react-router-dom";
import {
  ApiError,
  claimProTrial,
  deleteAccount,
  listAccounts,
  patchAccount,
  refreshAccount,
  warmupQoderAccounts,
  type Account,
} from "../lib/api";
import { AddAccountModal } from "../components/AddAccountModal";
import { BulkInjectModal } from "../components/BulkInjectModal";
import { InjectModal } from "../components/InjectModal";
import { StatusChip } from "../components/StatusChip";
import { isProviderId, labelProvider, type ProviderId } from "../lib/providers";
import { statusTooltip } from "../lib/status";

type StatusFilter = "all" | "bound" | "sealed" | "cut" | "fallen" | "inactive";
type SortKey = "email" | "status" | "cooldown" | "last_used" | "active";
type SortDir = "asc" | "desc";

const STATUS_PILLS: { id: StatusFilter; label: string }[] = [
  { id: "all", label: "All" },
  { id: "bound", label: "Bound" },
  { id: "sealed", label: "Sealed" },
  { id: "cut", label: "Cut" },
  { id: "fallen", label: "Fallen" },
  { id: "inactive", label: "Inactive" },
];

const PER_PAGE = 25;

export function AccountList() {
  const { provider: rawProvider } = useParams<{ provider: string }>();
  const navigate = useNavigate();
  const provider = isProviderId(rawProvider) ? rawProvider : null;

  const [accounts, setAccounts] = useState<Account[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [bulkBusy, setBulkBusy] = useState(false);
  const [detail, setDetail] = useState<Account | null>(null);
  const [search, setSearch] = useState("");
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  const [sortKey, setSortKey] = useState<SortKey>("email");
  const [sortDir, setSortDir] = useState<SortDir>("asc");
  const [page, setPage] = useState(1);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [addOpen, setAddOpen] = useState(false);
  const [injectTarget, setInjectTarget] = useState<{
    id: string;
    email: string | null;
  } | null>(null);
  const [bulkInjectOpen, setBulkInjectOpen] = useState(false);

  const load = useCallback(async () => {
    if (!provider) return;
    setLoading(true);
    setError(null);
    try {
      const res = await listAccounts({ provider });
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
  }, [provider]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    setPage(1);
    setSelectedIds(new Set());
    setDetail(null);
    setSearch("");
    setStatusFilter("all");
  }, [provider]);

  useEffect(() => {
    setPage(1);
  }, [search, statusFilter]);

  useEffect(() => {
    if (!message) return;
    const t = window.setTimeout(() => setMessage(null), 4000);
    return () => window.clearTimeout(t);
  }, [message]);

  useEffect(() => {
    setSelectedIds((prev) => {
      if (prev.size === 0) return prev;
      const live = new Set(accounts.map((a) => a.id));
      let changed = false;
      const next = new Set<string>();
      for (const id of prev) {
        if (live.has(id)) next.add(id);
        else changed = true;
      }
      return changed ? next : prev;
    });
  }, [accounts]);

  async function withBusy(id: string, fn: () => Promise<void>, okMsg?: string) {
    setBusyId(id);
    setError(null);
    try {
      await fn();
      await load();
      if (okMsg) setMessage(okMsg);
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

  function handleSort(key: SortKey) {
    if (sortKey === key) {
      setSortDir((d) => (d === "asc" ? "desc" : "asc"));
    } else {
      setSortKey(key);
      setSortDir("asc");
    }
  }

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    let rows = accounts.filter((a) => {
      if (statusFilter === "inactive") return !a.is_active;
      if (statusFilter !== "all") {
        return (a.status || "").toLowerCase() === statusFilter;
      }
      return true;
    });
    if (q) {
      rows = rows.filter((a) => {
        const email = (a.email ?? "").toLowerCase();
        const name = (a.name ?? "").toLowerCase();
        const id = a.id.toLowerCase();
        const err = (a.last_error ?? "").toLowerCase();
        return (
          email.includes(q) ||
          name.includes(q) ||
          id.includes(q) ||
          err.includes(q)
        );
      });
    }
    rows = [...rows].sort((a, b) => {
      let cmp = 0;
      switch (sortKey) {
        case "email":
          cmp = (a.email ?? a.name ?? a.id).localeCompare(
            b.email ?? b.name ?? b.id,
          );
          break;
        case "status":
          cmp = (a.status || "").localeCompare(b.status || "");
          break;
        case "cooldown": {
          const ta = a.cooldown_until
            ? new Date(a.cooldown_until).getTime()
            : 0;
          const tb = b.cooldown_until
            ? new Date(b.cooldown_until).getTime()
            : 0;
          cmp = ta - tb;
          break;
        }
        case "last_used": {
          const ta = a.last_used_at ? new Date(a.last_used_at).getTime() : 0;
          const tb = b.last_used_at ? new Date(b.last_used_at).getTime() : 0;
          cmp = ta - tb;
          break;
        }
        case "active":
          cmp = Number(a.is_active) - Number(b.is_active);
          break;
      }
      return sortDir === "asc" ? cmp : -cmp;
    });
    return rows;
  }, [accounts, search, statusFilter, sortKey, sortDir]);

  const pageCount = Math.max(1, Math.ceil(filtered.length / PER_PAGE));
  const pageSafe = Math.min(page, pageCount);
  const pageRows = filtered.slice(
    (pageSafe - 1) * PER_PAGE,
    pageSafe * PER_PAGE,
  );

  const filteredIds = useMemo(() => filtered.map((a) => a.id), [filtered]);
  const selectedVisible = filteredIds.filter((id) => selectedIds.has(id)).length;
  const allVisibleSelected =
    filteredIds.length > 0 && selectedVisible === filteredIds.length;
  const someVisibleSelected =
    selectedVisible > 0 && !allVisibleSelected;

  const activeCount = accounts.filter((a) => a.is_active).length;
  const inactiveCount = accounts.length - activeCount;
  const needInjectCount = accounts.filter(
    (a) => a.is_active && (!a.quota_limit || (a.quota_remaining ?? 0) <= 0),
  ).length;

  function toggleSelect(id: string) {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function toggleSelectAllVisible() {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (allVisibleSelected) {
        for (const id of filteredIds) next.delete(id);
      } else {
        for (const id of filteredIds) next.add(id);
      }
      return next;
    });
  }

  if (!provider) {
    return <Navigate to="/accounts" replace />;
  }

  const providerId = provider;

  async function handleBulkDelete() {
    const ids = Array.from(selectedIds);
    if (ids.length === 0) return;
    if (
      !window.confirm(
        `Delete ${ids.length} ${labelProvider(providerId)} account(s)? This cannot be undone.`,
      )
    ) {
      return;
    }
    setBulkBusy(true);
    setError(null);
    try {
      let deleted = 0;
      for (const id of ids) {
        await deleteAccount(id);
        deleted += 1;
        if (detail?.id === id) setDetail(null);
      }
      setSelectedIds(new Set());
      setMessage(`Deleted ${deleted} account(s)`);
      await load();
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e.message
          : e instanceof Error
            ? e.message
            : "Bulk delete failed",
      );
    } finally {
      setBulkBusy(false);
    }
  }

  async function handleWarmupQoder() {
    if (providerId !== "qoder") return;
    if (accounts.length === 0) return;
    setBulkBusy(true);
    setError(null);
    try {
      const res = await warmupQoderAccounts();
      setMessage(
        `Warmup: ${res.ok} ok · ${res.failed} failed · ${res.cut} cut (of ${res.total})`,
      );
      if (res.failed > 0 || res.cut > 0) {
        const sample = res.results
          .filter((r) => !r.ok)
          .slice(0, 3)
          .map((r) => r.error || "error")
          .join("; ");
        if (sample) setError(`Warmup issues: ${sample}`);
      }
      await load();
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e.message
          : e instanceof Error
            ? e.message
            : "Warmup failed",
      );
    } finally {
      setBulkBusy(false);
    }
  }

  async function handleToggleAll(enable: boolean) {
    const targets = accounts.filter((a) => a.is_active !== enable);
    if (targets.length === 0) return;
    setBulkBusy(true);
    setError(null);
    try {
      for (const a of targets) {
        await patchAccount(a.id, { is_active: enable });
      }
      setMessage(
        enable
          ? `Enabled ${targets.length} account(s)`
          : `Disabled ${targets.length} account(s)`,
      );
      await load();
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e.message
          : e instanceof Error
            ? e.message
            : "Bulk toggle failed",
      );
    } finally {
      setBulkBusy(false);
    }
  }

  return (
    <div>
      <header className="page-header">
        <div className="page-header-row">
          <div className="page-header-title">
            <button
              type="button"
              className="btn btn-ghost btn-sm"
              onClick={() => navigate("/accounts")}
              aria-label="Back to Accounts"
            >
              ←
            </button>
            <div>
              <h1>{labelProvider(providerId)}</h1>
              <p className="subtitle">
                {accounts.length} accounts · {activeCount} active
              </p>
            </div>
          </div>
          <div className="btn-row">
            <button
              type="button"
              className="btn btn-sm"
              onClick={() => void load()}
              disabled={loading || bulkBusy}
            >
              {loading ? <span className="spinner inline-spinner" /> : null}
              Refresh
            </button>
            {providerId === "qoder" && (
              <>
                <button
                  type="button"
                  className="btn btn-sm btn-primary"
                  disabled={bulkBusy || needInjectCount === 0}
                  title="One job: dudul inject all active accounts with no credit / not synced"
                  onClick={() => setBulkInjectOpen(true)}
                >
                  Inject dudul ({needInjectCount})
                </button>
                <button
                  type="button"
                  className="btn btn-sm"
                  disabled={bulkBusy || activeCount === 0}
                  title="Refresh auth + OpenAPI credits for all active Qoder accounts"
                  onClick={() => void handleWarmupQoder()}
                >
                  {bulkBusy ? "Warming…" : `Warmup credits (${activeCount})`}
                </button>
              </>
            )}
            <button
              type="button"
              className="btn btn-sm"
              disabled={bulkBusy || inactiveCount === 0}
              onClick={() => void handleToggleAll(true)}
            >
              Enable all ({inactiveCount})
            </button>
            <button
              type="button"
              className="btn btn-sm"
              disabled={bulkBusy || activeCount === 0}
              onClick={() => void handleToggleAll(false)}
            >
              Disable all ({activeCount})
            </button>
            <button
              type="button"
              className="btn btn-sm btn-primary"
              onClick={() => setAddOpen(true)}
            >
              + Add
            </button>
          </div>
        </div>
      </header>

      {error && (
        <div className="alert alert-error" role="alert">
          {error}
        </div>
      )}
      {message && (
        <div className="alert alert-ok" role="status">
          {message}
        </div>
      )}

      <div className="toolbar list-toolbar">
        <div className="search-field">
          <input
            type="search"
            className="input"
            placeholder="Search email, id, error…"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            aria-label="Search accounts"
          />
        </div>
        <div className="status-pills" role="tablist" aria-label="Status filter">
          {STATUS_PILLS.map((p) => (
            <button
              key={p.id}
              type="button"
              role="tab"
              aria-selected={statusFilter === p.id}
              className={`status-pill${statusFilter === p.id ? " active" : ""}`}
              onClick={() => setStatusFilter(p.id)}
            >
              {p.label}
            </button>
          ))}
        </div>
      </div>

      {selectedIds.size > 0 && (
        <div className="bulk-bar">
          <span>
            {selectedIds.size} selected
          </span>
          <div className="btn-row">
            <button
              type="button"
              className="btn btn-sm"
              disabled={bulkBusy}
              onClick={() => setSelectedIds(new Set())}
            >
              Clear
            </button>
            <button
              type="button"
              className="btn btn-sm btn-danger"
              disabled={bulkBusy}
              onClick={() => void handleBulkDelete()}
            >
              {bulkBusy ? "Deleting…" : `Delete (${selectedIds.size})`}
            </button>
          </div>
        </div>
      )}

      {!loading && filtered.length === 0 ? (
        <div className="panel empty">
          <p className="flavor">No threads in this filter.</p>
          <p>
            {accounts.length === 0
              ? "Add accounts for this provider, or import a 9Router backup."
              : "Try another status or search."}
          </p>
          {accounts.length === 0 && (
            <div className="btn-row" style={{ justifyContent: "center" }}>
              <button
                type="button"
                className="btn btn-primary"
                onClick={() => setAddOpen(true)}
              >
                + Add
              </button>
<Link to="/settings" className="btn">
9Router backup
              </Link>
            </div>
          )}
        </div>
      ) : (
        <div className="table-wrap">
          <table className="data">
            <thead>
              <tr>
                <th className="col-check">
                  <input
                    type="checkbox"
                    aria-label="Select all visible"
                    checked={allVisibleSelected}
                    ref={(el) => {
                      if (el) el.indeterminate = someVisibleSelected;
                    }}
                    onChange={toggleSelectAllVisible}
                  />
                </th>
                <th>
                  <SortHeader
                    label="Email"
                    active={sortKey === "email"}
                    dir={sortDir}
                    onClick={() => handleSort("email")}
                  />
                </th>
                <th>
                  <SortHeader
                    label="Status"
                    active={sortKey === "status"}
                    dir={sortDir}
                    onClick={() => handleSort("status")}
                  />
                </th>
                <th>
                  <SortHeader
                    label="Active"
                    active={sortKey === "active"}
                    dir={sortDir}
                    onClick={() => handleSort("active")}
                  />
                </th>
                <th>
                  <SortHeader
                    label="Cooldown"
                    active={sortKey === "cooldown"}
                    dir={sortDir}
                    onClick={() => handleSort("cooldown")}
                  />
                </th>
                <th>
                  <SortHeader
                    label="Last used"
                    active={sortKey === "last_used"}
                    dir={sortDir}
                    onClick={() => handleSort("last_used")}
                  />
                </th>
                <th>Health</th>
                <th>Error</th>
                <th>Actions</th>
              </tr>
            </thead>
            <tbody>
              {pageRows.map((a) => {
                const busy = busyId === a.id || bulkBusy;
                const selected = selectedIds.has(a.id);
                return (
                  <tr
                    key={a.id}
                    className={`${selected ? "row-selected" : ""}${
                      a.is_active ? "" : " row-inactive"
                    }`}
                  >
                    <td className="col-check">
                      <input
                        type="checkbox"
                        aria-label={`Select ${a.email ?? a.id}`}
                        checked={selected}
                        onChange={() => toggleSelect(a.id)}
                      />
                    </td>
                    <td>
                      <button
                        type="button"
                        className="btn btn-ghost btn-sm email-link"
                        onClick={() => setDetail(a)}
                        title={a.id}
                      >
                        {a.email ?? a.name ?? (
                          <span className="mono muted">
                            {a.id.slice(0, 8)}…
                          </span>
                        )}
                      </button>
                      {a.last_error && (
                        <div
                          className="row-error-hint"
                          title={a.last_error}
                        >
                          {a.last_error}
                        </div>
                      )}
                    </td>
                    <td>
                      <StatusChip
                        status={a.status}
                        channeling={busyId === a.id}
                        title={statusTooltip(a)}
                      />
                    </td>
                    <td>
                      <button
                        type="button"
                        role="switch"
                        aria-checked={a.is_active}
                        className={`toggle${a.is_active ? " on" : ""}`}
                        disabled={busy}
                        title={
                          a.is_active
                            ? "Click to disable"
                            : "Click to enable"
                        }
                        onClick={() =>
                          void withBusy(
                            a.id,
                            async () => {
                              await patchAccount(a.id, {
                                is_active: !a.is_active,
                              });
                            },
                            a.is_active ? "Disabled" : "Enabled",
                          )
                        }
                      >
                        <span className="toggle-knob" />
                      </button>
                    </td>
                    <td className="mono muted nowrap">
                      {a.cooldown_until
                        ? formatShort(a.cooldown_until)
                        : "—"}
                    </td>
                    <td className="mono muted nowrap">
                      {a.last_used_at ? formatShort(a.last_used_at) : "—"}
                    </td>
                    <td className="health-cell" title={fmtCreditTitle(a)}>
                      <AccountHealth a={a} compact />
                    </td>
                    <td
                      className="truncate muted"
                      title={a.last_error ?? undefined}
                    >
                      {a.last_error ?? "—"}
                    </td>
                    <td>
                      <div className="actions-cell">
                        <button
                          type="button"
                          className="btn btn-sm"
                          disabled={busy}
                          title="Refresh auth"
                          onClick={() =>
                            void withBusy(
                              a.id,
                              async () => {
                                await refreshAccount(a.id);
                              },
                              "Auth refreshed",
                            )
                          }
                        >
                          Auth
                        </button>
                        {provider === "qoder" && (
                          <>
                            <button
                              type="button"
                              className="btn btn-sm"
                              disabled={busy}
                              title="Self claim Pro Trial (openapi /api/v1/user/trial, qplot-style)"
                              onClick={() =>
                                void withBusy(
                                  a.id,
                                  async () => {
                                    const res = await claimProTrial(a.id);
                                    if (!res.ok) {
                                      throw new ApiError(
                                        res.message ||
                                          `Claim failed (HTTP ${res.http_status})`,
                                        res.http_status || 400,
                                        res,
                                      );
                                    }
                                    setMessage(
                                      res.message ||
                                        `Trial claimed (HTTP ${res.http_status})`,
                                    );
                                  },
                                  undefined,
                                )
                              }
                            >
                              Claim
                            </button>
                            <button
                              type="button"
                              className="btn btn-sm"
                              disabled={busy}
                              title="Dudul inject — activate Pro Trial for this PAT"
                              onClick={() =>
                                setInjectTarget({
                                  id: a.id,
                                  email: a.email,
                                })
                              }
                            >
                              Inject
                            </button>
                          </>
                        )}
                        {a.cooldown_until && (
                          <button
                            type="button"
                            className="btn btn-sm"
                            disabled={busy}
                            title="Clear cooldown"
                            onClick={() =>
                              void withBusy(
                                a.id,
                                async () => {
                                  await patchAccount(a.id, {
                                    clear_cooldown: true,
                                  });
                                },
                                "Cooldown cleared",
                              )
                            }
                          >
                            CD
                          </button>
                        )}
                        <button
                          type="button"
                          className="btn btn-sm btn-danger"
                          disabled={busy}
                          title="Delete"
                          onClick={() => {
                            if (
                              !window.confirm(
                                `Delete account ${a.email ?? a.id}? This cannot be undone.`,
                              )
                            ) {
                              return;
                            }
                            void withBusy(
                              a.id,
                              async () => {
                                await deleteAccount(a.id);
                                if (detail?.id === a.id) setDetail(null);
                                setSelectedIds((prev) => {
                                  if (!prev.has(a.id)) return prev;
                                  const next = new Set(prev);
                                  next.delete(a.id);
                                  return next;
                                });
                              },
                              "Deleted",
                            );
                          }}
                        >
                          Del
                        </button>
                      </div>
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
          <div
            className="drawer-backdrop"
            onClick={() => setDetail(null)}
          />
          <aside className="drawer" role="dialog" aria-label="Account detail">
            <div
              style={{
                display: "flex",
                justifyContent: "space-between",
                gap: 12,
              }}
            >
              <div>
                <h2>{detail.email ?? detail.name ?? "Account"}</h2>
                <p className="meta mono">{detail.id}</p>
              </div>
              <button
                type="button"
                className="btn btn-ghost btn-sm"
                onClick={() => setDetail(null)}
              >
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
              <dt>Credits / token</dt>
              <dd title={fmtCreditTitle(detail)}>
                <AccountHealth a={detail} />
              </dd>
              {detail.provider === "qoder" && readPlan(detail) && (
                <>
                  <dt>Plan</dt>
                  <dd title={fmtPlanTitle(detail)}>
                    <PlanLabel a={detail} />
                  </dd>
                </>
              )}
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
            <pre className="response">
              {JSON.stringify(detail.data, null, 2)}
            </pre>
            <div className="btn-row" style={{ marginTop: 16 }}>
              <button
                type="button"
                className="btn btn-sm"
                onClick={() =>
                  void withBusy(
                    detail.id,
                    async () => {
                      const updated = await patchAccount(detail.id, {
                        is_active: !detail.is_active,
                      });
                      setDetail(updated);
                    },
                    detail.is_active ? "Disabled" : "Enabled",
                  )
                }
              >
                {detail.is_active ? "Disable" : "Enable"}
              </button>
              {detail.quota_kind === "tokens" && (
                <button
                  type="button"
                  className="btn btn-sm"
                  onClick={() =>
                    void withBusy(
                      detail.id,
                      async () => {
                        const updated = await patchAccount(detail.id, {
                          reset_quota: true,
                        });
                        setDetail(updated);
                      },
                      "Quota reset to 1M",
                    )
                  }
                >
                  Reset quota
                </button>
              )}
              <button
                type="button"
                className="btn btn-sm"
                onClick={() =>
                  void withBusy(
                    detail.id,
                    async () => {
                      const updated = await refreshAccount(detail.id);
                      setDetail(updated);
                    },
                    "Auth refreshed",
                  )
                }
              >
                Refresh auth
              </button>
              {provider === "qoder" && (
                <button
                  type="button"
                  className="btn btn-sm"
                  disabled={busyId === detail.id || bulkBusy}
                  title="Self claim Pro Trial (openapi user/trial)"
                  onClick={() =>
                    void withBusy(
                      detail.id,
                      async () => {
                        const res = await claimProTrial(detail.id);
                        if (res.account) setDetail(res.account);
                        if (!res.ok) {
                          throw new ApiError(
                            res.message ||
                              `Claim failed (HTTP ${res.http_status})`,
                            res.http_status || 400,
                            res,
                          );
                        }
                        setMessage(
                          res.message ||
                            `Trial claimed (HTTP ${res.http_status})`,
                        );
                      },
                      undefined,
                    )
                  }
                >
                  Claim trial
                </button>
              )}
              {provider === "qoder" && (
                <button
                  type="button"
                  className="btn btn-sm btn-primary"
                  disabled={busyId === detail.id || bulkBusy}
                  title="Dudul inject — activate Pro Trial"
                  onClick={() =>
                    setInjectTarget({
                      id: detail.id,
                      email: detail.email,
                    })
                  }
                >
                  Inject dudul
                </button>
              )}
              {detail.cooldown_until && (
                <button
                  type="button"
                  className="btn btn-sm"
                  onClick={() =>
                    void withBusy(
                      detail.id,
                      async () => {
                        const updated = await patchAccount(detail.id, {
                          clear_cooldown: true,
                        });
                        setDetail(updated);
                      },
                      "Cooldown cleared",
                    )
                  }
                >
                  Clear cooldown
                </button>
              )}
            </div>
          </aside>
        </>
      )}

      {provider && (
        <AddAccountModal
          provider={provider as ProviderId}
          open={addOpen}
          onClose={() => setAddOpen(false)}
          onImported={(res) => {
            setMessage(
              `Imported — inserted ${res.inserted}, updated ${res.updated}, skipped ${res.skipped}.`,
            );
            void load();
          }}
        />
      )}

      {injectTarget && (
        <InjectModal
          open
          accountId={injectTarget.id}
          email={injectTarget.email}
          onClose={() => setInjectTarget(null)}
          onStarted={(jobId) => {
            setInjectTarget(null);
            setMessage(`Inject job ${jobId.slice(0, 8)}… started`);
            navigate(`/accounts/qoder/inject/${jobId}`);
          }}
        />
      )}

      {providerId === "qoder" && (
        <BulkInjectModal
          open={bulkInjectOpen}
          needCount={needInjectCount}
          activeCount={activeCount}
          onClose={() => setBulkInjectOpen(false)}
          onStarted={(jobId, summary) => {
            setBulkInjectOpen(false);
            setMessage(summary);
            navigate(`/accounts/qoder/inject/${jobId}`);
          }}
        />
      )}
    </div>
  );
}

function fmtAccountCredit(a: Account): string {
  if (a.quota_kind === "credits") {
    if (!a.quota_limit) return "Not synced";
    const rem = new Intl.NumberFormat().format(a.quota_remaining ?? 0);
    const lim = new Intl.NumberFormat().format(a.quota_limit);
    return `${rem}/${lim} cr`;
  }
  if (a.quota_kind === "none" || !a.quota_limit) return "RPM";
  const rem = new Intl.NumberFormat().format(a.quota_remaining ?? 0);
  const lim = new Intl.NumberFormat().format(a.quota_limit);
  return `${rem}/${lim}`;
}

type PlanInfo = {
  name: string;
  endMs: number | null;
  endAt: string | null;
  leftLabel: string | null;
  expired: boolean;
  isPaid: boolean;
};

function readPlan(a: Account): PlanInfo | null {
  const raw = a.data?.plan_tier_name;
  if (typeof raw !== "string" || !raw.trim()) return null;
  const endMsRaw = a.data?.plan_end_ms;
  let endMs: number | null = null;
  if (typeof endMsRaw === "number" && endMsRaw > 0) endMs = endMsRaw;
  else if (typeof endMsRaw === "string" && Number(endMsRaw) > 0) {
    endMs = Number(endMsRaw);
  }
  const endAtRaw = a.data?.plan_end_at;
  const endAt =
    typeof endAtRaw === "string" && endAtRaw ? endAtRaw : null;
  let leftLabel: string | null = null;
  let expired = false;
  if (endMs != null && endMs > 0) {
    const leftSec = (endMs - Date.now()) / 1000;
    if (leftSec <= 0) {
      expired = true;
      leftLabel = "ended";
    } else {
      leftLabel = formatDuration(leftSec);
    }
  }
  return {
    name: raw.trim(),
    endMs,
    endAt,
    leftLabel,
    expired,
    isPaid: a.data?.plan_is_paid === true,
  };
}

type FreeInfo = {
  limit: number;
  remaining: number;
  used: number;
  endMs: number | null;
  endAt: string | null;
  cliText: string | null;
  activityId: string | null;
};

function numField(v: unknown): number | null {
  if (typeof v === "number" && Number.isFinite(v)) return v;
  if (typeof v === "string" && v.trim() && Number.isFinite(Number(v))) {
    return Number(v);
  }
  return null;
}

function readFree(a: Account): FreeInfo | null {
  if (a.provider !== "qoder") return null;
  const limit = numField(a.data?.free_limit) ?? 0;
  const remaining = numField(a.data?.free_remaining) ?? 0;
  if (limit <= 0 && remaining <= 0) return null;
  const used =
    numField(a.data?.free_used) ?? Math.max(0, limit - remaining);
  const endMs = numField(a.data?.free_end_ms);
  const endAtRaw = a.data?.free_end_at;
  const endAt =
    typeof endAtRaw === "string" && endAtRaw ? endAtRaw : null;
  const cliRaw = a.data?.free_cli_text;
  const cliText =
    typeof cliRaw === "string" && cliRaw.trim() ? cliRaw.trim() : null;
  const idRaw = a.data?.free_activity_id;
  const activityId =
    typeof idRaw === "string" && idRaw.trim() ? idRaw.trim() : null;
  return {
    limit,
    remaining: Math.max(0, remaining),
    used: Math.max(0, used),
    endMs: endMs != null && endMs > 0 ? endMs : null,
    endAt,
    cliText,
    activityId,
  };
}

function fmtFreeTitle(a: Account): string {
  const f = readFree(a);
  if (!f) return "No Ultimate free promo synced — refresh account";
  const bits = [
    f.cliText ?? `Free Ultimate ${f.remaining}/${f.limit}`,
    f.activityId ? `id ${f.activityId}` : null,
    f.endAt ? `ends ${f.endAt}` : f.endMs ? `ends ms ${f.endMs}` : null,
  ].filter(Boolean);
  return bits.join(" · ");
}

function FreeLabel({ a, compact = false }: { a: Account; compact?: boolean }) {
  const f = readFree(a);
  if (!f) return null;
  const pct =
    f.limit > 0
      ? Math.max(0, Math.min(100, Math.round((f.remaining / f.limit) * 100)))
      : 0;
  const tone = healthTone(pct);
  const label = compact
    ? `Free ${f.remaining}/${f.limit}`
    : `Free Ultimate ${f.remaining}/${f.limit}`;
  return (
    <span
      className={`plan-chip plan-${tone}${compact ? " plan-chip-compact" : ""}`}
      title={fmtFreeTitle(a)}
    >
      {label}
    </span>
  );
}

function fmtPlanTitle(a: Account): string {
  const p = readPlan(a);
  if (!p) return "Plan not synced — refresh account";
  const bits = [p.name];
  if (p.isPaid) bits.push("paid");
  if (p.endMs == null) bits.push("no end date (Free-style)");
  else if (p.expired) bits.push(`ended ${p.endAt ?? ""}`.trim());
  else bits.push(`ends in ~${p.leftLabel} (${p.endAt ?? p.endMs})`);
  return bits.join(" · ");
}

function PlanLabel({ a, compact = false }: { a: Account; compact?: boolean }) {
  const p = readPlan(a);
  if (!p) return null;
  const tone =
    p.name.toLowerCase().includes("free") || p.expired
      ? "crit"
      : p.name.toLowerCase().includes("trial")
        ? "warn"
        : "ok";
  return (
    <span
      className={`plan-chip plan-${tone}${compact ? " plan-chip-compact" : ""}`}
      title={fmtPlanTitle(a)}
    >
      {p.name}
      {p.leftLabel
        ? p.expired
          ? " · ended"
          : ` · ${p.leftLabel}`
        : ""}
    </span>
  );
}

function fmtCreditTitle(a: Account): string {
  if (a.quota_kind === "credits") {
    if (!a.quota_limit) {
      return "Qoder credits not synced yet — refresh account or run a chat";
    }
    const parts = [`Qoder credits ${fmtAccountCredit(a)} (OpenAPI)`];
    const plan = readPlan(a);
    if (plan) parts.push(fmtPlanTitle(a));
    const free = readFree(a);
    if (free) parts.push(fmtFreeTitle(a));
    return parts.join(" · ");
  }
  if (a.quota_kind === "none" || !a.quota_limit) {
    return "No local quota budget";
  }
  const parts = [
    `Quota ${fmtAccountCredit(a)} (local budget, default 1M for grok-cli)`,
  ];
  const ttl = readTtl(a);
  if (ttl) {
    parts.push(
      ttl.pct <= 0
        ? `Access token expired (${ttl.expiresAt})`
        : `Access TTL ~${ttl.label} left (${ttl.pct}% of window)`,
    );
  }
  return parts.join(" · ");
}

type TtlInfo = {
  pct: number;
  label: string;
  expiresAt: string;
};

function readTtl(a: Account): TtlInfo | null {
  const raw = a.data?.expiresAt ?? a.data?.expires_at;
  if (typeof raw !== "string" || !raw) return null;
  const expMs = Date.parse(raw);
  if (Number.isNaN(expMs)) return null;
  const expiresInRaw = a.data?.expiresIn ?? a.data?.expires_in;
  const windowSec =
    typeof expiresInRaw === "number" && expiresInRaw > 0
      ? expiresInRaw
      : typeof expiresInRaw === "string" && Number(expiresInRaw) > 0
        ? Number(expiresInRaw)
        : 21600;
  const leftSec = Math.max(0, (expMs - Date.now()) / 1000);
  const pct = Math.max(
    0,
    Math.min(100, Math.round((leftSec / windowSec) * 100)),
  );
  return { pct, label: formatDuration(leftSec), expiresAt: raw };
}

function quotaPct(a: Account): number | null {
  if (
    a.quota_kind === "none" ||
    (a.quota_kind !== "tokens" && a.quota_kind !== "credits") ||
    !a.quota_limit ||
    a.quota_limit <= 0
  ) {
    return null;
  }
  return Math.max(
    0,
    Math.min(
      100,
      Math.round(((a.quota_remaining ?? 0) / a.quota_limit) * 100),
    ),
  );
}

function healthTone(pct: number): "ok" | "warn" | "crit" {
  if (pct <= 15) return "crit";
  if (pct <= 40) return "warn";
  return "ok";
}

function formatDuration(sec: number): string {
  if (sec <= 0) return "0m";
  const h = Math.floor(sec / 3600);
  const m = Math.floor((sec % 3600) / 60);
  if (h >= 24) {
    const d = Math.floor(h / 24);
    const rh = h % 24;
    return rh > 0 ? `${d}d ${rh}h` : `${d}d`;
  }
  if (h > 0) return m > 0 ? `${h}h ${m}m` : `${h}h`;
  return `${Math.max(1, m)}m`;
}

function AccountHealth({
  a,
  compact = false,
}: {
  a: Account;
  compact?: boolean;
}) {
  const qPct = quotaPct(a);
  const ttl = readTtl(a);
  const plan = a.provider === "qoder" ? readPlan(a) : null;
  const free = a.provider === "qoder" ? readFree(a) : null;

  if (qPct === null && !ttl && !plan && !free) {
    return (
      <span className="mono muted nowrap">{fmtAccountCredit(a)}</span>
    );
  }

  return (
    <div
      className={`health-stack${compact ? " health-stack-compact" : ""}`}
      title={fmtCreditTitle(a)}
    >
      {plan && (
        <div
          className="health-row"
          style={
            compact
              ? { gridTemplateColumns: "1fr" }
              : { gridTemplateColumns: "auto 1fr" }
          }
        >
          {!compact && <span className="health-tag">Plan</span>}
          <PlanLabel a={a} compact={compact} />
        </div>
      )}
      {free && (
        <div
          className="health-row"
          style={
            compact
              ? { gridTemplateColumns: "1fr" }
              : { gridTemplateColumns: "auto 1fr" }
          }
        >
          {!compact && <span className="health-tag">Free</span>}
          <FreeLabel a={a} compact={compact} />
        </div>
      )}
      {qPct !== null && (
        <div className="health-row">
          {!compact && <span className="health-tag">Quota</span>}
          <div
            className="health-track"
            role="meter"
            aria-label="Quota remaining"
            aria-valuemin={0}
            aria-valuemax={a.quota_limit}
            aria-valuenow={a.quota_remaining ?? 0}
          >
            <div
              className={`health-fill health-${healthTone(qPct)}`}
              style={{ width: `${qPct}%` }}
            />
          </div>
          <span className="health-meta mono muted">
            {fmtAccountCredit(a)}
          </span>
        </div>
      )}
      {ttl && (
        <div className="health-row">
          {!compact && <span className="health-tag">TTL</span>}
          <div
            className="health-track"
            role="meter"
            aria-label="Access token time remaining"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={ttl.pct}
          >
            <div
              className={`health-fill health-${healthTone(ttl.pct)}`}
              style={{ width: `${ttl.pct}%` }}
            />
          </div>
          <span className="health-meta mono muted">
            {compact ? ttl.label : `${ttl.label} left`}
          </span>
        </div>
      )}
    </div>
  );
}

function SortHeader({
  label,
  active,
  dir,
  onClick,
}: {
  label: string;
  active: boolean;
  dir: SortDir;
  onClick: () => void;
}) {
  return (
    <button type="button" className="th-sort" onClick={onClick}>
      {label}
      <span className="th-sort-icon" aria-hidden>
        {active ? (dir === "asc" ? "↑" : "↓") : "↕"}
      </span>
    </button>
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
