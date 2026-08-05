import { Fragment, useCallback, useEffect, useMemo, useState } from "react";
import { Link } from "react-router-dom";
import {
  ApiError,
  apiKeyUsage,
  createApiKey,
  deleteApiKey,
  listAdminModels,
  listApiKeys,
  patchApiKey,
  type ApiKey,
  type ApiKeyUsage,
  type ModelObject,
} from "../lib/api";

function errText(e: unknown, fallback: string): string {
  if (e instanceof ApiError) return e.message;
  if (e instanceof Error) return e.message;
  return fallback;
}

const numFmt = new Intl.NumberFormat();
const compactFmt = new Intl.NumberFormat(undefined, {
  notation: "compact",
  maximumFractionDigits: 1,
});

function fmtNum(n: number): string {
  return numFmt.format(n);
}

function fmtCompact(n: number): string {
  return compactFmt.format(n);
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

function parseLimit(raw: string): { ok: boolean; value: number | null } {
  const t = raw.trim();
  if (!t) return { ok: true, value: null };
  const n = Number(t);
  if (!Number.isFinite(n) || !Number.isInteger(n) || n < 0) {
    return { ok: false, value: null };
  }
  return { ok: true, value: n };
}

function meterPct(used: number, limit: number | null): number | null {
  if (limit == null || limit <= 0) return null;
  return Math.max(0, Math.min(100, Math.round((used / limit) * 100)));
}

function usageTone(pct: number): "ok" | "warn" | "crit" {
  if (pct >= 90) return "crit";
  if (pct >= 70) return "warn";
  return "ok";
}

function useEscape(onClose: () => void) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
}

function AllowlistPicker({
  models,
  selected,
  onChange,
  hint,
}: {
  models: ModelObject[];
  selected: string[];
  onChange: (next: string[]) => void;
  hint: string;
}) {
  function toggle(id: string) {
    onChange(
      selected.includes(id)
        ? selected.filter((x) => x !== id)
        : [...selected, id],
    );
  }
  return (
    <div style={{ gridColumn: "1 / -1" }}>
      <div
        className="combo-targets-head"
        style={{ marginBottom: "var(--space-2)" }}
      >
        <span>Model allowlist</span>
        <span className="muted" style={{ fontSize: 12 }}>
          {selected.length ? `${selected.length} selected` : "all models"}
        </span>
      </div>
      <div
        style={{
          maxHeight: 180,
          overflow: "auto",
          background: "var(--void)",
          border: "1px solid var(--border)",
          borderRadius: "var(--radius)",
          padding: "var(--space-2) var(--space-3)",
        }}
      >
        {models.length === 0 ? (
          <span className="muted">Model catalog unavailable</span>
        ) : (
          models.map((m) => (
            <label
              key={m.id}
              className="checkbox-label"
              style={{ display: "flex", width: "100%", padding: "2px 0" }}
            >
              <input
                type="checkbox"
                checked={selected.includes(m.id)}
                onChange={() => toggle(m.id)}
              />
              <span className="mono" style={{ fontSize: 12 }}>
                {m.id}
              </span>
            </label>
          ))
        )}
      </div>
      <p className="muted" style={{ fontSize: 12, margin: "6px 0 0" }}>
        {hint}
      </p>
    </div>
  );
}

function LimitInputs({
  rpm,
  setRpm,
  reqLimit,
  setReqLimit,
  tokLimit,
  setTokLimit,
}: {
  rpm: string;
  setRpm: (v: string) => void;
  reqLimit: string;
  setReqLimit: (v: string) => void;
  tokLimit: string;
  setTokLimit: (v: string) => void;
}) {
  return (
    <>
      <label>
        Rate limit (req/min)
        <input
          className="input mono"
          type="number"
          min={0}
          placeholder="unlimited"
          value={rpm}
          onChange={(e) => setRpm(e.target.value)}
          aria-label="Rate limit in requests per minute"
        />
      </label>
      <label>
        Request budget
        <input
          className="input mono"
          type="number"
          min={0}
          placeholder="unlimited"
          value={reqLimit}
          onChange={(e) => setReqLimit(e.target.value)}
          aria-label="Total request budget"
        />
      </label>
      <label>
        Token budget
        <input
          className="input mono"
          type="number"
          min={0}
          placeholder="unlimited"
          value={tokLimit}
          onChange={(e) => setTokLimit(e.target.value)}
          aria-label="Total token budget"
        />
      </label>
    </>
  );
}

