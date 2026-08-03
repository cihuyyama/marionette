import { useCallback, useEffect, useMemo, useState } from "react";
import { ApiError, listAdminModels, type ModelObject } from "../lib/api";
import { labelProvider } from "../lib/providers";
import { ComboManager } from "../components/ComboManager";
import { Link } from "react-router-dom";

function FlagCell({ on, label }: { on: boolean; label: string }) {
  return on ? (
    <span className="chip chip-bound" title={label}>
      <span className="chip-dot" />
      {label}
    </span>
  ) : (
    <span className="muted">—</span>
  );
}

export function ModelsPage() {
  const [models, setModels] = useState<ModelObject[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [search, setSearch] = useState("");
  const [owner, setOwner] = useState("all");
  const [copied, setCopied] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await listAdminModels();
      setModels(res.data || []);
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e.message
          : e instanceof Error
            ? e.message
            : "Failed to load models",
      );
      setModels([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (!copied) return;
    const t = window.setTimeout(() => setCopied(null), 1500);
    return () => window.clearTimeout(t);
  }, [copied]);

  const owners = useMemo(() => {
    const set = new Set(models.map((m) => m.owned_by));
    return ["all", ...Array.from(set).sort()];
  }, [models]);

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    return models.filter((m) => {
      if (owner !== "all" && m.owned_by !== owner) return false;
      if (!q) return true;
      return (
        m.id.toLowerCase().includes(q) ||
        m.owned_by.toLowerCase().includes(q) ||
        (m.model_key ?? "").toLowerCase().includes(q) ||
        (m.display_name ?? "").toLowerCase().includes(q) ||
        (m.credit_usage_rate ?? "").toLowerCase().includes(q) ||
        (m.max_input ?? "").toLowerCase().includes(q)
      );
    });
  }, [models, search, owner]);

  async function copyId(id: string) {
    try {
      await navigator.clipboard.writeText(id);
      setCopied(id);
    } catch {
      setError("Clipboard not available");
    }
  }

  return (
    <div>
      <header className="page-header">
        <div className="page-header-row">
          <div>
            <h1>Models</h1>
            <p className="subtitle">
              {models.length} models · key · display · price · max input ·
              reasoning / vision
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
          {(error.toLowerCase().includes("unauthor") ||
            error.toLowerCase().includes("401")) && (
            <>
              {" "}
              — re-enter admin key in <Link to="/settings">Settings</Link>.
            </>
          )}
        </div>
      )}

      <div className="toolbar list-toolbar">
        <div className="search-field">
          <input
            type="search"
            className="input"
            placeholder="Search id, key, name, rate…"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            aria-label="Search models"
          />
        </div>
        <div className="status-pills" role="tablist" aria-label="Provider filter">
          {owners.map((o) => (
            <button
              key={o}
              type="button"
              role="tab"
              aria-selected={owner === o}
              className={`status-pill${owner === o ? " active" : ""}`}
              onClick={() => setOwner(o)}
            >
              {o === "all" ? "All" : labelProvider(o)}
            </button>
          ))}
        </div>
      </div>

      {filtered.length === 0 && !loading ? (
        <div className="panel empty">
          <p className="flavor">No models in this filter.</p>
          <p>Check pool key in Settings if the list is empty.</p>
        </div>
      ) : (
        <div className="table-wrap">
          <table className="data">
            <thead>
              <tr>
                <th>Model id</th>
                <th>Key</th>
                <th>Display name</th>
                <th>Price</th>
                <th>Max input</th>
                <th>Reasoning</th>
                <th>Vision</th>
                <th>Default</th>
                <th>Provider</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {filtered.map((m) => (
                <tr key={m.id}>
                  <td className="mono">{m.id}</td>
                  <td className="mono muted">{m.model_key || "—"}</td>
                  <td>{m.display_name || "—"}</td>
                  <td className="mono muted">
                    {m.credit_usage_rate || "—"}
                  </td>
                  <td className="mono">{m.max_input || "—"}</td>
                  <td>
                    <FlagCell on={!!m.reasoning} label="R" />
                  </td>
                  <td>
                    <FlagCell on={!!m.vision} label="VL" />
                  </td>
                  <td>
                    {m.is_default ? (
                      <span className="chip chip-bound" title="Default">
                        <span className="chip-dot" />D
                      </span>
                    ) : (
                      <span className="muted">—</span>
                    )}
                  </td>
                  <td className="muted">{labelProvider(m.owned_by)}</td>
                  <td>
                    <button
                      type="button"
                      className="btn btn-sm"
                      onClick={() => void copyId(m.id)}
                    >
                      {copied === m.id ? "Copied" : "Copy"}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      <ComboManager models={models} />
    </div>
  );
}
