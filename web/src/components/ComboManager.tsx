import { useCallback, useEffect, useMemo, useState } from "react";
import {
  ApiError,
  createCombo,
  deleteCombo,
  listCombos,
  updateCombo,
  type ModelCombo,
  type ModelObject,
} from "../lib/api";

const MAX_TARGETS = 5;

function errText(e: unknown, fallback: string): string {
  if (e instanceof ApiError) return e.message;
  if (e instanceof Error) return e.message;
  return fallback;
}

export function ComboManager({ models }: { models: ModelObject[] }) {
  const [combos, setCombos] = useState<ModelCombo[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [formOpen, setFormOpen] = useState(false);
  const [slug, setSlug] = useState("");
  const [name, setName] = useState("");
  const [targets, setTargets] = useState<string[]>([]);
  const [saving, setSaving] = useState(false);

  const chatModels = useMemo(
    () =>
      models.filter(
        (m) =>
          m.owned_by !== "combo" &&
          !m.id.includes("imagine-image"),
      ),
    [models],
  );

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await listCombos();
      setCombos(res.combos || []);
    } catch (e) {
      setError(errText(e, "Failed to load combos"));
      setCombos([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (!message) return;
    const t = window.setTimeout(() => setMessage(null), 2500);
    return () => window.clearTimeout(t);
  }, [message]);

  function resetForm() {
    setSlug("");
    setName("");
    setTargets([]);
  }

  function addTarget(model: string) {
    if (!model || targets.includes(model) || targets.length >= MAX_TARGETS) return;
    setTargets((t) => [...t, model]);
  }

  function removeTarget(idx: number) {
    setTargets((t) => t.filter((_, i) => i !== idx));
  }

  function moveTarget(idx: number, dir: -1 | 1) {
    setTargets((t) => {
      const next = [...t];
      const j = idx + dir;
      if (j < 0 || j >= next.length) return next;
      [next[idx], next[j]] = [next[j], next[idx]];
      return next;
    });
  }

  const canSave =
    slug.trim().length >= 2 &&
    name.trim().length > 0 &&
    targets.length >= 1 &&
    targets.length <= MAX_TARGETS &&
    !saving;

  async function onCreate() {
    if (!canSave) return;
    setSaving(true);
    setError(null);
    try {
      await createCombo({ slug: slug.trim(), name: name.trim(), targets });
      setMessage(`Combo combo/${slug.trim()} created`);
      resetForm();
      setFormOpen(false);
      await load();
    } catch (e) {
      setError(errText(e, "Failed to create combo"));
    } finally {
      setSaving(false);
    }
  }

  async function onToggle(combo: ModelCombo) {
    setError(null);
    try {
      await updateCombo(combo.slug, { is_active: !combo.is_active });
      await load();
    } catch (e) {
      setError(errText(e, "Failed to update combo"));
    }
  }

  async function onDelete(combo: ModelCombo) {
    setError(null);
    try {
      await deleteCombo(combo.slug);
      setMessage(`Combo ${combo.id} deleted`);
      await load();
    } catch (e) {
      setError(errText(e, "Failed to delete combo"));
    }
  }

  return (
    <section className="panel combo-panel">
      <div className="page-header-row">
        <div>
          <h2>Combos</h2>
          <p className="subtitle">
            Virtual fallback models — try each target in order until one answers
          </p>
        </div>
        <button
          type="button"
          className="btn btn-sm btn-primary"
          onClick={() => {
            resetForm();
            setFormOpen((v) => !v);
          }}
        >
          {formOpen ? "Cancel" : "+ New combo"}
        </button>
      </div>

      {error && (
        <div className="alert alert-error" role="alert">
          {error}
        </div>
      )}
      {message && (
        <div className="alert alert-ok" role="status">
          {message}
        </div>
      )}

      {formOpen && (
        <div className="form-grid combo-form">
          <label>
            Slug
            <input
              className="input mono"
              placeholder="coding"
              value={slug}
              onChange={(e) => setSlug(e.target.value)}
              aria-label="Combo slug"
            />
          </label>
          <label>
            Name
            <input
              className="input"
              placeholder="Coding"
              value={name}
              onChange={(e) => setName(e.target.value)}
              aria-label="Combo name"
            />
          </label>

          <div className="combo-targets">
            <div className="combo-targets-head">
              <span>Targets (in fallback order, max {MAX_TARGETS})</span>
              <select
                className="input"
                value=""
                onChange={(e) => {
                  addTarget(e.target.value);
                  e.currentTarget.selectedIndex = 0;
                }}
                aria-label="Add target model"
                disabled={targets.length >= MAX_TARGETS}
              >
                <option value="">+ Add target…</option>
                {chatModels
                  .filter((m) => !targets.includes(m.id))
                  .map((m) => (
                    <option key={m.id} value={m.id}>
                      {m.id}
                    </option>
                  ))}
              </select>
            </div>
            {targets.length === 0 ? (
              <p className="muted">No targets yet. Add at least one.</p>
            ) : (
              <ol className="combo-target-list">
                {targets.map((model, idx) => (
                  <li key={model} className="combo-target-row">
                    <span className="mono">
                      {idx + 1}. {model}
                    </span>
                    <span className="btn-row">
                      <button
                        type="button"
                        className="btn btn-sm btn-ghost"
                        onClick={() => moveTarget(idx, -1)}
                        disabled={idx === 0}
                        aria-label={`Move ${model} up`}
                      >
                        ↑
                      </button>
                      <button
                        type="button"
                        className="btn btn-sm btn-ghost"
                        onClick={() => moveTarget(idx, 1)}
                        disabled={idx === targets.length - 1}
                        aria-label={`Move ${model} down`}
                      >
                        ↓
                      </button>
                      <button
                        type="button"
                        className="btn btn-sm btn-ghost"
                        onClick={() => removeTarget(idx)}
                        aria-label={`Remove ${model}`}
                      >
                        ✕
                      </button>
                    </span>
                  </li>
                ))}
              </ol>
            )}
          </div>

          <div className="btn-row" style={{ justifyContent: "flex-end" }}>
            <button
              type="button"
              className="btn btn-sm btn-primary"
              onClick={() => void onCreate()}
              disabled={!canSave}
            >
              {saving ? "Saving…" : "Create combo"}
            </button>
          </div>
        </div>
      )}

      {loading ? (
        <p className="muted">Loading combos…</p>
      ) : combos.length === 0 ? (
        <p className="flavor">No combos yet. Create one to enable fallback routing.</p>
      ) : (
        <div className="table-wrap">
          <table className="data">
            <thead>
              <tr>
                <th>Combo id</th>
                <th>Name</th>
                <th>Targets (fallback order)</th>
                <th>Status</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {combos.map((c) => (
                <tr key={c.id}>
                  <td className="mono">{c.id}</td>
                  <td>{c.name}</td>
                  <td className="mono muted">
                    {c.targets.map((t) => t.model).join(" → ")}
                  </td>
                  <td>
                    {c.is_active ? (
                      <span className="chip chip-bound" title="Active">
                        <span className="chip-dot" />
                        Active
                      </span>
                    ) : (
                      <span className="muted">Inactive</span>
                    )}
                  </td>
                  <td>
                    <span className="btn-row">
                      <button
                        type="button"
                        className="btn btn-sm"
                        onClick={() => void onToggle(c)}
                      >
                        {c.is_active ? "Disable" : "Enable"}
                      </button>
                      <button
                        type="button"
                        className="btn btn-sm btn-danger"
                        onClick={() => void onDelete(c)}
                      >
                        Delete
                      </button>
                    </span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}
