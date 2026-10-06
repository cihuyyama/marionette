import { useRef, useState, type FormEvent } from "react";
import { ApiError, importAccountsFile, type ImportResult } from "../lib/api";

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(2)} MB`;
}

function resultSummary(r: ImportResult): string {
  const parts: string[] = [];
  if (r.parsed != null) parts.push(`parsed ${r.parsed}`);
  parts.push(`inserted ${r.inserted}`);
  if (r.updated) parts.push(`updated ${r.updated}`);
  if (r.skipped) parts.push(`skipped ${r.skipped}`);
  return parts.join(", ");
}

export function NineRouterImport() {
  const [file, setFile] = useState<File | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<ImportResult | null>(null);
  const [loading, setLoading] = useState(false);
  const [drag, setDrag] = useState(false);
  const [skipExisting, setSkipExisting] = useState(false);
  const fileRef = useRef<HTMLInputElement>(null);

  function pickFile(f: File) {
    setFile(f);
    setError(null);
    setResult(null);
  }

  function clearAll() {
    setFile(null);
    setResult(null);
    setError(null);
    if (fileRef.current) fileRef.current.value = "";
  }

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    if (!file) return;
    setError(null);
    setResult(null);
    setLoading(true);
    try {
      const res = await importAccountsFile(file, undefined, skipExisting);
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
    <section className="settings-section" aria-labelledby="nine-router-import-heading">
      <div className="settings-section-header">
        <h2 id="nine-router-import-heading">9Router import</h2>
        <p className="subtitle muted">
          Backup JSON → pool (grok-cli + qoder only)
        </p>
      </div>

      {error && (
        <div className="alert alert-error" role="alert">
          {error}
        </div>
      )}
      {result && (
        <div className="alert alert-ok" role="status">
          9Router backup imported — {resultSummary(result)}.
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
            if (f) pickFile(f);
          }}
          role="button"
          tabIndex={0}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") fileRef.current?.click();
          }}
        >
          {file ? (
            <>
              <strong>{file.name}</strong>
              <span
                className="hint"
                style={{ display: "block", marginTop: "0.35rem" }}
              >
                {formatBytes(file.size)} — uploaded as-is; server filters
                supported providers
              </span>
            </>
          ) : (
            "Drop 9Router backup JSON here, or click to choose file"
          )}
          <input
            ref={fileRef}
            type="file"
            accept="application/json,.json"
            hidden
            onChange={(e) => {
              const f = e.target.files?.[0];
              if (f) pickFile(f);
            }}
          />
        </div>

        <p className="hint" style={{ margin: 0 }}>
          Expects a full 9Router backup export. Other providers in the file are
          ignored. For single or bulk tokens per provider, use{" "}
          <strong>+ Add</strong> on the Accounts hub.
        </p>

        <label className="field-inline" style={{ cursor: "pointer", userSelect: "none" }}>
          <input
            type="radio"
            name="import-dedup"
            checked={!skipExisting}
            onChange={() => setSkipExisting(false)}
          />
          <span>
            Replace existing — overwrite accounts with the same provider+email
          </span>
        </label>

        <label className="field-inline" style={{ cursor: "pointer", userSelect: "none" }}>
          <input
            type="radio"
            name="import-dedup"
            checked={skipExisting}
            onChange={() => setSkipExisting(true)}
          />
          <span>
            Skip existing — only import accounts whose provider+email is not
            already in the pool
          </span>
        </label>

        <div className="btn-row">
          <button
            type="submit"
            className="btn btn-primary"
            disabled={loading || !file}
          >
            {loading ? <span className="spinner inline-spinner" /> : null}
            Import backup
          </button>
          <button
            type="button"
            className="btn"
            disabled={!file}
            onClick={clearAll}
          >
            Clear
          </button>
        </div>
      </form>
    </section>
  );
}