function CreateKeyModal({
  models,
  onClose,
}: {
  models: ModelObject[];
  onClose: (reload: boolean) => void;
}) {
  const [name, setName] = useState("");
  const [rpm, setRpm] = useState("");
  const [reqLimit, setReqLimit] = useState("");
  const [tokLimit, setTokLimit] = useState("");
  const [selected, setSelected] = useState<string[]>([]);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [created, setCreated] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  useEscape(() => onClose(created !== null));

  async function onSubmit() {
    const rpmP = parseLimit(rpm);
    const reqP = parseLimit(reqLimit);
    const tokP = parseLimit(tokLimit);
    if (!rpmP.ok || !reqP.ok || !tokP.ok) {
      setError("Limits must be whole numbers >= 0 (empty = unlimited)");
      return;
    }
    setSaving(true);
    setError(null);
    try {
      const body: {
        name?: string;
        rate_limit_rpm?: number;
        request_limit?: number;
        token_limit?: number;
        model_allowlist?: string[];
      } = {};
      if (name.trim()) body.name = name.trim();
      if (rpmP.value != null) body.rate_limit_rpm = rpmP.value;
      if (reqP.value != null) body.request_limit = reqP.value;
      if (tokP.value != null) body.token_limit = tokP.value;
      if (selected.length) body.model_allowlist = selected;
      const res = await createApiKey(body);
      setCreated(res.key);
    } catch (e) {
      setError(errText(e, "Failed to create key"));
    } finally {
      setSaving(false);
    }
  }

  async function copyKey() {
    if (!created) return;
    try {
      await navigator.clipboard.writeText(created);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      setError("Clipboard blocked — select the text and copy manually");
    }
  }

  return (
    <>
      <div className="drawer-backdrop" onClick={() => onClose(created !== null)} />
      <div
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-label="Create API key"
      >
        <div className="modal-head">
          <div>
            <h2>New key</h2>
            <p className="muted" style={{ margin: 0 }}>
              {created
                ? "Key created — copy it now"
                : "Scoped pool access with per-key limits"}
            </p>
          </div>
          {!created && (
            <button
              type="button"
              className="btn btn-ghost btn-sm"
              onClick={() => onClose(false)}
            >
              Cancel
            </button>
          )}
        </div>

        {error && (
          <div className="alert alert-error" role="alert">
            {error}
          </div>
        )}

        {created ? (
          <>
            <div className="alert alert-info" role="status">
              This key is shown only once. Store it now — it cannot be
              retrieved again.
            </div>
            <div
              className="mono"
              style={{
                background: "var(--void)",
                border: "1px solid var(--border)",
                borderRadius: "var(--radius)",
                padding: "var(--space-3)",
                fontSize: 12.5,
                lineHeight: 1.5,
                wordBreak: "break-all",
                margin: "0 0 var(--space-3)",
                color: "var(--parchment)",
              }}
            >
              {created}
            </div>
            <div className="btn-row" style={{ justifyContent: "flex-end" }}>
              <button
                type="button"
                className="btn btn-sm"
                onClick={() => void copyKey()}
              >
                {copied ? "Copied" : "Copy key"}
              </button>
              <button
                type="button"
                className="btn btn-sm btn-primary"
                onClick={() => onClose(true)}
              >
                Done
              </button>
            </div>
          </>
        ) : (
          <>
            <div className="form-grid">
              <label style={{ gridColumn: "1 / -1" }}>
                Name
                <input
                  className="input"
                  placeholder="ci-runner (optional)"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  aria-label="Key name"
                />
              </label>
              <LimitInputs
                rpm={rpm}
                setRpm={setRpm}
                reqLimit={reqLimit}
                setReqLimit={setReqLimit}
                tokLimit={tokLimit}
                setTokLimit={setTokLimit}
              />
              <AllowlistPicker
                models={models}
                selected={selected}
                onChange={setSelected}
                hint="Empty fields = unlimited. No models selected = all models allowed."
              />
            </div>
            <div
              className="btn-row"
              style={{ justifyContent: "flex-end", marginTop: "var(--space-3)" }}
            >
              <button
                type="button"
                className="btn btn-sm btn-primary"
                onClick={() => void onSubmit()}
                disabled={saving}
              >
                {saving ? (
                  <>
                    <span className="spinner inline-spinner" /> Creating…
                  </>
                ) : (
                  "Create key"
                )}
              </button>
            </div>
          </>
        )}
      </div>
    </>
  );
}

