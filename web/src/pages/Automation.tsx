import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import { ApiError, getFarmStatus, type FarmStatus } from "../lib/api";
import {
  AUTOMATION_PROVIDERS,
  farmLivePath,
  type AutomationMethod,
  type AutomationProvider,
} from "../lib/automation";

function availabilityLabel(status: "ready" | "coming_soon"): string {
  return status === "ready" ? "Ready" : "Coming soon";
}

function availabilityChip(status: "ready" | "coming_soon"): string {
  return status === "ready" ? "chip-bound" : "chip-cut";
}

export function AutomationPage() {
  const navigate = useNavigate();
  const [picker, setPicker] = useState<AutomationProvider | null>(null);
  const [farm, setFarm] = useState<FarmStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refreshFarm = useCallback(async () => {
    try {
      const s = await getFarmStatus();
      setFarm(s);
      setError(null);
    } catch (e) {
      // Hub still usable without farm status (e.g. old binary).
      setError(
        e instanceof ApiError
          ? e.message
          : e instanceof Error
            ? e.message
            : "Farm status unavailable",
      );
    }
  }, []);

  useEffect(() => {
    void refreshFarm();
    const id = window.setInterval(() => void refreshFarm(), 8000);
    return () => window.clearInterval(id);
  }, [refreshFarm]);

  useEffect(() => {
    if (!picker) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setPicker(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [picker]);

  function openProvider(provider: AutomationProvider) {
    if (provider.status !== "ready" && provider.methods.every((m) => m.status !== "ready")) {
      setPicker(provider);
      return;
    }
    setPicker(provider);
  }

  function chooseMethod(provider: AutomationProvider, method: AutomationMethod) {
    if (method.status !== "ready" || provider.status !== "ready") return;
    setPicker(null);
    navigate(`/automation/${provider.id}/${method.id}`);
  }

  const busy = farm?.busy ?? false;
  const current = farm?.current;

  return (
    <div>
      <header className="page-header">
        <div className="page-header-row">
          <div>
            <h1>Automation</h1>
            <p className="subtitle">
              Thread new marionettes · pick a provider, then an auth path
            </p>
          </div>
          <div className="btn-row">
            <button
              type="button"
              className="btn btn-sm"
              onClick={() => void refreshFarm()}
            >
              Refresh
            </button>
          </div>
        </div>
      </header>

      {error && (
        <div className="alert alert-error" role="alert">
          {error}
        </div>
      )}

      {busy && current && (
        <div className="alert alert-ok" role="status">
          Job running:{" "}
          <button
            type="button"
            className="btn btn-ghost btn-sm"
            style={{ verticalAlign: "baseline", padding: "0 4px" }}
            onClick={() => navigate(farmLivePath(current.provider))}
          >
            open live log →
          </button>
          <span className="mono muted" style={{ marginLeft: 8 }}>
            {current.id.slice(0, 8)}… · {current.provider ?? "qoder"} ·{" "}
            {current.status} · ok {current.ok}/{current.total}
          </span>
        </div>
      )}

      <div className="provider-grid">
        {AUTOMATION_PROVIDERS.map((provider) => {
          const readyMethods = provider.methods.filter((m) => m.status === "ready")
            .length;
          const isLive =
            busy &&
            current?.status === "running" &&
            (current.provider ?? "qoder") === provider.id;
          return (
            <div key={provider.id} className="provider-card provider-card-static">
              <button
                type="button"
                className="provider-card-main"
                onClick={() => openProvider(provider)}
              >
                <div className="provider-card-head">
                  <h2>{provider.label}</h2>
                  <span
                    className={`chip ${
                      isLive
                        ? "chip-channeling"
                        : availabilityChip(provider.status)
                    }`}
                  >
                    <span className="chip-dot" aria-hidden />
                    {isLive
                      ? "Running"
                      : availabilityLabel(provider.status)}
                  </span>
                </div>
                <p className="muted" style={{ margin: "0 0 var(--space-3)", fontSize: 13 }}>
                  {provider.blurb}
                </p>
                <div className="provider-card-meta">
                  <span className="muted">
                    Methods:{" "}
                    <strong>
                      {readyMethods}/{provider.methods.length} ready
                    </strong>
                  </span>
                  <span className="provider-card-cta">
                    {provider.status === "ready" ? "Choose path →" : "Preview →"}
                  </span>
                </div>
              </button>
            </div>
          );
        })}
      </div>

      {picker && (
        <MethodPickerModal
          provider={picker}
          onClose={() => setPicker(null)}
          onChoose={(method) => chooseMethod(picker, method)}
        />
      )}
    </div>
  );
}

function MethodPickerModal({
  provider,
  onClose,
  onChoose,
}: {
  provider: AutomationProvider;
  onClose: () => void;
  onChoose: (method: AutomationMethod) => void;
}) {
  return (
    <>
      <div className="drawer-backdrop" onClick={onClose} />
      <div
        className="modal modal-wide"
        role="dialog"
        aria-modal="true"
        aria-label={`${provider.label} automation path`}
      >
        <div className="modal-head">
          <div>
            <h2>{provider.label}</h2>
            <p className="muted" style={{ margin: 0 }}>
              Choose how accounts are created or verified
            </p>
          </div>
          <button type="button" className="btn btn-ghost btn-sm" onClick={onClose}>
            Close
          </button>
        </div>

        <div className="method-grid">
          {provider.methods.map((method) => {
            const ready =
              provider.status === "ready" && method.status === "ready";
            return (
              <button
                key={method.id}
                type="button"
                className={`method-card${ready ? "" : " method-card-disabled"}`}
                disabled={!ready}
                onClick={() => onChoose(method)}
                title={ready ? `Open ${method.label} farm` : "Not available yet"}
              >
                <div className="method-card-head">
                  <span className="method-card-title">{method.label}</span>
                  <span className={`chip ${availabilityChip(method.status)}`}>
                    <span className="chip-dot" aria-hidden />
                    {availabilityLabel(method.status)}
                  </span>
                </div>
                <p className="method-card-desc muted">{method.description}</p>
                <span className="provider-card-cta">
                  {ready ? "Open farm →" : "Coming soon"}
                </span>
              </button>
            );
          })}
        </div>
      </div>
    </>
  );
}
