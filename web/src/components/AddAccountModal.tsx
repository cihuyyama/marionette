import { useEffect, useMemo, useState, type FormEvent } from "react";
import {
  ApiError,
  importAccounts,
  type ImportResult,
} from "../lib/api";
import { labelProvider, type ProviderId } from "../lib/providers";

type Mode = "pat" | "json";

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
    if (provider === "qoder") return ["pat", "json"];
    return ["json"];
  }, [provider]);

  const [mode, setMode] = useState<Mode>("json");
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!open) return;
    setMode(modes[0]);
    setText("");
    setError(null);
    setLoading(false);
  }, [open, modes]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    setError(null);
    setLoading(true);
    try {
      const body = buildPayload(provider, mode, text);
      const res = await importAccounts(body);
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

  return (
    <>
      <div className="drawer-backdrop" onClick={onClose} />
      <div className="modal" role="dialog" aria-modal="true" aria-label="Add account">
        <div className="modal-head">
          <div>
            <h2>Add {labelProvider(provider)}</h2>
            <p className="muted" style={{ margin: 0 }}>
              Bind tokens into the pool
            </p>
          </div>
          <button type="button" className="btn btn-ghost btn-sm" onClick={onClose}>
            Close
          </button>
        </div>

        {modes.length > 1 && (
          <div className="mode-tabs" role="tablist" aria-label="Import mode">
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
                {m === "pat" ? "PAT" : "JSON"}
              </button>
            ))}
          </div>
        )}

        {error && (
          <div className="alert alert-error" role="alert">
            {error}
          </div>
        )}

        <form className="stack-gap" onSubmit={(e) => void onSubmit(e)}>
          <div className="field" style={{ marginBottom: 0 }}>
            <label htmlFor="add-account-payload">
              {mode === "pat"
                ? "Personal tokens (one per line)"
                : "JSON (array, object, or {accounts:[]})"}
            </label>
            <textarea
              id="add-account-payload"
              className="textarea"
              rows={10}
              value={text}
              onChange={(e) => setText(e.target.value)}
              placeholder={
                mode === "pat"
                  ? "qoder_pat_...\n(or email|personalToken per line)"
                  : provider === "qoder"
                    ? '[{"provider":"qoder","email":"a@b.com","personalToken":"..."}]'
                    : '[{"provider":"grok-cli","email":"a@b.com","accessToken":"...","refreshToken":"..."}]'
              }
              required
            />
          </div>
          <div className="btn-row" style={{ justifyContent: "flex-end" }}>
            <button type="button" className="btn btn-sm" onClick={onClose} disabled={loading}>
              Cancel
            </button>
            <button type="submit" className="btn btn-sm btn-primary" disabled={loading || !text.trim()}>
              {loading ? <span className="spinner inline-spinner" /> : null}
              Import
            </button>
          </div>
        </form>
      </div>
    </>
  );
}

function buildPayload(provider: ProviderId, mode: Mode, raw: string): unknown {
  const text = raw.trim();
  if (!text) throw new Error("Empty payload");

  if (mode === "json") {
    try {
      const parsed = JSON.parse(text) as unknown;
      return stampProvider(provider, parsed);
    } catch {
      throw new Error("Invalid JSON");
    }
  }

  const lines = text
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
    return {
      provider,
      personalToken: line,
    };
  });
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
