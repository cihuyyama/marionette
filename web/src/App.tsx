import { lazy, Suspense } from "react";
import { BrowserRouter, Navigate, Route, Routes } from "react-router-dom";
import { Layout } from "./components/Layout";
import { Overview } from "./pages/Overview";
import { AuthGate } from "./components/AuthGate";

const Accounts = lazy(() =>
  import("./pages/Accounts").then((m) => ({ default: m.Accounts })),
);
const AccountList = lazy(() =>
  import("./pages/AccountList").then((m) => ({ default: m.AccountList })),
);
const ModelsPage = lazy(() =>
  import("./pages/Models").then((m) => ({ default: m.ModelsPage })),
);
const ActivityPage = lazy(() =>
  import("./pages/Activity").then((m) => ({ default: m.ActivityPage })),
);
const SetupPage = lazy(() =>
  import("./pages/Setup").then((m) => ({ default: m.SetupPage })),
);
const SmokeTest = lazy(() =>
  import("./pages/SmokeTest").then((m) => ({ default: m.SmokeTest })),
);
const SettingsPage = lazy(() =>
  import("./pages/Settings").then((m) => ({ default: m.SettingsPage })),
);
const AutomationPage = lazy(() =>
  import("./pages/Automation").then((m) => ({ default: m.AutomationPage })),
);
const FarmPage = lazy(() =>
  import("./pages/Farm").then((m) => ({ default: m.FarmPage })),
);
const InjectJobPage = lazy(() =>
  import("./pages/InjectJob").then((m) => ({ default: m.InjectJobPage })),
);
const ProxiesPage = lazy(() =>
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
              <Route path="accounts/:provider" element={<AccountList />} />
              <Route path="models" element={<ModelsPage />} />
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
