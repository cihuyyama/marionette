import { useEffect, useMemo, useState, type FormEvent } from "react";
import {
  ApiError,
  importAccounts,
  type ImportResult,
} from "../lib/api";
import { labelProvider, type ProviderId } from "../lib/providers";

type Mode = "single" | "bulk" | "pat";

type Props = {
  provider: ProviderId;
  open: boolean;
  onClose: () => void;
  onImported: (result: ImportResult) => void;
};

export function AddAccountModal({
  provider,
  open,
  onClose,
  onImported,
}: Props) {
  const modes = useMemo<Mode[]>(() => {
    if (provider === "qoder") return ["single", "pat", "bulk"];
    return ["single", "bulk"];
  }, [provider]);

  const [mode, setMode] = useState<Mode>("single");
  const [email, setEmail] = useState("");
  const [accessToken, setAccessToken] = useState("");
  const [refreshToken, setRefreshToken] = useState("");
  const [expiresAt, setExpiresAt] = useState("");
  const [clientId, setClientId] = useState("");
  const [personalToken, setPersonalToken] = useState("");
  const [bulkText, setBulkText] = useState("");
  const [replace, setReplace] = useState(false);
  const [skipExisting, setSkipExisting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!open) return;
    setMode(modes[0]);
    setEmail("");
    setAccessToken("");
    setRefreshToken("");
    setExpiresAt("");
    setClientId("");
    setPersonalToken("");
    setBulkText("");
    setReplace(false);
    setSkipExisting(false);
    setError(null);
    setLoading(false);
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

  const canSubmit = (() => {
    if (mode === "bulk" || mode === "pat") return Boolean(bulkText.trim());
    if (provider === "qoder") return Boolean(personalToken.trim());
    return Boolean(accessToken.trim() && refreshToken.trim());
  })();

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    setError(null);
    setLoading(true);
    try {
      const body = buildPayload(provider, mode, {
        email,
        accessToken,
        refreshToken,
        expiresAt,
        clientId,
        personalToken,
        bulkText,
      });
      const res = await importAccounts(body, replace, undefined, skipExisting);
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
    return "Bulk JSON";
  }

  return (
    <>
      <div className="drawer-backdrop" onClick={onClose} />
      <div className="modal" role="dialog" aria-modal="true" aria-label="Add account">
        <div className="modal-head">
          <div>
            <h2>Add {labelProvider(provider)}</h2>
            <p className="muted" style={{ margin: 0 }}>
              Single account or bulk tokens for this provider only
            </p>
          </div>
          <button type="button" className="btn btn-ghost btn-sm" onClick={onClose}>
            Close
          </button>
        </div>

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

        {error && (
          <div className="alert alert-error" role="alert">
            {error}
          </div>
        )}

        <form className="stack-gap" onSubmit={(e) => void onSubmit(e)}>
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

          {replace && (
            <div className="alert alert-error" role="alert">
              Replace all wipes every existing grok-cli and qoder account before
              importing this payload.
            </div>
          )}

          <label className="field-inline" style={{ cursor: "pointer", userSelect: "none" }}>
            <input
              type="checkbox"
              checked={replace}
              onChange={(e) => {
                setReplace(e.target.checked);
                if (e.target.checked) setSkipExisting(false);
              }}
            />
            <span>Replace all — wipe existing grok-cli and qoder accounts first</span>
          </label>

          <label className="field-inline" style={{ cursor: "pointer", userSelect: "none" }}>
            <input
              type="checkbox"
              checked={skipExisting}
              onChange={(e) => {
                setSkipExisting(e.target.checked);
                if (e.target.checked) setReplace(false);
              }}
            />
            <span>Skip existing — ignore rows whose provider+email is already in the pool</span>
          </label>

          <div className="btn-row" style={{ justifyContent: "flex-end" }}>
            <button type="button" className="btn btn-sm" onClick={onClose} disabled={loading}>
              Cancel
            </button>
            <button
              type="submit"
              className={`btn btn-sm ${replace ? "btn-danger" : "btn-primary"}`}
              disabled={loading || !canSubmit}
            >
              {loading ? <span className="spinner inline-spinner" /> : null}
              {replace ? "Replace & import" : mode === "single" ? "Add account" : "Import"}
            </button>
          </div>
        </form>
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

  if (mode === "pat") {
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
          personalToken: token,
        };
      }
      return { provider, personalToken: line };
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
