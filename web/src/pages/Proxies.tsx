import { useCallback, useEffect, useMemo, useState } from "react";
import {
  ApiError,
  assignProxies,
  checkAllProxies,
  checkProxy,
  deleteProxy,
  importProxies,
  listProxies,
  toggleProxy,
  updateProxySettings,
  type Proxy,
  type ProxyAutomationMode,
  type ProxyChatMode,
  type ProxyOnDead,
  type ProxySettings,
} from "../lib/api";

function healthChip(h: Proxy["health"]): string {
  if (h === "ok") return "chip-bound";
  if (h === "dead") return "chip-cut";
  return "chip-sealed";
}

function fmtLatency(ms?: number | null): string {
  if (ms == null) return "—";
  return `${ms} ms`;
}

function maskProxy(p: Proxy): string {
  const auth = p.has_auth && p.username ? `${p.username}:••••@` : "";
  return `${p.scheme}://${auth}${p.host}:${p.port}`;
}

export function ProxiesPage() {
  const [proxies, setProxies] = useState<Proxy[]>([]);
  const [settings, setSettings] = useState<ProxySettings | null>(null);
  const [total, setTotal] = useState(0);
  const [healthy, setHealthy] = useState(0);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [importText, setImportText] = useState("");
  const [importOpen, setImportOpen] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const res = await listProxies();
      setProxies(res.proxies);
      setTotal(res.total);
      setHealthy(res.healthy);
      setSettings(res.settings);
      setError(null);
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e.message
          : e instanceof Error
            ? e.message
            : "Failed to load proxies",
      );
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (!message) return;
    const t = window.setTimeout(() => setMessage(null), 2600);
    return () => window.clearTimeout(t);
  }, [message]);

  const assignedTotal = useMemo(
    () => proxies.reduce((n, p) => n + p.assigned_count, 0),
    [proxies],
  );

  function fail(e: unknown, fallback: string) {
    setError(
      e instanceof ApiError
        ? e.message
        : e instanceof Error
          ? e.message
          : fallback,
    );
  }

  async function onImport() {
    if (!importText.trim()) return;
    setBusy(true);
    setError(null);
    try {
      const res = await importProxies({
        text: importText,
        source: "ui-import",
      });
      setMessage(
        `Imported — ${res.inserted} new · ${res.updated} updated · ${res.skipped} skipped`,
      );
      setImportText("");
      setImportOpen(false);
      await load();
    } catch (e) {
      fail(e, "Import failed");
    } finally {
      setBusy(false);
    }
  }

  async function onCheckAll() {
    setBusy(true);
    setError(null);
    try {
      const res = await checkAllProxies();
      setMessage(`Checked ${res.checked} · ${res.healthy} healthy · ${res.dead} dead`);
      await load();
    } catch (e) {
      fail(e, "Health check failed");
    } finally {
      setBusy(false);
    }
  }

  async function onCheckOne(id: string) {
    try {
      await checkProxy(id);
      await load();
    } catch (e) {
      fail(e, "Check failed");
    }
  }

  async function onToggle(p: Proxy) {
    try {
      await toggleProxy(p.id, !p.is_active);
      await load();
    } catch (e) {
      fail(e, "Toggle failed");
    }
  }

  async function onDelete(id: string) {
    try {
      await deleteProxy(id);
      await load();
    } catch (e) {
      fail(e, "Delete failed");
    }
  }

  async function onAssign(provider: string) {
    setBusy(true);
    setError(null);
    try {
      const res = await assignProxies(provider);
      setMessage(`Assigned ${res.assigned} ${provider} accounts to proxies`);
      await load();
    } catch (e) {
      fail(e, "Assign failed");
    } finally {
      setBusy(false);
    }
  }

  async function onSetting(
    key: "chat_mode" | "automation_mode" | "on_dead",
    value: string,
  ) {
    try {
      const next = await updateProxySettings({
        [key]: value,
      } as Partial<ProxySettings>);
      setSettings(next);
      setMessage("Proxy settings saved");
    } catch (e) {
      fail(e, "Save failed");
    }
  }

  return (
    <div>
      <header className="page-header">
        <div className="page-header-row">
          <div>
            <h1>Proxies</h1>
            <p className="subtitle">
              {total} proxies · {healthy} healthy · {assignedTotal} account
              bindings
            </p>
          </div>
          <div className="btn-row">
            <button
              type="button"
              className="btn btn-sm"
              onClick={() => setImportOpen((v) => !v)}
            >
              Import
            </button>
            <button
              type="button"
              className="btn btn-sm"
              onClick={() => void onCheckAll()}
              disabled={busy || loading || total === 0}
            >
              Health check
            </button>
            <button
              type="button"
              className="btn btn-sm"
              onClick={() => void load()}
              disabled={loading}
            >
              Refresh
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

      {settings && (
        <div className="panel proxy-settings">
          <div className="proxy-setting">
            <label htmlFor="chat_mode">Chat proxy</label>
            <select
              id="chat_mode"
              className="input"
              value={settings.chat_mode}
              onChange={(e) =>
                void onSetting("chat_mode", e.target.value as ProxyChatMode)
              }
            >
              <option value="off">Off — direct VPS IP</option>
              <option value="follow-account">
                Follow account — same proxy as login (recommended)
              </option>
              <option value="rotating">Rotating — least-loaded per call</option>
            </select>
          </div>
          <div className="proxy-setting">
            <label htmlFor="automation_mode">Automation proxy</label>
            <select
              id="automation_mode"
              className="input"
              value={settings.automation_mode}
              onChange={(e) =>
                void onSetting(
                  "automation_mode",
                  e.target.value as ProxyAutomationMode,
                )
              }
            >
              <option value="off">Off</option>
              <option value="sticky">Sticky per account (recommended)</option>
              <option value="rotating">Rotating</option>
            </select>
          </div>
          <div className="proxy-setting">
            <label htmlFor="on_dead">On dead proxy</label>
            <select
              id="on_dead"
              className="input"
              value={settings.on_dead}
              onChange={(e) =>
                void onSetting("on_dead", e.target.value as ProxyOnDead)
              }
            >
              <option value="direct">Fall back to direct</option>
              <option value="reassign">Reassign to healthy proxy</option>
              <option value="fail">Fail the request</option>
            </select>
          </div>
          <div className="proxy-setting proxy-assign">
            <span className="muted">Assign accounts</span>
            <div className="btn-row">
              <button
                type="button"
                className="btn btn-sm"
                onClick={() => void onAssign("grok-cli")}
                disabled={busy}
              >
                grok-cli
              </button>
              <button
                type="button"
                className="btn btn-sm"
                onClick={() => void onAssign("qoder")}
                disabled={busy}
              >
                qoder
              </button>
            </div>
          </div>
        </div>
      )}

      {importOpen && (
        <div className="panel">
          <p className="muted">
            Paste one per line: <code>host:port:user:pass</code>,{" "}
            <code>host:port</code>, or <code>scheme://user:pass@host:port</code>.
            Duplicates (host+port+user) are updated, not re-added.
          </p>
          <textarea
            className="input mono"
            rows={6}
            value={importText}
            onChange={(e) => setImportText(e.target.value)}
            placeholder={"82.21.231.243:7557:user:pass\n104.252.149.193:5607:user:pass"}
          />
          <div className="btn-row">
            <button
              type="button"
              className="btn btn-primary btn-sm"
              onClick={() => void onImport()}
              disabled={busy || !importText.trim()}
            >
              Import proxies
            </button>
            <button
              type="button"
              className="btn btn-sm"
              onClick={() => setImportOpen(false)}
            >
              Cancel
            </button>
          </div>
        </div>
      )}

      {loading ? (
        <div className="panel empty">
          <span className="spinner" /> Loading proxies…
        </div>
      ) : proxies.length === 0 ? (
        <div className="panel empty">
          <p className="flavor">No proxies yet.</p>
          <p>Use Import to paste your proxy pool.</p>
        </div>
      ) : (
        <table className="data">
          <thead>
            <tr>
              <th>Proxy</th>
              <th>Country</th>
              <th>Health</th>
              <th>Latency</th>
              <th>Accounts</th>
              <th>Active</th>
              <th aria-label="Actions" />
            </tr>
          </thead>
          <tbody>
            {proxies.map((p) => (
              <tr key={p.id}>
                <td className="mono">{maskProxy(p)}</td>
                <td>{p.country || "—"}</td>
                <td>
                  <span className={`chip ${healthChip(p.health)}`}>
                    {p.health}
                  </span>
                  {p.last_error && p.health === "dead" ? (
                    <span className="muted proxy-err" title={p.last_error}>
                      {" "}
                      {p.last_error}
                    </span>
                  ) : null}
                </td>
                <td>{fmtLatency(p.latency_ms)}</td>
                <td>{p.assigned_count}</td>
                <td>
                  <button
                    type="button"
                    className={`toggle ${p.is_active ? "toggle-on" : ""}`}
                    onClick={() => void onToggle(p)}
                    aria-pressed={p.is_active}
                    title={p.is_active ? "Active" : "Disabled"}
                  >
                    {p.is_active ? "on" : "off"}
                  </button>
                </td>
                <td className="row-actions">
                  <button
                    type="button"
                    className="btn btn-sm btn-ghost"
                    onClick={() => void onCheckOne(p.id)}
                  >
                    Check
                  </button>
                  <button
                    type="button"
                    className="btn btn-sm btn-ghost"
                    onClick={() => void onDelete(p.id)}
                  >
                    Delete
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
