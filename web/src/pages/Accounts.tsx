import { useCallback, useEffect, useMemo, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import {
  ApiError,
  listAccounts,
  listProviderSettings,
  patchProviderLoadBalance,
  type Account,
  type LoadBalanceOption,
  type ProviderSetting,
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
  };
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
    if (a.quota_kind === "tokens" || a.quota_limit > 0) {
      c.quotaKind = "tokens";
      c.quotaLimit += a.quota_limit || 0;
      c.quotaRemaining += a.quota_remaining || 0;
    }
  }
  if (accounts.length > 0 && accounts.every((a) => a.quota_kind === "none" || !a.quota_limit)) {
    c.quotaKind = "none";
  }
  return c;
}

function fmtPoolCredits(c: ProviderCounts): string {
  if (c.quotaKind !== "tokens" || c.quotaLimit <= 0) return "RPM / no credits";
  const rem = new Intl.NumberFormat().format(c.quotaRemaining);
  const lim = new Intl.NumberFormat().format(c.quotaLimit);
  return `${rem} / ${lim} tok`;
}

export function Accounts() {
  const navigate = useNavigate();
  const [accounts, setAccounts] = useState<Account[]>([]);
  const [lbByProvider, setLbByProvider] = useState<
    Record<string, ProviderSetting>
  >({});
  const [strategies, setStrategies] = useState<LoadBalanceOption[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [savingProvider, setSavingProvider] = useState<string | null>(null);
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
              disabled={loading}
            >
              {loading ? <span className="spinner inline-spinner" /> : null}
              Refresh
            </button>
            <Link to="/import" className="btn btn-sm btn-primary">
              Import
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
                    Credits: <strong className="mono">{fmtPoolCredits(stat)}</strong>
                  </span>
                  <span className="provider-card-cta">Open list →</span>
                </div>
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
              </div>
            </div>
          );
        })}
      </div>

      {!loading && total === 0 && !error && (
        <div className="panel empty" style={{ marginTop: "var(--space-4)" }}>
          <p className="flavor">No marionettes bound yet.</p>
          <p>Use + Add on a provider card, or bulk import JSON.</p>
          <div className="btn-row" style={{ justifyContent: "center" }}>
            <button
              type="button"
              className="btn btn-primary"
              onClick={() => setAddProvider("qoder")}
            >
              + Add Qoder
            </button>
            <Link to="/import" className="btn">
              Bulk Import
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
