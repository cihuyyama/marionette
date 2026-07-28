import { BrowserRouter, Navigate, Route, Routes } from "react-router-dom";
import { Layout } from "./components/Layout";
import { Overview } from "./pages/Overview";
import { Accounts } from "./pages/Accounts";
import { AccountList } from "./pages/AccountList";
import { ModelsPage } from "./pages/Models";
import { ActivityPage } from "./pages/Activity";
import { SetupPage } from "./pages/Setup";
import { SmokeTest } from "./pages/SmokeTest";
import { SettingsPage } from "./pages/Settings";
import { AutomationPage } from "./pages/Automation";
import { FarmPage } from "./pages/Farm";
import { InjectJobPage } from "./pages/InjectJob";
import { AuthGate } from "./components/AuthGate";

export default function App() {
  return (
    <BrowserRouter>
      <AuthGate>
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
            <Route path="farm" element={<Navigate to="/automation" replace />} />
            <Route path="smoke" element={<SmokeTest />} />
            <Route path="settings" element={<SettingsPage />} />
            <Route path="*" element={<Navigate to="/" replace />} />
          </Route>
        </Routes>
      </AuthGate>
    </BrowserRouter>
  );
}