function EditKeyModal({
  keyData,
  models,
  onClose,
}: {
  keyData: ApiKey;
  models: ModelObject[];
  onClose: (reload: boolean) => void;
}) {
  const [name, setName] = useState(keyData.name ?? "");
  const [rpm, setRpm] = useState(
    keyData.rate_limit_rpm != null ? String(keyData.rate_limit_rpm) : "",
  );
  const [reqLimit, setReqLimit] = useState(
    keyData.request_limit != null ? String(keyData.request_limit) : "",
  );
  const [tokLimit, setTokLimit] = useState(
    keyData.token_limit != null ? String(keyData.token_limit) : "",
  );
  const [selected, setSelected] = useState<string[]>(
    keyData.model_allowlist ?? [],
  );
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEscape(() => onClose(false));

  async function onSave() {
    const rpmP = parseLimit(rpm);
    const reqP = parseLimit(reqLimit);
    const tokP = parseLimit(tokLimit);
    if (!rpmP.ok || !reqP.ok || !tokP.ok) {
      setError("Limits must be whole numbers >= 0 (empty clears the limit)");
      return;
    }
    setSaving(true);
    setError(null);
    try {
      const body: {
        name?: string;
        rate_limit_rpm: number | null;
        request_limit: number | null;
        token_limit: number | null;
        model_allowlist: string[] | null;
      } = {
        rate_limit_rpm: rpmP.value,
        request_limit: reqP.value,
        token_limit: tokP.value,
        model_allowlist: selected.length ? selected : null,
      };
      if (name.trim()) body.name = name.trim();
      await patchApiKey(keyData.id, body);
      onClose(true);
    } catch (e) {
      setError(errText(e, "Failed to update key"));
      setSaving(false);
    }
  }

  return (
    <>
      <div className="drawer-backdrop" onClick={() => onClose(false)} />
      <div
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-label={`Edit API key ${keyData.key_prefix ?? keyData.id}`}
      >
        <div className="modal-head">
          <div>
            <h2>Edit key</h2>
            <p className="muted mono" style={{ margin: 0 }}>
              {keyData.key_prefix ?? keyData.id.slice(0, 8)}…
            </p>
          </div>
          <button
            type="button"
            className="btn btn-ghost btn-sm"
            onClick={() => onClose(false)}
          >
            Cancel
          </button>
        </div>

        {error && (
          <div className="alert alert-error" role="alert">
            {error}
          </div>
        )}

        <div className="form-grid">
          <label style={{ gridColumn: "1 / -1" }}>
            Name
            <input
              className="input"
              placeholder="Unnamed (empty keeps current)"
              value={name}
              onChange={(e) => setName(e.target.value)}
              aria-label="Key name"
            />
          </label>
          <LimitInputs
            rpm={rpm}
            setRpm={setRpm}
            reqLimit={reqLimit}
            setReqLimit={setReqLimit}
            tokLimit={tokLimit}
            setTokLimit={setTokLimit}
          />
          <AllowlistPicker
            models={models}
            selected={selected}
            onChange={setSelected}
            hint="Clear a field to remove its limit. Select no models to allow all."
          />
        </div>
        <div
          className="btn-row"
          style={{ justifyContent: "flex-end", marginTop: "var(--space-3)" }}
        >
          <button
            type="button"
            className="btn btn-sm btn-primary"
            onClick={() => void onSave()}
            disabled={saving}
          >
            {saving ? (
              <>
                <span className="spinner inline-spinner" /> Saving…
              </>
            ) : (
              "Save changes"
            )}
          </button>
        </div>
      </div>
    </>
  );
}

function UsageCell({ keyData }: { keyData: ApiKey }) {
  const reqPct = meterPct(keyData.requests_used, keyData.request_limit);
  const tokPct = meterPct(keyData.tokens_used, keyData.token_limit);
  const rpmNote =
    keyData.rate_limit_rpm != null
      ? `${fmtNum(keyData.rate_limit_rpm)}/min`
      : null;

  if (reqPct === null && tokPct === null && !rpmNote) {
    return <span className="muted">unlimited</span>;
  }

  return (
    <div className="health-stack health-stack-compact" style={{ minWidth: 130 }}>
      {reqPct !== null && (
        <div className="health-row">
          <div
            className="health-track"
            role="meter"
            aria-label="Requests used"
            aria-valuemin={0}
            aria-valuemax={keyData.request_limit ?? 0}
            aria-valuenow={keyData.requests_used}
          >
            <div
              className={`health-fill health-${usageTone(reqPct)}`}
              style={{ width: `${reqPct}%` }}
            />
          </div>
          <span className="health-meta mono muted">
            {fmtCompact(keyData.requests_used)}/{fmtCompact(keyData.request_limit ?? 0)} req
          </span>
        </div>
      )}
      {tokPct !== null && (
        <div className="health-row">
          <div
            className="health-track"
            role="meter"
            aria-label="Tokens used"
            aria-valuemin={0}
            aria-valuemax={keyData.token_limit ?? 0}
            aria-valuenow={keyData.tokens_used}
          >
            <div
              className={`health-fill health-${usageTone(tokPct)}`}
              style={{ width: `${tokPct}%` }}
            />
          </div>
          <span className="health-meta mono muted">
            {fmtCompact(keyData.tokens_used)}/{fmtCompact(keyData.token_limit ?? 0)} tok
          </span>
        </div>
      )}
      {rpmNote && (
        <span className="health-meta mono muted">{rpmNote} cap</span>
      )}
    </div>
  );
}

