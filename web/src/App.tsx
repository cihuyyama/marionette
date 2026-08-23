import { lazy, Suspense } from "react";
import type { ComponentType } from "react";
import { BrowserRouter, Navigate, Route, Routes } from "react-router-dom";
import { Layout } from "./components/Layout";
import { Overview } from "./pages/Overview";
import { AuthGate } from "./components/AuthGate";

const CHUNK_RELOAD_KEY = "marionette.chunk-reload.v1";

function lazyPage(
  importer: () => Promise<{ default: ComponentType }>,
): ReturnType<typeof lazy> {
  return lazy(() =>
    importer()
      .then((m) => {
        sessionStorage.removeItem(CHUNK_RELOAD_KEY);
        return m;
      })
      .catch((err) => {
        if (!sessionStorage.getItem(CHUNK_RELOAD_KEY)) {
          sessionStorage.setItem(CHUNK_RELOAD_KEY, "1");
          window.location.reload();
        }
        throw err;
      }),
  );
}

const Accounts = lazyPage(() =>
  import("./pages/Accounts").then((m) => ({ default: m.Accounts })),
);
const AccountList = lazyPage(() =>
  import("./pages/AccountList").then((m) => ({ default: m.AccountList })),
);
const ModelsPage = lazyPage(() =>
  import("./pages/Models").then((m) => ({ default: m.ModelsPage })),
);
const CombosPage = lazyPage(() =>
  import("./pages/Combos").then((m) => ({ default: m.CombosPage })),
);
const ApiKeysPage = lazyPage(() =>
  import("./pages/ApiKeys").then((m) => ({ default: m.ApiKeysPage })),
);
const ActivityPage = lazyPage(() =>
  import("./pages/Activity").then((m) => ({ default: m.ActivityPage })),
);
const SetupPage = lazyPage(() =>
  import("./pages/Setup").then((m) => ({ default: m.SetupPage })),
);
const SmokeTest = lazyPage(() =>
  import("./pages/SmokeTest").then((m) => ({ default: m.SmokeTest })),
);
const SettingsPage = lazyPage(() =>
  import("./pages/Settings").then((m) => ({ default: m.SettingsPage })),
);
const AutomationPage = lazyPage(() =>
  import("./pages/Automation").then((m) => ({ default: m.AutomationPage })),
);
const FarmPage = lazyPage(() =>
  import("./pages/Farm").then((m) => ({ default: m.FarmPage })),
);
const InjectJobPage = lazyPage(() =>
  import("./pages/InjectJob").then((m) => ({ default: m.InjectJobPage })),
);
const ProxiesPage = lazyPage(() =>
  import("./pages/Proxies").then((m) => ({ default: m.ProxiesPage })),
);

export default function App() {
  return (
    <BrowserRouter>
      <AuthGate>
        <Suspense fallback={null}>
          <Routes>
            <Route element={<Layout />}>
              <Route index element={<Overview />} />
              <Route path="accounts" element={<Accounts />} />
              <Route
                path="accounts/qoder/inject/:jobId"
                element={<InjectJobPage />}
              />
              <Route
                path="accounts/byok"
                element={<Navigate to="/accounts" replace />}
              />
              <Route path="accounts/byok/:slug" element={<AccountList />} />
              <Route path="accounts/:provider" element={<AccountList />} />
              <Route path="models" element={<ModelsPage />} />
              <Route path="combos" element={<CombosPage />} />
              <Route path="api-keys" element={<ApiKeysPage />} />
              <Route path="activity" element={<ActivityPage />} />
              <Route path="setup" element={<SetupPage />} />
              <Route
                path="import"
                element={<Navigate to="/settings" replace />}
              />
              <Route path="automation" element={<AutomationPage />} />
              <Route
                path="automation/:provider/:method"
                element={<FarmPage />}
              />
              <Route
                path="farm"
                element={<Navigate to="/automation" replace />}
              />
              <Route path="smoke" element={<SmokeTest />} />
              <Route path="proxies" element={<ProxiesPage />} />
              <Route path="settings" element={<SettingsPage />} />
              <Route path="*" element={<Navigate to="/" replace />} />
            </Route>
          </Routes>
        </Suspense>
      </AuthGate>
    </BrowserRouter>
  );
}
