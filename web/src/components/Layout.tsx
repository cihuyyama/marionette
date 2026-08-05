import { NavLink, Outlet } from "react-router-dom";
import { useEffect, useState } from "react";
import { getHealth } from "../lib/api";
import { loadSettings, clearAdminKey } from "../lib/settings";

const NAV = [
  { to: "/", label: "Overview", end: true },
  { to: "/accounts", label: "Accounts" },
  { to: "/models", label: "Models" },
  { to: "/combos", label: "Combos" },
  { to: "/activity", label: "Activity" },
  { to: "/setup", label: "Setup" },
  { to: "/automation", label: "Automation" },
  { to: "/proxies", label: "Proxies" },
  { to: "/smoke", label: "Smoke test" },
  { to: "/settings", label: "Settings" },
] as const;

export function Layout() {
  const [conn, setConn] = useState<"unknown" | "ok" | "err">("unknown");

  useEffect(() => {
    let cancelled = false;
    const ctrl = new AbortController();
    const tick = () => {
      getHealth(loadSettings(), ctrl.signal)
        .then(() => {
          if (!cancelled) setConn("ok");
        })
        .catch(() => {
          if (!cancelled) setConn("err");
        });
    };
    tick();
    const id = window.setInterval(tick, 30_000);
    return () => {
      cancelled = true;
      ctrl.abort();
      window.clearInterval(id);
    };
  }, []);

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="sidebar-brand">
          <h1>Marionette</h1>
          <p>Proxy pool</p>
          <div className="conn" style={{ marginTop: "var(--space-2)" }}>
            <span
              className={`dot ${conn === "ok" ? "ok" : conn === "err" ? "err" : ""}`}
              aria-hidden
            />
            <span>
              {conn === "ok"
                ? "API reachable"
                : conn === "err"
                  ? "API offline"
                  : "Checking…"}
            </span>
          </div>
        </div>
        <nav className="nav" aria-label="Primary">
          {NAV.map((item) => (
            <NavLink
              key={item.to}
              to={item.to}
              end={"end" in item ? item.end : false}
              className={({ isActive }) => (isActive ? "active" : undefined)}
            >
              <span className="nav-mark" aria-hidden />
              {item.label}
            </NavLink>
          ))}
        </nav>
        <div className="sidebar-foot">
          <button
            type="button"
            className="btn btn-sm"
            onClick={() => {
              clearAdminKey();
              window.dispatchEvent(new Event("marionette-unauthorized"));
            }}
            style={{ width: "100%" }}
          >
            Logout
          </button>
        </div>
      </aside>
      <main className="main">
        <Outlet />
      </main>
    </div>
  );
}
