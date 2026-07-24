import { useEffect, useState, type FormEvent } from "react";
import { Link } from "react-router-dom";
import { ApiError, chatCompletion, listModels } from "../lib/api";

export function SmokeTest() {
  const [models, setModels] = useState<string[]>([]);
  const [model, setModel] = useState("gcli/grok-4.5");
  const [message, setMessage] = useState("Reply with exactly: pong");
  const [loading, setLoading] = useState(false);
  const [modelsError, setModelsError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [response, setResponse] = useState<string | null>(null);
  const [raw, setRaw] = useState<unknown>(null);

  useEffect(() => {
    let cancelled = false;
    listModels()
      .then((r) => {
        if (cancelled) return;
        const ids = r.data.map((m) => m.id);
        setModels(ids);
        setModel((current) => (ids.length && !ids.includes(current) ? ids[0] : current));
        setModelsError(null);
      })
      .catch((e) => {
        if (cancelled) return;
        setModelsError(
          e instanceof ApiError
            ? e.message
            : e instanceof Error
              ? e.message
              : "Could not load models",
        );
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    setError(null);
    setResponse(null);
    setRaw(null);
    setLoading(true);
    try {
      const res = await chatCompletion({
        model,
        messages: [{ role: "user", content: message }],
        stream: false,
      });
      setRaw(res);
      setResponse(extractContent(res));
    } catch (err) {
      setError(
        err instanceof ApiError
          ? err.message
          : err instanceof Error
            ? err.message
            : "Request failed",
      );
    } finally {
      setLoading(false);
    }
  }

  return (
    <div>
      <header className="page-header">
        <h1>Smoke test</h1>
        <p className="subtitle">Probe pool chat (non-stream)</p>
      </header>

      {modelsError && (
        <div className="alert alert-info" role="status">
          Models: {modelsError}
          {" — "}
          set pool key in <Link to="/settings">Settings</Link>. You can still type a model id.
        </div>
      )}
      {error && (
        <div className="alert alert-error" role="alert">
          {error}
        </div>
      )}

      <form className="panel" onSubmit={(e) => void onSubmit(e)}>
        <div className="row-fields">
          <div className="field">
            <label htmlFor="smoke-model">Model</label>
            {models.length > 0 ? (
              <select
                id="smoke-model"
                className="select"
                value={model}
                onChange={(e) => setModel(e.target.value)}
              >
                {models.map((id) => (
                  <option key={id} value={id}>
                    {id}
                  </option>
                ))}
              </select>
            ) : (
              <input
                id="smoke-model"
                className="input mono"
                value={model}
                onChange={(e) => setModel(e.target.value)}
                placeholder="gcli/grok-4.5"
              />
            )}
          </div>
          <div className="field">
            <label htmlFor="smoke-msg">Message</label>
            <input
              id="smoke-msg"
              className="input"
              value={message}
              onChange={(e) => setMessage(e.target.value)}
            />
          </div>
        </div>
        <div className="btn-row">
          <button
            type="submit"
            className="btn btn-primary"
            disabled={loading || !model.trim() || !message.trim()}
          >
            {loading ? <span className="spinner inline-spinner" /> : null}
            Send
          </button>
        </div>
      </form>

      {response !== null && (
        <section className="panel" style={{ marginTop: 16 }}>
          <h2
            style={{
              margin: "0 0 12px",
              fontFamily: "var(--font-display)",
              fontWeight: 400,
              fontSize: "1.25rem",
            }}
          >
            Response
          </h2>
          <pre className="response">{response || "(empty content)"}</pre>
          {raw != null && (
            <details style={{ marginTop: 12 }}>
              <summary className="muted" style={{ cursor: "pointer", fontSize: 12 }}>
                Raw JSON
              </summary>
              <pre className="response" style={{ marginTop: 8 }}>
                {JSON.stringify(raw, null, 2)}
              </pre>
            </details>
          )}
        </section>
      )}
    </div>
  );
}

function extractContent(res: Record<string, unknown>): string {
  const choices = res.choices;
  if (Array.isArray(choices) && choices[0] && typeof choices[0] === "object") {
    const c0 = choices[0] as Record<string, unknown>;
    const msg = c0.message;
    if (msg && typeof msg === "object") {
      const content = (msg as Record<string, unknown>).content;
      if (typeof content === "string") return content;
      if (content != null) return JSON.stringify(content, null, 2);
    }
    if (typeof c0.text === "string") return c0.text;
  }
  if (typeof res.content === "string") return res.content;
  return JSON.stringify(res, null, 2);
}
