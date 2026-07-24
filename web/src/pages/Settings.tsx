import { useState, type FormEvent } from "react";
import { ApiError, getHealth, getStats } from "../lib/api";
import { loadSettings, saveSettings, type Settings } from "../lib/settings";

export function SettingsPage() {
  const [form, setForm] = useState<Settings>(() => loadSettings());
  const [saved, setSaved] = useState(false);
  const [healthOut, setHealthOut] = useState<string | null>(null);
  const [statsOut, setStatsOut] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [testing, setTesting] = useState<"health" | "stats" | null>(null);

  function onSave(e: FormEvent) {
    e.preventDefault();
    saveSettings(form);
    setSaved(true);
    setError(null);
    window.setTimeout(() => setSaved(false), 2000);
  }

  async function testHealth() {
    setTesting("health");
    setError(null);
    setHealthOut(null);
    try {
      saveSettings(form);
      const h = await getHealth(form);
      setHealthOut(JSON.stringify(h, null, 2));
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e.message
          : e instanceof Error
            ? e.message
            : "Health check failed",
      );
    } finally {
      setTesting(null);
    }
  }

  async function testStats() {
    setTesting("stats");
    setError(null);
    setStatsOut(null);
    try {
      saveSettings(form);
      const s = await getStats(form);
      setStatsOut(JSON.stringify(s, null, 2));
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e.message
          : e instanceof Error
            ? e.message
            : "Stats check failed",
      );
    } finally {
      setTesting(null);
    }
  }

  return (
    <div>
      <header className="page-header">
        <h1>Settings</h1>
        <p className="subtitle">Connection — stored in this browser only</p>
      </header>

      {error && (
        <div className="alert alert-error" role="alert">
          {error}
        </div>
      )}
      {saved && (
        <div className="alert alert-ok" role="status">
          Saved to localStorage.
        </div>
      )}

      <form className="panel" onSubmit={onSave}>
        <div className="field">
          <label htmlFor="base-url">Base URL</label>
          <input
            id="base-url"
            className="input mono"
            value={form.baseUrl}
            onChange={(e) => setForm((f) => ({ ...f, baseUrl: e.target.value }))}
            placeholder="http://127.0.0.1:1940"
            autoComplete="off"
          />
          <span className="hint">
            Default targets the Vite proxy in dev. Use a full URL for remote Marionette.
          </span>
        </div>
        <div className="field">
          <label htmlFor="admin-key">Admin key</label>
          <input
            id="admin-key"
            className="input mono"
            type="password"
            value={form.adminKey}
            onChange={(e) => setForm((f) => ({ ...f, adminKey: e.target.value }))}
            placeholder="MARIONETTE_ADMIN_KEY"
            autoComplete="off"
          />
          <span className="hint">Bearer for /admin/* — never commit this value.</span>
        </div>
        <div className="field">
          <label htmlFor="pool-key">Pool key</label>
          <input
            id="pool-key"
            className="input mono"
            type="password"
            value={form.poolKey}
            onChange={(e) => setForm((f) => ({ ...f, poolKey: e.target.value }))}
            placeholder="MARIONETTE_API_KEY"
            autoComplete="off"
          />
          <span className="hint">Bearer for /v1/* smoke tests.</span>
        </div>
        <div className="btn-row">
          <button type="submit" className="btn btn-primary">
            Save
          </button>
          <button
            type="button"
            className="btn"
            disabled={testing !== null}
            onClick={() => void testHealth()}
          >
            {testing === "health" ? <span className="spinner inline-spinner" /> : null}
            Test health
          </button>
          <button
            type="button"
            className="btn"
            disabled={testing !== null}
            onClick={() => void testStats()}
          >
            {testing === "stats" ? <span className="spinner inline-spinner" /> : null}
            Test stats
          </button>
        </div>
      </form>

      {(healthOut || statsOut) && (
        <section className="panel" style={{ marginTop: 16 }}>
          {healthOut && (
            <>
              <h2
                style={{
                  margin: "0 0 8px",
                  fontFamily: "var(--font-display)",
                  fontWeight: 400,
                  fontSize: "1.15rem",
                }}
              >
                /health
              </h2>
              <pre className="response">{healthOut}</pre>
            </>
          )}
          {statsOut && (
            <>
              <h2
                style={{
                  margin: healthOut ? "16px 0 8px" : "0 0 8px",
                  fontFamily: "var(--font-display)",
                  fontWeight: 400,
                  fontSize: "1.15rem",
                }}
              >
                /admin/stats
              </h2>
              <pre className="response">{statsOut}</pre>
            </>
          )}
        </section>
      )}
    </div>
  );
}
