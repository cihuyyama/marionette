import { Fragment, useCallback, useEffect, useMemo, useState } from "react";
import {
  ApiError,
  createCombo,
  deleteCombo,
  listCombos,
  putComboTargets,
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

function TargetPicker({
  chatModels,
  targets,
  onAdd,
  onRemove,
  onMove,
  addLabel,
}: {
  chatModels: ModelObject[];
  targets: string[];
  onAdd: (model: string) => void;
  onRemove: (idx: number) => void;
  onMove: (idx: number, dir: -1 | 1) => void;
  addLabel: string;
}) {
  return (
    <div className="combo-targets">
      <div className="combo-targets-head">
        <span>Targets (in fallback order, max {MAX_TARGETS})</span>
        <select
          className="input"
          value=""
          onChange={(e) => {
            onAdd(e.target.value);
            e.currentTarget.selectedIndex = 0;
          }}
          aria-label={addLabel}
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
                  onClick={() => onMove(idx, -1)}
                  disabled={idx === 0}
                  aria-label={`Move ${model} up`}
                >
                  ↑
                </button>
                <button
                  type="button"
                  className="btn btn-sm btn-ghost"
                  onClick={() => onMove(idx, 1)}
                  disabled={idx === targets.length - 1}
                  aria-label={`Move ${model} down`}
                >
                  ↓
                </button>
                <button
                  type="button"
                  className="btn btn-sm btn-ghost"
                  onClick={() => onRemove(idx)}
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
  );
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
  const [editingSlug, setEditingSlug] = useState<string | null>(null);
  const [editTargets, setEditTargets] = useState<string[]>([]);
  const [editSaving, setEditSaving] = useState(false);

  const chatModels = useMemo(
    () =>
      models.filter(
        (m) =>
          m.owned_by !== "combo" &&
          !m.id.includes("imagine-image"),
      ),
    [models],
  );

  const editingCombo = useMemo(
    () => combos.find((c) => c.slug === editingSlug) ?? null,
    [combos, editingSlug],
  );

  const editDirty = editingCombo
    ? JSON.stringify(editTargets) !==
      JSON.stringify(editingCombo.targets.map((t) => t.model))
    : false;

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
      if (editingSlug === combo.slug) cancelEditing();
      await load();
    } catch (e) {
      setError(errText(e, "Failed to delete combo"));
    }
  }

  function startEditing(combo: ModelCombo) {
    if (editingSlug === combo.slug) {
      cancelEditing();
      return;
    }
    setError(null);
    setEditingSlug(combo.slug);
    setEditTargets(combo.targets.map((t) => t.model));
  }

  function cancelEditing() {
    setEditingSlug(null);
    setEditTargets([]);
  }

  function addEditTarget(model: string) {
    if (
      !model ||
      editTargets.includes(model) ||
      editTargets.length >= MAX_TARGETS
    )
      return;
    setEditTargets((t) => [...t, model]);
  }

  function removeEditTarget(idx: number) {
    setEditTargets((t) => t.filter((_, i) => i !== idx));
  }

  function moveEditTarget(idx: number, dir: -1 | 1) {
    setEditTargets((t) => {
      const next = [...t];
      const j = idx + dir;
      if (j < 0 || j >= next.length) return next;
      [next[idx], next[j]] = [next[j], next[idx]];
      return next;
    });
  }

  const canSaveEdit =
    editingSlug !== null &&
    editTargets.length >= 1 &&
    editTargets.length <= MAX_TARGETS &&
    !editSaving;

  async function onSaveTargets() {
    if (!canSaveEdit || !editingSlug) return;
    setEditSaving(true);
    setError(null);
    try {
      await putComboTargets(editingSlug, editTargets);
      setMessage(`Targets for combo/${editingSlug} updated`);
      cancelEditing();
      await load();
    } catch (e) {
      setError(errText(e, "Failed to update targets"));
    } finally {
      setEditSaving(false);
    }
  }

  return (
    <section className="panel combo-panel">
      <div className="btn-row" style={{ justifyContent: "flex-end" }}>
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

          <TargetPicker
            chatModels={chatModels}
            targets={targets}
            onAdd={addTarget}
            onRemove={removeTarget}
            onMove={moveTarget}
            addLabel="Add target model"
          />

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
                <Fragment key={c.id}>
                  <tr>
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
                          onClick={() => startEditing(c)}
                          aria-expanded={editingSlug === c.slug}
                        >
                          {editingSlug === c.slug ? "Close" : "Edit targets"}
                        </button>
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
                  {editingSlug === c.slug && (
                    <tr>
                      <td colSpan={5}>
                        <TargetPicker
                          chatModels={chatModels}
                          targets={editTargets}
                          onAdd={addEditTarget}
                          onRemove={removeEditTarget}
                          onMove={moveEditTarget}
                          addLabel={`Add target model to ${c.id}`}
                        />
                        <div className="combo-edit-foot">
                          <span className="muted">
                            {editTargets.length}/{MAX_TARGETS} targets · order is
                            fallback priority
                            {editDirty ? " · unsaved changes" : ""}
                          </span>
                          <span className="btn-row">
                            <button
                              type="button"
                              className="btn btn-sm btn-ghost"
                              onClick={cancelEditing}
                            >
                              Cancel
                            </button>
                            <button
                              type="button"
                              className="btn btn-sm btn-primary"
                              onClick={() => void onSaveTargets()}
                              disabled={!canSaveEdit || !editDirty}
                            >
                              {editSaving ? (
                                <>
                                  <span className="spinner inline-spinner" />{" "}
                                  Saving…
                                </>
                              ) : (
                                "Save targets"
                              )}
                            </button>
                          </span>
                        </div>
                      </td>
                    </tr>
                  )}
                </Fragment>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}
