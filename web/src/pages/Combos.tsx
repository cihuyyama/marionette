import { useCallback, useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { ApiError, listAdminModels, type ModelObject } from "../lib/api";
import { ComboManager } from "../components/ComboManager";

export function CombosPage() {
  const [models, setModels] = useState<ModelObject[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

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

  return (
    <div>
      <header className="page-header">
        <div className="page-header-row">
          <div>
            <h1>Combos</h1>
            <p className="subtitle">
              Virtual fallback models — try each target in order until one
              answers
            </p>
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
            <span className="spinner inline-spinner" /> Loading model catalog…
          </p>
        </div>
      ) : (
        <ComboManager models={models} />
      )}
    </div>
  );
}
