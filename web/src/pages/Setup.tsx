import { useEffect, useMemo, useState } from "react";
import { getConnection } from "../lib/api";
import { applyPoolKeyFromServer, loadSettings } from "../lib/settings";

export function SetupPage() {
  const [settings, setSettings] = useState(() => loadSettings());
  const base = (settings.baseUrl || "http://127.0.0.1:1940").replace(/\/$/, "");
  const poolKey = settings.poolKey || "change-me";
  const [copied, setCopied] = useState<string | null>(null);

  useEffect(() => {
    getConnection()
      .then((conn) => {
        setSettings(applyPoolKeyFromServer(conn.pool_key));
      })
      .catch(() => {
        setSettings(loadSettings());
      });
  }, []);

  const snippets = useMemo(
    () => ({
      curl: `curl ${base}/v1/models \\
  -H "Authorization: Bearer ${poolKey}"`,
      chat: `curl ${base}/v1/chat/completions \\
  -H "Authorization: Bearer ${poolKey}" \\
  -H "Content-Type: application/json" \\
  -d '{
    "model": "gcli/grok-build",
    "messages": [{"role":"user","content":"hi"}],
    "stream": false
  }'`,
      openaiEnv: `OPENAI_BASE_URL=${base}/v1
OPENAI_API_KEY=${poolKey}`,
      opencode: `{
  "provider": {
    "marionette": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "Marionette",
      "options": {
        "baseURL": "${base}/v1",
        "apiKey": "${poolKey}"
      },
      "models": {
        "gcli/grok-build": { "name": "Grok Build" },
        "gcli/grok-4.5": { "name": "Grok 4.5" },
        "qd/lite": { "name": "Qoder Lite" }
      }
    }
  }
}`,
      hermes: `# Hermes / OpenAI-compatible client
base_url: ${base}/v1
api_key: ${poolKey}
# example model ids:
#   gcli/grok-build
#   gcli/grok-4.5
#   qd/lite`,
    }),
    [base, poolKey],
  );

  async function copy(label: string, text: string) {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(label);
      window.setTimeout(() => setCopied(null), 1500);
    } catch {
      /* ignore */
    }
  }

  return (
    <div>
      <header className="page-header">
        <h1>Setup</h1>
        <p className="subtitle">
          Wire OpenCode, Hermes, or any OpenAI-compatible client to Marionette
        </p>
      </header>

      <div className="setup-grid">
        <section className="panel">
          <h2 className="section-title">Connection</h2>
          <dl className="kv">
            <dt>Base URL</dt>
            <dd className="mono">{base}</dd>
            <dt>OpenAI base</dt>
            <dd className="mono">{base}/v1</dd>
            <dt>Pool key</dt>
            <dd className="mono">
              {maskKey(poolKey)}{" "}
              <button
                type="button"
                className="btn btn-sm"
                style={{ marginLeft: 8 }}
                onClick={() => void copy("pool", poolKey)}
              >
                {copied === "pool" ? "Copied" : "Copy key"}
              </button>
            </dd>
            <dt>Admin key</dt>
            <dd className="muted">Dashboard only — not for chat clients</dd>
          </dl>
          <p className="muted" style={{ fontSize: 12, marginTop: 12 }}>
            Values come from browser Settings. Change pool key there if needed.
          </p>
        </section>

        <section className="panel">
          <h2 className="section-title">Endpoints</h2>
          <ul className="setup-list">
            <li>
              <span className="mono">GET /health</span>
              <span className="muted">no auth</span>
            </li>
            <li>
              <span className="mono">GET /v1/models</span>
              <span className="muted">Bearer pool key</span>
            </li>
            <li>
              <span className="mono">POST /v1/chat/completions</span>
              <span className="muted">Bearer pool key</span>
            </li>
          </ul>
        </section>
      </div>

      <Snippet
        title="OpenAI-compatible env"
        text={snippets.openaiEnv}
        copied={copied === "env"}
        onCopy={() => void copy("env", snippets.openaiEnv)}
      />
      <Snippet
        title="OpenCode (openai-compatible provider)"
        text={snippets.opencode}
        copied={copied === "opencode"}
        onCopy={() => void copy("opencode", snippets.opencode)}
      />
      <Snippet
        title="Hermes / generic client"
        text={snippets.hermes}
        copied={copied === "hermes"}
        onCopy={() => void copy("hermes", snippets.hermes)}
      />
      <Snippet
        title="curl · list models"
        text={snippets.curl}
        copied={copied === "curl"}
        onCopy={() => void copy("curl", snippets.curl)}
      />
      <Snippet
        title="curl · chat"
        text={snippets.chat}
        copied={copied === "chat"}
        onCopy={() => void copy("chat", snippets.chat)}
      />
    </div>
  );
}

function Snippet({
  title,
  text,
  copied,
  onCopy,
}: {
  title: string;
  text: string;
  copied: boolean;
  onCopy: () => void;
}) {
  return (
    <section className="panel" style={{ marginTop: "var(--space-4)" }}>
      <div className="page-header-row" style={{ marginBottom: 8 }}>
        <h2 className="section-title" style={{ margin: 0 }}>
          {title}
        </h2>
        <button type="button" className="btn btn-sm" onClick={onCopy}>
          {copied ? "Copied" : "Copy"}
        </button>
      </div>
      <pre className="response setup-pre">{text}</pre>
    </section>
  );
}

function maskKey(key: string): string {
  if (!key) return "—";
  if (key.length <= 8) return "••••";
  return `${key.slice(0, 4)}…${key.slice(-4)}`;
}