function UsageDetail({
  keyId,
  onClose,
}: {
  keyId: string;
  onClose: () => void;
}) {
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [data, setData] = useState<ApiKeyUsage | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const res = await apiKeyUsage(keyId);
        if (!cancelled) setData(res);
      } catch (e) {
        if (!cancelled) setError(errText(e, "Failed to load usage"));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [keyId]);

  const rows = data?.by_model ?? data?.models ?? [];
  const total = data?.requests ?? 0;

  return (
    <td colSpan={7}>
      <div className="panel" style={{ margin: 0 }}>
        <div className="btn-row" style={{ justifyContent: "space-between" }}>
          <strong>Usage</strong>
          <button type="button" className="btn btn-sm btn-ghost" onClick={onClose}>
            Close
          </button>
        </div>
        {error && (
          <div className="alert alert-error" role="alert">
            {error}
          </div>
        )}
        {loading ? (
          <p className="muted">
            <span className="spinner inline-spinner" /> Loading usage…
          </p>
        ) : total === 0 ? (
          <p className="muted">No requests logged for this key yet.</p>
        ) : (
          <>
            <dl className="kv">
              <dt>Requests</dt>
              <dd>
                {fmtNum(total)}
                {data?.success != null && data?.errors != null
                  ? ` (${fmtNum(data.success)} ok / ${fmtNum(data.errors)} errors)`
                  : ""}
              </dd>
              <dt>Prompt tokens</dt>
              <dd>{fmtNum(data?.prompt_tokens ?? 0)}</dd>
              <dt>Completion tokens</dt>
              <dd>{fmtNum(data?.completion_tokens ?? 0)}</dd>
              <dt>Total tokens</dt>
              <dd>{fmtNum(data?.total_tokens ?? 0)}</dd>
            </dl>
            {rows.length > 0 && (
              <div className="table-wrap">
                <table className="data">
                  <thead>
                    <tr>
                      <th>Model</th>
                      <th>Requests</th>
                      <th>Total tokens</th>
                    </tr>
                  </thead>
                  <tbody>
                    {rows.map((m) => (
                      <tr key={m.model ?? "__none__"}>
                        <td className="mono">{m.model ?? "(none)"}</td>
                        <td className="mono">{fmtNum(m.requests)}</td>
                        <td className="mono">{fmtNum(m.total_tokens)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </>
        )}
      </div>
    </td>
  );
}

export function ApiKeysPage() {
  const [keys, setKeys] = useState<ApiKey[]>([]);
  const [models, setModels] = useState<ModelObject[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [createOpen, setCreateOpen] = useState(false);
  const [editing, setEditing] = useState<ApiKey | null>(null);
  const [usageOpenId, setUsageOpenId] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await listApiKeys();
      setKeys(res.keys || []);
    } catch (e) {
      setError(errText(e, "Failed to load API keys"));
      setKeys([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    let cancelled = false;
    listAdminModels()
      .then((res) => {
        if (!cancelled) setModels(res.data || []);
      })
      .catch(() => {
        if (!cancelled) setModels([]);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const catalogModels = useMemo(
    () =>
      models.filter(
        (m) => m.owned_by !== "combo" && !m.id.includes("imagine-image"),
      ),
    [models],
  );

  async function toggleActive(k: ApiKey) {
    setBusyId(k.id);
    setError(null);
    try {
      await patchApiKey(k.id, { is_active: !k.is_active });
      await load();
    } catch (e) {
      setError(errText(e, "Failed to update key"));
    } finally {
      setBusyId(null);
    }
  }

  async function onDelete(k: ApiKey) {
    const label = k.name || k.key_prefix || k.id.slice(0, 8);
    if (
      !window.confirm(`Delete API key "${label}"? Clients using it will get 401.`)
    ) {
      return;
    }
    setBusyId(k.id);
    setError(null);
    try {
      await deleteApiKey(k.id);
      if (usageOpenId === k.id) setUsageOpenId(null);
      await load();
    } catch (e) {
      setError(errText(e, "Failed to delete key"));
    } finally {
      setBusyId(null);
    }
  }

  return (
    <div>
      <header className="page-header">
        <div className="page-header-row">
          <div>
            <h1>API keys</h1>
            <p className="subtitle">
              Scoped pool keys — per-key rate limits, budgets and model
              allowlists
            </p>
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
            <button
              type="button"
              className="btn btn-sm btn-primary"
              onClick={() => setCreateOpen(true)}
            >
              New key
            </button>
          </div>
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

      {loading && !error ? (
        <div className="panel empty">
          <p>
            <span className="spinner inline-spinner" /> Loading API keys…
          </p>
        </div>
      ) : !loading && keys.length === 0 && !error ? (
        <div className="panel empty">
          <p className="flavor">No keys cut from the spool yet.</p>
          <p>No API keys yet — create one to hand out scoped access.</p>
          <div className="btn-row" style={{ justifyContent: "center" }}>
            <button
              type="button"
              className="btn btn-primary"
              onClick={() => setCreateOpen(true)}
            >
              New key
            </button>
          </div>
        </div>
      ) : (
        <div className="table-wrap">
          <table className="data">
            <thead>
              <tr>
                <th>Name</th>
                <th>Key</th>
                <th>Status</th>
                <th>Usage</th>
                <th>Allowlist</th>
                <th>Last used</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {keys.map((k) => (
                <Fragment key={k.id}>
                  <tr className={k.is_active ? "" : "row-inactive"}>
                    <td>{k.name || <span className="muted">Unnamed</span>}</td>
                    <td>
                      <span className="mono">{k.key_prefix ?? "mk-"}…</span>
                    </td>
                    <td>
                      {k.is_active ? (
                        <span className="chip chip-bound" title="Active">
                          <span className="chip-dot" />
                          Active
                        </span>
                      ) : (
                        <span className="chip chip-channeling" title="Disabled">
                          <span className="chip-dot" />
                          Disabled
                        </span>
                      )}
                    </td>
                    <td>
                      <UsageCell keyData={k} />
                    </td>
                    <td>
                      {k.model_allowlist ? (
                        <span
                          className="chip chip-cut"
                          title={k.model_allowlist.join(", ")}
                        >
                          {k.model_allowlist.length} model
                          {k.model_allowlist.length === 1 ? "" : "s"}
                        </span>
                      ) : (
                        <span className="muted">all models</span>
                      )}
                    </td>
                    <td className="muted">
                      {k.last_used_at ? timeAgo(k.last_used_at) : "never"}
                    </td>
                    <td>
                      <span className="actions-cell">
                        <button
                          type="button"
                          className="btn btn-sm"
                          onClick={() =>
                            setUsageOpenId(
                              usageOpenId === k.id ? null : k.id,
                            )
                          }
                          aria-expanded={usageOpenId === k.id}
                        >
                          {usageOpenId === k.id ? "Close" : "Usage"}
                        </button>
                        <button
                          type="button"
                          className="btn btn-sm"
                          onClick={() => setEditing(k)}
                        >
                          Edit
                        </button>
                        <button
                          type="button"
                          className="btn btn-sm"
                          disabled={busyId === k.id}
                          onClick={() => void toggleActive(k)}
                        >
                          {k.is_active ? "Disable" : "Enable"}
                        </button>
                        <button
                          type="button"
                          className="btn btn-sm btn-danger"
                          disabled={busyId === k.id}
                          onClick={() => void onDelete(k)}
                        >
                          Delete
                        </button>
                      </span>
                    </td>
                  </tr>
                  {usageOpenId === k.id && (
                    <tr>
                      <UsageDetail
                        keyId={k.id}
                        onClose={() => setUsageOpenId(null)}
                      />
                    </tr>
                  )}
                </Fragment>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {createOpen && (
        <CreateKeyModal
          models={catalogModels}
          onClose={(reload) => {
            setCreateOpen(false);
            if (reload) void load();
          }}
        />
      )}

      {editing && (
        <EditKeyModal
          keyData={editing}
          models={catalogModels}
          onClose={(reload) => {
            setEditing(null);
            if (reload) void load();
          }}
        />
      )}
    </div>
  );
}
