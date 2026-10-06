import { useEffect, useMemo, useState, type FormEvent } from "react";
import {
  ApiError,
  createByok,
  importAccounts,
  type ByokCreateResult,
  type ImportResult,
} from "../lib/api";
import { labelProvider, type ProviderId } from "../lib/providers";

type Mode = "single" | "bulk" | "pat" | "keys";

const BYOK_SLUG_RE = /^[a-z0-9][a-z0-9_-]{0,31}$/;

type Props = {
  provider: ProviderId;
  open: boolean;
  onClose: () => void;
  onImported: (result: ImportResult) => void;
  onByokCreated?: (result: ByokCreateResult) => void;
  byokSlug?: string;
  byokBaseUrl?: string;
};

export function AddAccountModal({
  provider,
  open,
  onClose,
  onImported,
  onByokCreated,
  byokSlug: fixedSlug,
  byokBaseUrl: fixedBaseUrl,
}: Props) {
  const addKeyMode = provider === "byok" && Boolean(fixedSlug);
  const modes = useMemo<Mode[]>(() => {
    if (provider === "qoder") return ["single", "pat", "bulk"];
    if (provider === "commandcode") return ["single", "keys", "bulk"];
    if (provider === "byok") return ["single"];
    return ["single", "bulk"];
  }, [provider]);

  const [mode, setMode] = useState<Mode>("single");
  const [email, setEmail] = useState("");
  const [accessToken, setAccessToken] = useState("");
  const [refreshToken, setRefreshToken] = useState("");
  const [expiresAt, setExpiresAt] = useState("");
  const [clientId, setClientId] = useState("");
  const [personalToken, setPersonalToken] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [bulkText, setBulkText] = useState("");
  const [skipExisting, setSkipExisting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const [byokSlug, setByokSlug] = useState("");
  const [byokName, setByokName] = useState("");
  const [byokBaseUrl, setByokBaseUrl] = useState("");
  const [byokApiKey, setByokApiKey] = useState("");
  const [byokAutoFetch, setByokAutoFetch] = useState(true);
  const [byokResult, setByokResult] = useState<ByokCreateResult | null>(null);

  useEffect(() => {
    if (!open) return;
    setMode(modes[0]);
    setEmail("");
    setAccessToken("");
    setRefreshToken("");
    setExpiresAt("");
    setClientId("");
    setPersonalToken("");
    setApiKey("");
    setBulkText("");
    setSkipExisting(false);
    setError(null);
    setLoading(false);
    setByokSlug("");
    setByokName("");
    setByokBaseUrl("");
    setByokApiKey("");
    setByokAutoFetch(true);
    setByokResult(null);
  }, [open, modes, provider]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  const slugTrimmed = byokSlug.trim();
  const baseUrlTrimmed = byokBaseUrl.trim();
  const byokApiKeyTrimmed = byokApiKey.trim();

  const slugInvalid =
    slugTrimmed !== "" && !BYOK_SLUG_RE.test(slugTrimmed);
  const baseUrlInvalid =
    baseUrlTrimmed !== "" &&
    !baseUrlTrimmed.startsWith("http://") &&
    !baseUrlTrimmed.startsWith("https://");

  const canSubmit = (() => {
    if (addKeyMode) {
      return byokApiKeyTrimmed !== "";
    }
    if (provider === "byok") {
      return (
        BYOK_SLUG_RE.test(slugTrimmed) &&
        !baseUrlInvalid &&
        baseUrlTrimmed !== "" &&
        byokApiKeyTrimmed !== ""
      );
    }
    if (mode === "bulk" || mode === "pat" || mode === "keys") {
      return Boolean(bulkText.trim());
    }
    if (provider === "qoder") return Boolean(personalToken.trim());
    if (provider === "commandcode") return Boolean(apiKey.trim());
    return Boolean(accessToken.trim() && refreshToken.trim());
  })();

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    setError(null);
    if (provider === "byok" && addKeyMode && fixedSlug) {
      setLoading(true);
      try {
        const res = await createByok({
          slug: fixedSlug,
          api_key: byokApiKeyTrimmed,
        });
        setByokResult(res);
        if (onByokCreated) onByokCreated(res);
        else onImported({ inserted: 1, updated: 0, skipped: 0 });
      } catch (err) {
        setError(
          err instanceof ApiError
            ? err.status === 409
              ? `This key is already added for ${fixedSlug}`
              : err.message
            : err instanceof Error
              ? err.message
              : "Add BYOK key failed",
        );
      } finally {
        setLoading(false);
      }
      return;
    }
    if (provider === "byok") {
      if (slugInvalid) {
        setError(
          "Slug must match ^[a-z0-9][a-z0-9_-]{0,31}$ (lowercase letters, digits, _ or -).",
        );
        return;
      }
      if (baseUrlInvalid) {
        setError("Base URL must start with http:// or https://");
        return;
      }
      setLoading(true);
      try {
        const body: {
          slug: string;
          name?: string;
          base_url: string;
          api_key: string;
          auto_fetch: boolean;
        } = {
          slug: slugTrimmed,
          base_url: baseUrlTrimmed,
          api_key: byokApiKeyTrimmed,
          auto_fetch: byokAutoFetch,
        };
        const nameTrimmed = byokName.trim();
        if (nameTrimmed) body.name = nameTrimmed;
        const res = await createByok(body);
        setByokResult(res);
        if (onByokCreated) onByokCreated(res);
        else onImported({ inserted: 1, updated: 0, skipped: 0 });
      } catch (err) {
        setError(
          err instanceof ApiError
            ? err.status === 409
              ? `slug already exists (${err.message})`
              : err.message
            : err instanceof Error
              ? err.message
              : "Create BYOK account failed",
        );
      } finally {
        setLoading(false);
      }
      return;
    }
    setLoading(true);
    try {
      const body = buildPayload(provider, mode, {
        email,
        accessToken,
        refreshToken,
        expiresAt,
        clientId,
        personalToken,
        apiKey,
        bulkText,
      });
      const res = await importAccounts(body, undefined, skipExisting);
      onImported(res);
      onClose();
    } catch (err) {
      setError(
        err instanceof ApiError
          ? err.message
          : err instanceof Error
            ? err.message
            : "Import failed",
      );
    } finally {
      setLoading(false);
    }
  }

  function modeLabel(m: Mode): string {
    if (m === "single") return "Single";
    if (m === "pat") return "PAT lines";
    if (m === "keys") return "API key lines";
    return "Bulk JSON";
  }

  return (
    <>
      <div className="drawer-backdrop" onClick={onClose} />
      <div className="modal" role="dialog" aria-modal="true" aria-label="Add account">
        <div className="modal-head">
          <div>
            <h2>{addKeyMode ? "Add key" : `Add ${labelProvider(provider)}`}</h2>
            <p className="muted" style={{ margin: 0 }}>
              {addKeyMode
                ? `Add an API key to the ${fixedSlug} endpoint`
                : provider === "byok"
                  ? "One OpenAI-compatible endpoint per account"
                  : "Single account or bulk tokens for this provider only"}
            </p>
          </div>
          <button type="button" className="btn btn-ghost btn-sm" onClick={onClose}>
            Close
          </button>
        </div>

        {modes.length > 1 && (
          <div className="mode-tabs" role="tablist" aria-label="Add mode">
            {modes.map((m) => (
              <button
                key={m}
                type="button"
                role="tab"
                aria-selected={mode === m}
                className={`mode-tab${mode === m ? " active" : ""}`}
                onClick={() => {
                  setMode(m);
                  setError(null);
                }}
              >
                {modeLabel(m)}
              </button>
            ))}
          </div>
        )}

        {error && (
          <div className="alert alert-error" role="alert">
            {error}
          </div>
        )}

        {byokResult ? (
          <div className="stack-gap">
            <div className="alert alert-ok" role="status">
              {byokResult.new_provider
                ? "Endpoint created"
                : addKeyMode
                  ? "Key added"
                  : "Added"}{" "}
              {byokResult.email ?? byokResult.id.slice(0, 8)} — fetched{" "}
              <strong className="mono">{byokResult.models_count}</strong>{" "}
              model{byokResult.models_count === 1 ? "" : "s"}.
            </div>
            {byokResult.models_fetch_error && (
              <div className="alert alert-info" role="status">
                Models not fetched: {byokResult.models_fetch_error}. Use
                “Refresh models” on the account row to retry.
              </div>
            )}
            <div className="btn-row" style={{ justifyContent: "flex-end" }}>
              <button type="button" className="btn btn-sm" onClick={onClose}>
                Done
              </button>
            </div>
          </div>
        ) : (
          <form className="stack-gap" onSubmit={(e) => void onSubmit(e)}>
          {addKeyMode && (
            <>
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="add-byok-key-slug">Endpoint</label>
                <input
                  id="add-byok-key-slug"
                  className="input"
                  value={fixedSlug ?? ""}
                  readOnly
                  spellCheck={false}
                />
                {fixedBaseUrl ? (
                  <span className="hint mono" title={fixedBaseUrl}>
                    {fixedBaseUrl}
                  </span>
                ) : (
                  <span className="hint">Base URL inherited from endpoint</span>
                )}
              </div>
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="add-byok-key-api-key">API key</label>
                <input
                  id="add-byok-key-api-key"
                  className="input"
                  type="password"
                  value={byokApiKey}
                  onChange={(e) => setByokApiKey(e.target.value)}
                  placeholder="sk-…"
                  required
                  autoComplete="new-password"
                  spellCheck={false}
                />
                <span className="hint">
                  Shares this endpoint&apos;s base URL and model catalog
                </span>
              </div>
            </>
          )}
          {provider === "byok" && !addKeyMode && (
            <>
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="add-byok-slug">Slug</label>
                <input
                  id="add-byok-slug"
                  className="input"
                  value={byokSlug}
                  onChange={(e) => setByokSlug(e.target.value)}
                  placeholder="openrouter"
                  required
                  autoComplete="off"
                  spellCheck={false}
                />
                {slugInvalid ? (
                  <span className="hint" style={{ color: "var(--blood)" }}>
                    Must match ^[a-z0-9][a-z0-9_-]{"{0,31}"}$ — lowercase,
                    digits, _ or -, max 32 chars
                  </span>
                ) : (
                  <span className="hint">
                    Lowercase id used in model routing — models appear as{" "}
                    <span className="mono">{slugTrimmed || "openrouter"}/…</span>
                  </span>
                )}
              </div>
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="add-byok-name">Name (optional)</label>
                <input
                  id="add-byok-name"
                  className="input"
                  value={byokName}
                  onChange={(e) => setByokName(e.target.value)}
                  placeholder="OpenRouter"
                  autoComplete="off"
                />
                <span className="hint">Display name shown in lists</span>
              </div>
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="add-byok-base-url">Base URL</label>
                <input
                  id="add-byok-base-url"
                  className="input"
                  value={byokBaseUrl}
                  onChange={(e) => setByokBaseUrl(e.target.value)}
                  placeholder="https://openrouter.ai/api/v1"
                  required
                  autoComplete="off"
                  spellCheck={false}
                />
                {baseUrlInvalid ? (
                  <span className="hint" style={{ color: "var(--blood)" }}>
                    Must start with http:// or https://
                  </span>
                ) : (
                  <span className="hint">
                    OpenAI-compatible endpoint — requests go to{" "}
                    <span className="mono">&lt;base&gt;/chat/completions</span>
                  </span>
                )}
              </div>
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="add-byok-api-key">API key</label>
                <input
                  id="add-byok-api-key"
                  className="input"
                  type="password"
                  value={byokApiKey}
                  onChange={(e) => setByokApiKey(e.target.value)}
                  placeholder="sk-…"
                  required
                  autoComplete="new-password"
                  spellCheck={false}
                />
              </div>
              <label className="field-inline" style={{ cursor: "pointer", userSelect: "none" }}>
                <input
                  type="checkbox"
                  checked={byokAutoFetch}
                  onChange={(e) => setByokAutoFetch(e.target.checked)}
                />
                <span>Fetch model list on add</span>
              </label>
            </>
          )}
          {mode === "single" && provider === "grok-cli" && (
            <>
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="add-email">Email (optional)</label>
                <input
                  id="add-email"
                  className="input"
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                  placeholder="account@example.com"
                  autoComplete="off"
                />
              </div>
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="add-access">Access token</label>
                <textarea
                  id="add-access"
                  className="textarea"
                  rows={3}
                  value={accessToken}
                  onChange={(e) => setAccessToken(e.target.value)}
                  required
                  spellCheck={false}
                />
              </div>
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="add-refresh">Refresh token</label>
                <textarea
                  id="add-refresh"
                  className="textarea"
                  rows={3}
                  value={refreshToken}
                  onChange={(e) => setRefreshToken(e.target.value)}
                  required
                  spellCheck={false}
                />
              </div>
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="add-expires">Expires at (optional ISO)</label>
                <input
                  id="add-expires"
                  className="input"
                  value={expiresAt}
                  onChange={(e) => setExpiresAt(e.target.value)}
                  placeholder="2026-07-26T00:00:00.000Z"
                  autoComplete="off"
                />
              </div>
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="add-client">Client ID (optional)</label>
                <input
                  id="add-client"
                  className="input"
                  value={clientId}
                  onChange={(e) => setClientId(e.target.value)}
                  placeholder="b1a00492-073a-47ea-816f-4c329264a828"
                  autoComplete="off"
                  spellCheck={false}
                />
              </div>
            </>
          )}

          {mode === "single" && provider === "qoder" && (
            <>
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="add-email-q">Email (optional)</label>
                <input
                  id="add-email-q"
                  className="input"
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                  placeholder="account@example.com"
                  autoComplete="off"
                />
              </div>
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="add-pat">Personal token</label>
                <textarea
                  id="add-pat"
                  className="textarea"
                  rows={4}
                  value={personalToken}
                  onChange={(e) => setPersonalToken(e.target.value)}
                  required
                  spellCheck={false}
                  placeholder="qoder_pat_..."
                />
              </div>
            </>
          )}

          {mode === "single" && provider === "commandcode" && (
            <>
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="add-email-c">Email (optional)</label>
                <input
                  id="add-email-c"
                  className="input"
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                  placeholder="account@example.com"
                  autoComplete="off"
                />
              </div>
              <div className="field" style={{ marginBottom: 0 }}>
                <label htmlFor="add-apikey-c">API key</label>
                <input
                  id="add-apikey-c"
                  className="input"
                  value={apiKey}
                  onChange={(e) => setApiKey(e.target.value)}
                  required
                  spellCheck={false}
                  autoComplete="off"
                  placeholder="user_..."
                />
              </div>
            </>
          )}


          {mode === "pat" && (
            <div className="field" style={{ marginBottom: 0 }}>
              <label htmlFor="add-pat-lines">Personal tokens (one per line)</label>
              <textarea
                id="add-pat-lines"
                className="textarea"
                rows={10}
                value={bulkText}
                onChange={(e) => setBulkText(e.target.value)}
                placeholder={"qoder_pat_...\nemail@x.com|qoder_pat_..."}
                required
                spellCheck={false}
              />
              <span className="hint">Line = token, or email|token</span>
            </div>
          )}

          {mode === "keys" && (
            <div className="field" style={{ marginBottom: 0 }}>
              <label htmlFor="add-key-lines">API keys (one per line)</label>
              <textarea
                id="add-key-lines"
                className="textarea"
                rows={10}
                value={bulkText}
                onChange={(e) => setBulkText(e.target.value)}
                placeholder={"sk-...\nemail@x.com|sk-..."}
                required
                spellCheck={false}
              />
              <span className="hint">Line = key, or email|key</span>
            </div>
          )}


          {mode === "bulk" && (
            <div className="field" style={{ marginBottom: 0 }}>
              <label htmlFor="add-bulk-json">JSON array or object</label>
              <textarea
                id="add-bulk-json"
                className="textarea"
                rows={10}
                value={bulkText}
                onChange={(e) => setBulkText(e.target.value)}
                placeholder={
                  provider === "qoder"
                    ? '[{"email":"a@b.com","personalToken":"..."}]'
                    : '[{"email":"a@b.com","accessToken":"...","refreshToken":"..."}]'
                }
                required
                spellCheck={false}
              />
              <span className="hint">
                Provider is set to {provider} automatically. Not for full 9Router backups —
                use Import.
              </span>
            </div>
          )}

          {provider !== "byok" && (
            <>
              <label className="field-inline" style={{ cursor: "pointer", userSelect: "none" }}>
                <input
                  type="radio"
                  name="add-dedup"
                  checked={!skipExisting}
                  onChange={() => setSkipExisting(false)}
                />
                <span>Replace existing — overwrite accounts with the same provider+email</span>
              </label>

              <label className="field-inline" style={{ cursor: "pointer", userSelect: "none" }}>
                <input
                  type="radio"
                  name="add-dedup"
                  checked={skipExisting}
                  onChange={() => setSkipExisting(true)}
                />
                <span>Skip existing — ignore rows whose provider+email is already in the pool</span>
              </label>
            </>
          )}

          <div className="btn-row" style={{ justifyContent: "flex-end" }}>
            <button type="button" className="btn btn-sm" onClick={onClose} disabled={loading}>
              Cancel
            </button>
            <button
              type="submit"
              className="btn btn-sm btn-primary"
              disabled={loading || !canSubmit}
            >
              {loading ? <span className="spinner inline-spinner" /> : null}
              {addKeyMode
                ? "Add key"
                : provider === "byok" || mode === "single"
                  ? "Add account"
                  : "Import"}
            </button>
          </div>
        </form>
        )}
      </div>
    </>
  );
}

type Fields = {
  email: string;
  accessToken: string;
  refreshToken: string;
  expiresAt: string;
  clientId: string;
  personalToken: string;
  apiKey: string;
  bulkText: string;
};

function buildPayload(provider: ProviderId, mode: Mode, f: Fields): unknown {
  if (mode === "single") {
    if (provider === "qoder") {
      const row: Record<string, string> = {
        provider,
        personalToken: f.personalToken.trim(),
      };
      if (f.email.trim()) row.email = f.email.trim();
      return row;
    }
    if (provider === "commandcode") {
      const row: Record<string, string> = {
        provider,
        apiKey: f.apiKey.trim(),
      };
      if (f.email.trim()) row.email = f.email.trim();
      return row;
    }
    const row: Record<string, string> = {
      provider,
      accessToken: f.accessToken.trim(),
      refreshToken: f.refreshToken.trim(),
    };
    if (f.email.trim()) row.email = f.email.trim();
    if (f.expiresAt.trim()) row.expiresAt = f.expiresAt.trim();
    if (f.clientId.trim()) row.clientId = f.clientId.trim();
    return row;
  }

  if (mode === "pat" || mode === "keys") {
    const secretKey = mode === "pat" ? "personalToken" : "apiKey";
    const lines = f.bulkText
      .split(/\r?\n/)
      .map((l) => l.trim())
      .filter(Boolean);
    if (lines.length === 0) throw new Error("No tokens");
    return lines.map((line) => {
      if (line.includes("|")) {
        const [email, token] = line.split("|").map((s) => s.trim());
        if (!token) throw new Error(`Bad line: ${line}`);
        return {
          provider,
          email: email || undefined,
          [secretKey]: token,
        };
      }
      return { provider, [secretKey]: line };
    });
  }

  const text = f.bulkText.trim();
  if (!text) throw new Error("Empty payload");
  let parsed: unknown;
  try {
    parsed = JSON.parse(text) as unknown;
  } catch {
    throw new Error("Invalid JSON");
  }
  return stampProvider(provider, parsed);
}

function stampProvider(provider: ProviderId, body: unknown): unknown {
  if (Array.isArray(body)) {
    return body.map((item) =>
      item && typeof item === "object"
        ? { provider, ...(item as Record<string, unknown>) }
        : item,
    );
  }
  if (body && typeof body === "object") {
    const obj = body as Record<string, unknown>;
    if (Array.isArray(obj.accounts)) {
      return {
        ...obj,
        accounts: obj.accounts.map((item) =>
          item && typeof item === "object"
            ? { provider, ...(item as Record<string, unknown>) }
            : item,
        ),
      };
    }
    return { provider, ...obj };
  }
  return body;
}
