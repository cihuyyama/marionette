import { useState, useEffect, type FormEvent } from "react";
import { getStats, getConnection, ApiError } from "../lib/api";
import {
  loadSettings,
  saveSettings,
  applyPoolKeyFromServer,
} from "../lib/settings";

async function syncPoolKeyFromEnv(settings = loadSettings()) {
  try {
    const conn = await getConnection(settings);
    applyPoolKeyFromServer(conn.pool_key);
  } catch {
    /* non-fatal: smoke/setup can still use local pool key */
  }
}

export function AuthGate({ children }: { children: React.ReactNode }) {
  const [authorized, setAuthorized] = useState<boolean | null>(null);
  const [keyInput, setKeyInput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);

  useEffect(() => {
    const s = loadSettings();
    getStats(s)
      .then(async () => {
        await syncPoolKeyFromEnv(s);
        setAuthorized(true);
      })
      .catch((err) => {
        if (err instanceof ApiError && err.status === 401) {
          setAuthorized(false);
        } else {
          setAuthorized(true);
        }
      });
  }, []);

  useEffect(() => {
    const handler = () => setAuthorized(false);
    window.addEventListener("marionette-unauthorized", handler);
    return () => window.removeEventListener("marionette-unauthorized", handler);
  }, []);

  async function handleLogin(e: FormEvent) {
    e.preventDefault();
    setChecking(true);
    setError(null);
    const s = loadSettings();
    const nextSettings = { ...s, adminKey: keyInput.trim() };
    try {
      await getStats(nextSettings);
      saveSettings(nextSettings);
      await syncPoolKeyFromEnv(nextSettings);
      setAuthorized(true);
    } catch (err) {
      setError(
        err instanceof ApiError
          ? err.message
          : err instanceof Error
            ? err.message
            : "Invalid key",
      );
    } finally {
      setChecking(false);
    }
  }

  if (authorized === null) {
    return null;
  }

  if (!authorized) {
    return (
      <div className="login-screen">
        <form className="login-form panel" onSubmit={handleLogin}>
          <h2
            style={{
              margin: "0 0 var(--space-2)",
              fontFamily: "var(--font-display)",
              fontSize: "1.75rem",
              fontWeight: 400,
            }}
          >
            Marionette Admin
          </h2>
          <p className="muted" style={{ margin: "0 0 var(--space-2)" }}>
            Enter MARIONETTE_ADMIN_KEY to connect.
          </p>
          {error && <div className="alert alert-error">{error}</div>}
          <input
            type="password"
            value={keyInput}
            onChange={(e) => setKeyInput(e.target.value)}
            placeholder="change-me-admin"
            className="input mono"
            autoFocus
            required
          />
          <button
            type="submit"
            className="btn btn-primary"
            disabled={checking}
            style={{ marginTop: "var(--space-2)" }}
          >
            {checking ? "Checking..." : "Unlock"}
          </button>
        </form>
      </div>
    );
  }

  return <>{children}</>;
}
