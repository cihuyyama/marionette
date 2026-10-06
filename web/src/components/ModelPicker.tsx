import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent } from "react";
import type { ModelObject } from "../lib/api";

const MAX_VISIBLE = 60;

/** Best display label: human name when it adds something, else the id. */
function label(m: ModelObject): string {
  const name = m.display_name?.trim();
  if (!name) return m.id;
  return name;
}

const compact = (s: string) => s.toLowerCase().replace(/[\s/_.:-]+/g, "");

function matches(m: ModelObject, q: string): boolean {
  if (!q) return true;
  const fields = [m.id, m.display_name ?? "", m.owned_by ?? "", m.model_key ?? ""];
  if (fields.some((f) => f.toLowerCase().includes(q))) return true;
  // "grok4.6" should find "gcli/grok-4.6": compare with separators stripped.
  const qc = compact(q);
  if (!qc) return false;
  return fields.some((f) => compact(f).includes(qc));
}

/**
 * Filterable model combobox.
 *
 * The pool exposes 400+ model ids (dominated by BYOK endpoints), so a native
 * <select> is unusable. Free text is still accepted: an id that is not in the
 * list can be typed and submitted, which keeps the smoke test useful when the
 * models endpoint is unreachable.
 */
export function ModelPicker({
  value,
  onChange,
  models,
  id,
}: {
  value: string;
  onChange: (next: string) => void;
  models: ModelObject[];
  id?: string;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState<string | null>(null);
  const [active, setActive] = useState(0);

  const rootRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLUListElement>(null);
  // commit() refocuses the input; without this the onFocus handler would
  // immediately reopen the menu we just closed.
  const suppressFocusOpen = useRef(false);
  const fallbackId = useId();
  const listId = `${id ?? fallbackId}-list`;

  const filtered = useMemo(() => {
    const q = (query ?? "").trim().toLowerCase();
    if (!q) return models;
    const qc = compact(q);
    // Rank: id prefix > compact hit > substring elsewhere. Ties keep backend
    // order, which lists first-party providers ahead of BYOK endpoints.
    const scored: { m: ModelObject; rank: number }[] = [];
    for (const m of models) {
      if (!matches(m, q)) continue;
      const mid = m.id.toLowerCase();
      let rank = 2;
      if (mid.startsWith(q)) rank = 0;
      else if (compact(m.id).includes(qc)) rank = 1;
      scored.push({ m, rank });
    }
    scored.sort((a, b) => a.rank - b.rank);
    return scored.map((s) => s.m);
  }, [models, query]);

  const visible = filtered.slice(0, MAX_VISIBLE);
  const hidden = filtered.length - visible.length;

  // Close on outside click.
  useEffect(() => {
    if (!open) return;
    function onDocDown(e: MouseEvent) {
      if (!rootRef.current?.contains(e.target as Node)) {
        setOpen(false);
        setQuery(null);
      }
    }
    document.addEventListener("mousedown", onDocDown);
    return () => document.removeEventListener("mousedown", onDocDown);
  }, [open]);

  // Keep the highlighted row in view.
  useEffect(() => {
    if (!open) return;
    const el = listRef.current?.children[active] as HTMLElement | undefined;
    el?.scrollIntoView({ block: "nearest" });
  }, [active, open]);

  function commit(m: ModelObject) {
    onChange(m.id);
    setOpen(false);
    setQuery(null);
    const el = inputRef.current;
    // Only arm the guard when focus() will actually fire a focus event;
    // otherwise the flag leaks and swallows the next genuine focus.
    if (el && document.activeElement !== el) {
      suppressFocusOpen.current = true;
      el.focus();
    }
  }

  function onKeyDown(e: KeyboardEvent<HTMLInputElement>) {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (!open) {
        setOpen(true);
        setActive(0);
        return;
      }
      const dir = e.key === "ArrowDown" ? 1 : -1;
      setActive((i) => {
        if (visible.length === 0) return 0;
        return (i + dir + visible.length) % visible.length;
      });
      return;
    }
    if (e.key === "Enter") {
      if (open && visible[active]) {
        e.preventDefault();
        commit(visible[active]);
      }
      return;
    }
    if (e.key === "Escape") {
      if (open) {
        e.preventDefault();
        setOpen(false);
        setQuery(null);
      }
      return;
    }
    if (e.key === "Tab" && open) {
      setOpen(false);
      setQuery(null);
    }
  }

  const shown = query ?? value;

  return (
    <div className="model-picker" ref={rootRef}>
      <input
        id={id}
        ref={inputRef}
        className="input mono"
        role="combobox"
        aria-expanded={open}
        aria-controls={listId}
        aria-autocomplete="list"
        aria-activedescendant={open && visible[active] ? `${listId}-${active}` : undefined}
        autoComplete="off"
        spellCheck={false}
        placeholder="gcli/grok-4.6"
        value={shown}
        onFocus={() => {
          if (suppressFocusOpen.current) {
            suppressFocusOpen.current = false;
            return;
          }
          setOpen(true);
          setActive(0);
        }}
        onClick={() => {
          // Already focused (e.g. right after picking) fires no focus event.
          if (!open) {
            setOpen(true);
            setActive(0);
          }
        }}
        onChange={(e) => {
          setQuery(e.target.value);
          onChange(e.target.value);
          setOpen(true);
          setActive(0);
        }}
        onKeyDown={onKeyDown}
      />

      {open && (
        <div className="model-picker-menu">
          <ul className="model-picker-list" id={listId} role="listbox" ref={listRef}>
            {visible.map((m, i) => (
              <li
                key={m.id}
                id={`${listId}-${i}`}
                role="option"
                aria-selected={m.id === value}
                className={`model-picker-option${i === active ? " active" : ""}${
                  m.id === value ? " selected" : ""
                }`}
                onMouseEnter={() => setActive(i)}
                onMouseDown={(e) => {
                  // Keep focus on the input; commit before the blur closes us.
                  e.preventDefault();
                  commit(m);
                }}
              >
                <span className="model-picker-id mono">{m.id}</span>
                {label(m) !== m.id && (
                  <span className="model-picker-name truncate">{label(m)}</span>
                )}
                <span className="model-picker-owner">{m.owned_by}</span>
              </li>
            ))}
            {visible.length === 0 && (
              <li className="model-picker-empty">
                No match. <span className="mono">{shown.trim() || "…"}</span> will be sent as typed.
              </li>
            )}
          </ul>
          <div className="model-picker-foot">
            {filtered.length === 0
              ? "0 models"
              : hidden > 0
                ? `${visible.length} of ${filtered.length} · type to narrow`
                : `${filtered.length} of ${models.length}`}
          </div>
        </div>
      )}
    </div>
  );
}
