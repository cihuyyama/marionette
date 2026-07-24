import { useRef, useState, type FormEvent } from "react";
import { ApiError, importAccounts, type ImportResult } from "../lib/api";

const PLACEHOLDER = `[
  {
    "provider": "grok-cli",
    "email": "account@example.com",
    "accessToken": "...",
    "refreshToken": "...",
    "expiresAt": "2026-07-26T00:00:00.000Z",
    "clientId": "b1a00492-073a-47ea-816f-4c329264a828"
  }
]`;

export function ImportPage() {
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<ImportResult | null>(null);
  const [loading, setLoading] = useState(false);
  const [drag, setDrag] = useState(false);
  const fileRef = useRef<HTMLInputElement>(null);

  function applyFile(file: File) {
    const reader = new FileReader();
    reader.onload = () => {
      setText(String(reader.result ?? ""));
      setError(null);
      setResult(null);
    };
    reader.onerror = () => setError("Failed to read file");
    reader.readAsText(file);
  }

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    setError(null);
    setResult(null);
    let body: unknown;
    try {
      body = JSON.parse(text);
    } catch {
      setError("Invalid JSON — paste an array, {accounts:[]}, or a single account object.");
      return;
    }
    setLoading(true);
    try {
      const res = await importAccounts(body);
      setResult(res);
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
    <div>
      <header className="page-header">
        <h1>Import</h1>
        <p className="subtitle">Bind accounts into the pool</p>
      </header>

      {error && (
        <div className="alert alert-error" role="alert">
          {error}
        </div>
      )}
      {result && (
        <div className="alert alert-ok" role="status">
          Imported — inserted {result.inserted}, updated {result.updated}, skipped{" "}
          {result.skipped}.
        </div>
      )}

      <form className="panel stack-gap" onSubmit={(e) => void onSubmit(e)}>
        <div
          className={`file-drop ${drag ? "drag" : ""}`}
          onClick={() => fileRef.current?.click()}
          onDragOver={(e) => {
            e.preventDefault();
            setDrag(true);
          }}
          onDragLeave={() => setDrag(false)}
          onDrop={(e) => {
            e.preventDefault();
            setDrag(false);
            const f = e.dataTransfer.files[0];
            if (f) applyFile(f);
          }}
          role="button"
          tabIndex={0}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") fileRef.current?.click();
          }}
        >
          Drop JSON file here, or click to upload
          <input
            ref={fileRef}
            type="file"
            accept="application/json,.json"
            hidden
            onChange={(e) => {
              const f = e.target.files?.[0];
              if (f) applyFile(f);
            }}
          />
        </div>

        <div className="field" style={{ marginBottom: 0 }}>
          <label htmlFor="import-json">JSON payload</label>
          <textarea
            id="import-json"
            className="textarea"
            value={text}
            onChange={(e) => setText(e.target.value)}
            placeholder={PLACEHOLDER}
            spellCheck={false}
          />
          <span className="hint">
            Accepts array, {"{ accounts: [] }"}, or a single account. Tokens stay on the server;
            responses are masked.
          </span>
        </div>

        <div className="btn-row">
          <button
            type="submit"
            className="btn btn-primary"
            disabled={loading || !text.trim()}
          >
            {loading ? <span className="spinner inline-spinner" /> : null}
            Import accounts
          </button>
          <button
            type="button"
            className="btn"
            disabled={!text}
            onClick={() => {
              setText("");
              setResult(null);
              setError(null);
            }}
          >
            Clear
          </button>
        </div>
      </form>
    </div>
  );
}
