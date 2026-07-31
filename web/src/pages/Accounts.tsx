import { useCallback, useEffect, useMemo, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import {
  ApiError,
  listAccounts,
  listProviderSettings,
  patchProviderLoadBalance,
  patchProviderPickMode,
  warmupQoderAccounts,
  refreshAllAccounts,
  getRefreshJob,
  getRefreshStatus,
  cancelRefreshAll,
  type Account,
  type LoadBalanceOption,
  type PickModeOption,
  type ProviderSetting,
  type RefreshJob,
} from "../lib/api";
import { AddAccountModal } from "../components/AddAccountModal";
import { PROVIDERS, labelProvider, type ProviderId } from "../lib/providers";

type ProviderCounts = {
  total: number;
  bound: number;
  sealed: number;
  cut: number;
  fallen: number;
  inactive: number;
  quotaLimit: number;
  quotaRemaining: number;
  quotaKind: string;
  freeLimit: number;
  freeRemaining: number;
  freeUsed: number;
  freeClaimed: number;
};

function emptyCounts(): ProviderCounts {
  return {
    total: 0,
    bound: 0,
    sealed: 0,
    cut: 0,
    fallen: 0,
    inactive: 0,
    quotaLimit: 0,
    quotaRemaining: 0,
    quotaKind: "none",
    freeLimit: 0,
    freeRemaining: 0,
    freeUsed: 0,
    freeClaimed: 0,
  };
}

function numData(v: unknown): number {
  if (typeof v === "number" && Number.isFinite(v)) return v;
  if (typeof v === "string" && v.trim() && Number.isFinite(Number(v))) {
    return Number(v);
  }
  return 0;
}

function countFor(accounts: Account[]): ProviderCounts {
  const c = emptyCounts();
  for (const a of accounts) {
    c.total += 1;
    if (!a.is_active) c.inactive += 1;
    const s = (a.status || "bound").toLowerCase();
    if (s === "sealed") c.sealed += 1;
    else if (s === "cut") c.cut += 1;
    else if (s === "fallen") c.fallen += 1;
    else c.bound += 1;
    if (a.quota_kind === "credits") {
      if (c.quotaKind !== "tokens") c.quotaKind = "credits";
      c.quotaLimit += a.quota_limit || 0;
      c.quotaRemaining += a.quota_remaining || 0;
    } else if (a.quota_kind === "tokens" || a.quota_limit > 0) {
      c.quotaKind = "tokens";
      c.quotaLimit += a.quota_limit || 0;
      c.quotaRemaining += a.quota_remaining || 0;
    }
    if (a.provider === "qoder") {
      const fl = Math.max(0, numData(a.data?.free_limit));
      const fr = Math.max(0, numData(a.data?.free_remaining));
      const fuRaw = numData(a.data?.free_used);
      const fu =
        fuRaw > 0 ? fuRaw : fl > 0 ? Math.max(0, fl - fr) : 0;
      if (fl > 0 || fr > 0) {
        c.freeClaimed += 1;
        c.freeLimit += fl;
        c.freeRemaining += fr;
        c.freeUsed += fu;
      }
    }
  }
  return c;
}

function fmtPoolFree(c: ProviderCounts): string {
  if (c.freeLimit <= 0 && c.freeClaimed <= 0) {
    return "Not claimed / not synced";
  }
  const used = new Intl.NumberFormat().format(c.freeUsed);
  const rem = new Intl.NumberFormat().format(c.freeRemaining);
  const lim = new Intl.NumberFormat().format(c.freeLimit);
  return `${used} used · ${rem} / ${lim} left`;
}

function poolFreePct(c: ProviderCounts): number | null {
  if (c.freeLimit <= 0) return null;
  return Math.max(
    0,
    Math.min(100, Math.round((c.freeRemaining / c.freeLimit) * 100)),
  );
}

function fmtPoolCredits(c: ProviderCounts): string {
  if (c.quotaKind === "credits") {
    if (c.quotaLimit <= 0) return "Credits not synced";
    const rem = new Intl.NumberFormat().format(c.quotaRemaining);
    const lim = new Intl.NumberFormat().format(c.quotaLimit);
    return `${rem} / ${lim} cr`;
  }
  if (c.quotaKind !== "tokens" || c.quotaLimit <= 0) return "RPM / no credits";
  const rem = new Intl.NumberFormat().format(c.quotaRemaining);
  const lim = new Intl.NumberFormat().format(c.quotaLimit);
  return `${rem} / ${lim} tok`;
}

function poolCreditPct(c: ProviderCounts): number | null {
  if (
    (c.quotaKind !== "tokens" && c.quotaKind !== "credits") ||
    c.quotaLimit <= 0
  ) {
    return null;
  }
  return Math.max(
    0,
    Math.min(100, Math.round((c.quotaRemaining / c.quotaLimit) * 100)),
  );
}

function creditTone(pct: number): "ok" | "warn" | "crit" {
  if (pct <= 15) return "crit";
  if (pct <= 40) return "warn";
  return "ok";
}

function ProviderCreditBar({ counts }: { counts: ProviderCounts }) {
  const pct = poolCreditPct(counts);
  if (pct === null) {
    const emptyLabel =
      counts.quotaKind === "credits"
        ? "Credits not synced"
        : "No local token budget";
    return (
      <div className="provider-credit-bar provider-credit-bar-none">
        <span className="health-meta mono muted">{emptyLabel}</span>
      </div>
    );
  }
  return (
    <div
      className="provider-credit-bar"
      title={`Pool budget ${fmtPoolCredits(counts)}`}
    >
      <div
        className="health-track"
        role="meter"
        aria-label="Provider credit remaining"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={pct}
      >
        <div
          className={`health-fill health-${creditTone(pct)}`}
          style={{ width: `${pct}%` }}
        />
      </div>
      <span className="health-meta mono muted">{pct}%</span>
    </div>
  );
}

function ProviderFreeBar({ counts }: { counts: ProviderCounts }) {
  const pct = poolFreePct(counts);
  if (pct === null) {
    return (
      <div className="provider-credit-bar provider-credit-bar-none">
        <span className="health-meta mono muted">
          Free Ultimate not claimed / not synced
        </span>
      </div>
    );
  }
  return (
    <div
      className="provider-credit-bar provider-free-bar"
      title={`Ultimate free pool · ${fmtPoolFree(counts)} · ${counts.freeClaimed} claimed account${counts.freeClaimed === 1 ? "" : "s"}`}
    >
      <div
        className="health-track"
        role="meter"
        aria-label="Ultimate free calls remaining"
        aria-valuemin={0}
        aria-valuemax={counts.freeLimit}
        aria-valuenow={counts.freeRemaining}
      >
        <div
          className={`health-fill health-${creditTone(pct)}`}
          style={{ width: `${pct}%` }}
        />
      </div>
      <span className="health-meta mono muted">{pct}%</span>
    </div>
  );
}

export function Accounts() {
  const navigate = useNavigate();
  const [accounts, setAccounts] = useState<Account[]>([]);
  const [lbByProvider, setLbByProvider] = useState<
    Record<string, ProviderSetting>
  >({});
  const [strategies, setStrategies] = useState<LoadBalanceOption[]>([]);
  const [pickModes, setPickModes] = useState<PickModeOption[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [savingProvider, setSavingProvider] = useState<string | null>(null);
  const [warmupBusy, setWarmupBusy] = useState(false);
  const [refreshJob, setRefreshJob] = useState<RefreshJob | null>(null);
  const refreshBusy =
    refreshJob?.status === "running";
  const [addProvider, setAddProvider] = useState<ProviderId | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [acc, prov] = await Promise.all([
        listAccounts(),
        listProviderSettings(),
      ]);
      setAccounts(acc.accounts);
      const map: Record<string, ProviderSetting> = {};
      for (const p of prov.providers) {
        map[p.provider] = p;
      }
      setLbByProvider(map);
      setStrategies(prov.strategies || []);
      setPickModes(prov.pick_modes || []);
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
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    const ctrl = new AbortController();
    void (async () => {
      try {
        const snap = await getRefreshStatus(undefined, ctrl.signal);
        const cur = snap.current;
        if (cur?.status === "running" && cur.provider === "grok-cli") {
          setRefreshJob(cur);
        }
      } catch {
        /* ignore — no running job or offline */
      }
    })();
    return () => ctrl.abort();
  }, []);

  useEffect(() => {
    if (!message) return;
    const t = window.setTimeout(() => setMessage(null), 2500);
    return () => window.clearTimeout(t);
  }, [message]);

  const byProvider = useMemo(() => {
    const map: Record<ProviderId, ProviderCounts> = {
      "grok-cli": emptyCounts(),
      qoder: emptyCounts(),
    };
    for (const p of PROVIDERS) {
      map[p] = countFor(accounts.filter((a) => a.provider === p));
    }
    return map;
  }, [accounts]);

  const total = accounts.length;

  async function onWarmupQoder() {
    const n = byProvider.qoder.total - byProvider.qoder.inactive;
    if (n <= 0) return;
    setWarmupBusy(true);
    setError(null);
    try {
      const res = await warmupQoderAccounts();
      setMessage(
        `Qoder warmup: ${res.ok} ok · ${res.failed} failed · ${res.cut} cut (of ${res.total})`,
      );
      await load();
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e.message
          : e instanceof Error
            ? e.message
            : "Qoder warmup failed",
      );
    } finally {
      setWarmupBusy(false);
    }
  }

  async function onRefreshAllGrok() {
    if (refreshBusy) return;
    setError(null);
    try {
      const job = await refreshAllAccounts({ provider: "grok-cli", force: true });
      setRefreshJob(job);
      setMessage(`Refresh started for ${job.total} grok accounts…`);
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e.message
          : e instanceof Error
            ? e.message
            : "Failed to start refresh-all",
      );
    }
  }

  async function onCancelRefresh() {
    try {
      await cancelRefreshAll();
      setMessage("Cancelling refresh-all…");
    } catch {
      /* ignore — job may have just finished */
    }
  }

  useEffect(() => {
    if (refreshJob?.status !== "running") return;
    const id = refreshJob.id;
    let stop = false;
    const ctrl = new AbortController();
    const tick = async () => {
      try {
        const job = await getRefreshJob(id, undefined, ctrl.signal);
        if (stop) return;
        setRefreshJob(job);
        if (job.status !== "running") {
          setMessage(
            `Refresh done: ${job.ok} ok · ${job.failed} failed · ${job.cut} cut (of ${job.total})`,
          );
          void load();
        }
      } catch {
        /* transient poll error — keep trying */
      }
    };
    const timer = window.setInterval(() => void tick(), 1500);
    return () => {
      stop = true;
      ctrl.abort();
      window.clearInterval(timer);
    };
  }, [refreshJob?.status, refreshJob?.id, load]);

  async function onLoadBalanceChange(provider: string, value: string) {
    setSavingProvider(provider);
    setError(null);
    try {
      const updated = await patchProviderLoadBalance(provider, value);
      setLbByProvider((prev) => ({ ...prev, [provider]: updated }));
      setMessage(
        `${labelProvider(provider)}: ${updated.load_balance_label}`,
      );
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e.message
          : e instanceof Error
            ? e.message
            : "Failed to update load balancing",
      );
    } finally {
      setSavingProvider(null);
    }
  }

  async function onPickModeChange(provider: string, value: string) {
    setSavingProvider(provider);
    setError(null);
    try {
      const updated = await patchProviderPickMode(provider, value);
      setLbByProvider((prev) => ({ ...prev, [provider]: updated }));
      setMessage(
        `${labelProvider(provider)}: ${updated.pick_mode_label || value}`,
      );
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e.message
          : e instanceof Error
            ? e.message
            : "Failed to update Ultimate free pick mode",
      );
    } finally {
      setSavingProvider(null);
    }
  }

  return (
    <div>
      <header className="page-header">
        <div className="page-header-row">
          <div>
            <h1>Accounts</h1>
            <p className="subtitle">Bound marionettes · {total} total</p>
          </div>
          <div className="btn-row">
            <button
              type="button"
              className="btn btn-sm"
              onClick={() => void load()}
              disabled={loading || warmupBusy}
            >
              {loading ? <span className="spinner inline-spinner" /> : null}
              Refresh
            </button>
            <button
              type="button"
              className="btn btn-sm"
              disabled={
                loading ||
                warmupBusy ||
                byProvider.qoder.total - byProvider.qoder.inactive <= 0
              }
              title="Refresh auth + credits + Ultimate free activity for all active Qoder accounts"
              onClick={() => void onWarmupQoder()}
            >
              {warmupBusy
                ? "Warming Qoder…"
                : `Warmup Qoder (${Math.max(0, byProvider.qoder.total - byProvider.qoder.inactive)})`}
            </button>
<Link to="/settings" className="btn btn-sm">
9Router backup
            </Link>
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

      <div className="provider-grid">
        {PROVIDERS.map((provider) => {
          const stat = byProvider[provider];
          const lb = lbByProvider[provider];
          const current =
            lb?.load_balance || strategies[0]?.id || "round_robin";
          const hint =
            strategies.find((s) => s.id === current)?.hint ||
            lb?.load_balance_label ||
            "";
          return (
            <div key={provider} className="provider-card provider-card-static">
              <button
                type="button"
                className="provider-card-main"
                onClick={() => navigate(`/accounts/${provider}`)}
              >
                <div className="provider-card-head">
                  <h2>{labelProvider(provider)}</h2>
                  <span className="mono muted">{stat.total} accounts</span>
                </div>

                <div className="provider-stat-grid">
                  <div className="provider-stat" data-tone="thread">
                    <p className="provider-stat-value">{stat.bound}</p>
                    <p className="provider-stat-label">Bound</p>
                  </div>
                  <div className="provider-stat" data-tone="fog">
                    <p className="provider-stat-value">{stat.sealed}</p>
                    <p className="provider-stat-label">Sealed</p>
                  </div>
                  <div className="provider-stat" data-tone="seal">
                    <p className="provider-stat-value">{stat.cut}</p>
                    <p className="provider-stat-label">Cut</p>
                  </div>
                  <div className="provider-stat" data-tone="blood">
                    <p className="provider-stat-value">{stat.fallen}</p>
                    <p className="provider-stat-label">Fallen</p>
                  </div>
                </div>

                <div className="provider-card-meta">
                  <span className="muted">
                    Credits:{" "}
                    <strong className="mono">{fmtPoolCredits(stat)}</strong>
                  </span>
                  <span className="provider-card-cta">Open list →</span>
                </div>
                <ProviderCreditBar counts={stat} />
                {provider === "qoder" ? (
                  <>
                    <div className="provider-card-meta provider-card-meta-free">
                      <span className="muted">
                        Free Ultimate:{" "}
                        <strong className="mono">{fmtPoolFree(stat)}</strong>
                      </span>
                      {stat.freeClaimed > 0 ? (
                        <span className="muted mono">
                          {stat.freeClaimed}/{stat.total} claimed
                        </span>
                      ) : null}
                    </div>
                    <ProviderFreeBar counts={stat} />
                  </>
                ) : null}
                <div className="provider-card-meta">
                  <span className="muted">
                    Inactive: <strong>{stat.inactive}</strong>
                  </span>
                </div>
              </button>

              <div
                className="provider-lb"
                onClick={(e) => e.stopPropagation()}
                onKeyDown={(e) => e.stopPropagation()}
              >
                <div className="provider-lb-row">
                  <label htmlFor={`lb-${provider}`}>Load balancing</label>
                  <button
                    type="button"
                    className="btn btn-sm btn-primary"
                    onClick={() => setAddProvider(provider)}
                  >
                    + Add
                  </button>
                </div>
                <select
                  id={`lb-${provider}`}
                  className="select"
                  value={current}
                  disabled={savingProvider === provider || loading}
                  onChange={(e) =>
                    void onLoadBalanceChange(provider, e.target.value)
                  }
                >
                  {(strategies.length
                    ? strategies
                    : [
                        {
                          id: "round_robin",
                          label: "Round robin",
                          hint: "",
                        },
                      ]
                  ).map((s) => (
                    <option key={s.id} value={s.id}>
                      {s.label}
                    </option>
                  ))}
                </select>
                {hint ? <p className="provider-lb-hint">{hint}</p> : null}
                {provider === "qoder" ? (
                  <>
                    <label
                      htmlFor={`pick-${provider}`}
                      className="provider-lb-pick-label"
                    >
                      Ultimate free pick
                    </label>
                    <select
                      id={`pick-${provider}`}
                      className="select"
                      value={lb?.pick_mode || "normal"}
                      disabled={savingProvider === provider || loading}
                      onChange={(e) =>
                        void onPickModeChange(provider, e.target.value)
                      }
                    >
                      {(pickModes.length
                        ? pickModes
                        : [
                            {
                              id: "normal",
                              label: "Normal (credits + free)",
                              hint: "",
                            },
                            {
                              id: "ultimate_free",
                              label: "Ultimate free only",
                              hint: "",
                            },
                          ]
                      ).map((m) => (
                        <option key={m.id} value={m.id}>
                          {m.label}
                        </option>
                      ))}
                    </select>
                    {(() => {
                      const pm = lb?.pick_mode || "normal";
                      const ph =
                        pickModes.find((m) => m.id === pm)?.hint ||
                        lb?.pick_mode_label ||
                        "";
                      return ph ? (
                        <p className="provider-lb-hint">{ph}</p>
                      ) : null;
                    })()}
                  </>
                ) : null}
                {provider === "grok-cli" ? (
                  <div className="provider-lb-refresh">
                    <button
                      type="button"
                      className="btn btn-sm"
                      disabled={
                        refreshBusy ||
                        loading ||
                        byProvider["grok-cli"].total -
                          byProvider["grok-cli"].inactive <=
                          0
                      }
                      onClick={() => void onRefreshAllGrok()}
                    >
                      {refreshBusy
                        ? `Refreshing ${refreshJob?.processed ?? 0}/${refreshJob?.total ?? 0}…`
                        : `Refresh all auth (${Math.max(
                            0,
                            byProvider["grok-cli"].total -
                              byProvider["grok-cli"].inactive,
                          )})`}
                    </button>
                    {refreshBusy ? (
                      <button
                        type="button"
                        className="btn btn-sm btn-ghost"
                        onClick={() => void onCancelRefresh()}
                      >
                        Cancel
                      </button>
                    ) : null}
                    {refreshJob && refreshJob.provider === "grok-cli" ? (
                      <p className="provider-lb-hint">
                        {refreshJob.ok} ok · {refreshJob.failed} failed ·{" "}
                        {refreshJob.cut} cut
                      </p>
                    ) : null}
                  </div>
                ) : null}
              </div>
            </div>
          );
        })}
      </div>

      {!loading && total === 0 && !error && (
        <div className="panel empty" style={{ marginTop: "var(--space-4)" }}>
          <p className="flavor">No marionettes bound yet.</p>
          <p>Use + Add on a provider card, or import a 9Router backup.</p>
          <div className="btn-row" style={{ justifyContent: "center" }}>
            <button
              type="button"
              className="btn btn-primary"
              onClick={() => setAddProvider("qoder")}
            >
              + Add Qoder
            </button>
<Link to="/settings" className="btn">
9Router backup
            </Link>
          </div>
        </div>
      )}

      {addProvider && (
        <AddAccountModal
          provider={addProvider}
          open
          onClose={() => setAddProvider(null)}
          onImported={(res) => {
            setMessage(
              `Imported — inserted ${res.inserted}, updated ${res.updated}, skipped ${res.skipped}.`,
            );
            void load();
          }}
        />
      )}
    </div>
  );
}
