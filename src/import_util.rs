/// Shared utilities for importing accounts from 9Router backup JSON.
///
/// 9Router backup shape:
/// ```json
/// {
///   "providerConnections": [
///     { "id": "...", "provider": "grok-cli", "email": "...", "name": "...",
///       "isActive": true, "priority": 1, "createdAt": "...", "updatedAt": "...",
///       "accessToken": "...", "refreshToken": "...", "expiresAt": "...",
///       "idToken": "...", "clientId": "...", ... },
///     { "id": "...", "provider": "qoder", ...,
///       "providerSpecificData": { "personalToken": "...", "machineId": "...", ... } },
///     { "id": "...", "provider": "grok-cli", ...,
///       "apiKey": "sk-...", "password": "..." }
///   ]
/// }
/// ```
///
/// We support "grok-cli", "qoder", and "commandcode". Other providers
/// are silently skipped.
use crate::db::{self, Account};
use serde_json::{Value, json};
use uuid::Uuid;

pub const SUPPORTED_PROVIDERS: &[&str] = &["grok-cli", "qoder", "commandcode", "cline", "antigravity", "kiro"];

/// Parse a 9Router full-backup JSON value and return accounts
/// for supported providers only.
///
/// Accepts:
/// - `{ "providerConnections": [...] }` — 9Router backup dump
/// - `[...]` — bare array of connection objects
pub fn parse_9router_backup(v: &Value) -> Vec<Account> {
    let items = if let Some(arr) = v
        .get("providerConnections")
        .and_then(|x| x.as_array())
    {
        arr.as_slice()
    } else if let Some(arr) = v.as_array() {
        arr.as_slice()
    } else {
        return vec![];
    };

    items
        .iter()
        .filter_map(|item| map_connection(item).ok())
        .collect()
}

/// Map a single providerConnection object → Account.
/// Returns Err (silently skipped by caller) if provider is unsupported
/// or required tokens are missing.
fn map_connection(item: &Value) -> Result<Account, String> {
    let provider = item
        .get("provider")
        .and_then(|v| v.as_str())
        .ok_or("missing provider")?;

    if !SUPPORTED_PROVIDERS.contains(&provider) {
        return Err(format!("unsupported provider: {provider}"));
    }

    let id = item
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let email = item.get("email").and_then(|v| v.as_str()).map(String::from);
    let name = item
        .get("name")
        .or_else(|| item.get("displayName"))
        .and_then(|v| v.as_str())
        .map(String::from);

    // isActive: backup stores as bool
    let is_active = item
        .get("isActive")
        .and_then(|v| v.as_bool())
        .map(|b| if b { 1i64 } else { 0 })
        .unwrap_or(1);

    let priority = item
        .get("priority")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let data = build_data(item, provider)?;

    let (q_lim, q_rem) = db::default_quota_for_provider(provider);
    let now = db::now_rfc3339();

    // Prefer backup timestamps so we don't reset created_at
    let created_at = item
        .get("createdAt")
        .and_then(|v| v.as_str())
        .unwrap_or(&now)
        .to_string();
    let updated_at = item
        .get("updatedAt")
        .and_then(|v| v.as_str())
        .unwrap_or(&now)
        .to_string();

    Ok(Account {
        id,
        provider: provider.to_string(),
        email,
        name,
        is_active,
        priority,
        data: data.to_string(),
        cooldown_until: None,
        last_error: None,
        last_used_at: item
            .get("lastUsedAt")
            .and_then(|v| v.as_str())
            .map(String::from),
        created_at,
        updated_at,
        quota_limit: q_lim,
        quota_remaining: q_rem,
    })
}

/// Build the `data` JSON blob for a connection, normalised to Marionette's
/// expected shape for each provider.
pub fn build_data(item: &Value, provider: &str) -> Result<Value, String> {
    match provider {
        "grok-cli" => build_grok_data(item),
        "qoder" => build_qoder_data(item),
        "commandcode" => build_commandcode_data(item),
        "cline" => build_cline_data(item),
        "antigravity" => build_antigravity_data(item),
        "kiro" => build_kiro_data(item),
        _ => Err(format!("unsupported: {provider}")),
    }
}

/// grok-cli data: OAuth token fields directly on the connection object.
fn build_grok_data(item: &Value) -> Result<Value, String> {
    let access_token = item
        .get("accessToken")
        .and_then(|v| v.as_str())
        .ok_or("grok-cli: missing accessToken")?;

    let mut out = serde_json::Map::new();
    out.insert("accessToken".into(), json!(access_token));

    copy_str(item, &mut out, "refreshToken");
    copy_str(item, &mut out, "idToken");
    copy_str(item, &mut out, "clientId");
    copy_str(item, &mut out, "expiresAt");
    if let Some(v) = item.get("expiresIn").and_then(|v| v.as_i64()) {
        out.insert("expiresIn".into(), json!(v));
    }
    copy_str(item, &mut out, "scope");
    // Backoff / error state — reset on import (fresh start)
    out.insert("backoffLevel".into(), json!(0));

    Ok(Value::Object(out))
}

/// qoder data: tokens split between top-level and `providerSpecificData`.
/// QoderTokens::from_data() reads both via `effective_data()` which merges them,
/// so we store everything flat (no nested providerSpecificData).
fn build_qoder_data(item: &Value) -> Result<Value, String> {
    // Collect top-level fields
    let top = item.as_object().ok_or("qoder: not an object")?;

    // providerSpecificData has the critical tokens (personalToken, machineId, etc.)
    let psd = item
        .get("providerSpecificData")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let mut out = serde_json::Map::new();

    // Merge: psd wins for token fields (it's the authoritative source)
    let token_keys = [
        "personalToken",
        "securityOauthToken",
        "machineToken",
        "machineId",
        "machineType",
        "userId",
        "organizationId",
        "plan",
        "authMethod",
    ];
    for k in &token_keys {
        if let Some(v) = psd.get(*k).or_else(|| top.get(*k)) {
            if !v.is_null() {
                out.insert(k.to_string(), v.clone());
            }
        }
    }

    // Top-level OAuth fields
    copy_str(item, &mut out, "accessToken");
    copy_str(item, &mut out, "refreshToken");
    copy_str(item, &mut out, "expiresAt");
    if let Some(v) = item.get("expiresIn").and_then(|v| v.as_i64()) {
        out.insert("expiresIn".into(), json!(v));
    }
    // displayName → userName (QoderTokens uses displayName alias)
    if let Some(dn) = item.get("displayName").and_then(|v| v.as_str()) {
        out.entry("userName".to_string()).or_insert_with(|| json!(dn));
    }

    // Copy expireTime (numeric millis) — providerSpecificData preferred, top-level fallback.
    // Stale/past expireTime is intentional: forces a lazy jobToken refresh on first use.
    if let Some(v) = psd
        .get("expireTime")
        .and_then(|v| v.as_i64())
        .or_else(|| item.get("expireTime").and_then(|v| v.as_i64()))
    {
        out.insert("expireTime".into(), json!(v));
    }

    // Validate: personalToken is required for Qoder
    if !out.contains_key("personalToken") {
        return Err("qoder: missing personalToken in providerSpecificData".into());
    }

    Ok(Value::Object(out))
}

/// commandcode data: a static bearer API key (`user_…`) for the
/// api.commandcode.ai/alpha/generate NDJSON gateway — no OAuth, no refresh.
/// cline data. Accepts either a full OAuth pair or a bare refresh token.
///
/// A refresh token alone is enough: `/auth/refresh` is a plain POST, so a
/// pasted RT can bootstrap without the WorkOS device screen.
/// antigravity data: a Google refresh token. `/token` needs no browser state,
/// so a pasted RT bootstraps the account.
/// kiro data. Paste-only: the four import families all validate upstream
/// before the row is persisted, so a dead credential cannot be stored.
fn build_kiro_data(item: &Value) -> Result<Value, String> {
    let mut out = serde_json::Map::new();

    let method = item
        .get("authMethod")
        .or_else(|| item.get("auth_method"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty());

    let has_api_key = item
        .get("apiKey")
        .or_else(|| item.get("api_key"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .is_some();

    // Infer api_key when a key is present and no method was stated.
    let method = match (method, has_api_key) {
        (Some(m), _) => m,
        (None, true) => "api_key".to_string(),
        (None, false) => "imported".to_string(),
    };

    match method.as_str() {
        "api_key" => {
            let key = item
                .get("apiKey")
                .or_else(|| item.get("api_key"))
                .and_then(|v| v.as_str())
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .ok_or("kiro: missing apiKey")?;
            out.insert("authMethod".into(), json!("api_key"));
            out.insert("accessToken".into(), json!(key));
            // Static: no refresh, no expiry.
            out.insert("expiresAt".into(), json!(null));
        }
        "external_idp" => {
            for field in ["accessToken", "refreshToken", "clientId", "tokenEndpoint"] {
                let v = item
                    .get(field)
                    .and_then(|x| x.as_str())
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| format!("kiro: external_idp missing {field}"))?;
                out.insert(field.into(), json!(v));
            }
            // This posts the account's refresh token to whatever the JSON
            // names, so the host must be one we accept.
            let endpoint = out
                .get("tokenEndpoint")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !crate::providers::kiro::is_allowed_idp_endpoint(endpoint) {
                return Err("kiro: tokenEndpoint is not an allowed Microsoft host".into());
            }
            out.insert("authMethod".into(), json!("external_idp"));
            if let Some(s) = item.get("scope").and_then(|v| v.as_str()) {
                out.insert("scope".into(), json!(s));
            }
        }
        "idc" => {
            for field in ["refreshToken", "clientId", "clientSecret"] {
                let v = item
                    .get(field)
                    .and_then(|x| x.as_str())
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| format!("kiro: idc missing {field}"))?;
                out.insert(field.into(), json!(v));
            }
            out.insert("authMethod".into(), json!("idc"));
        }
        _ => {
            let rt = item
                .get("refreshToken")
                .or_else(|| item.get("refresh_token"))
                .and_then(|v| v.as_str())
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .ok_or("kiro: missing refreshToken")?;
            out.insert("refreshToken".into(), json!(rt));
            out.insert("authMethod".into(), json!("imported"));
        }
    }

    if let Some(at) = item
        .get("accessToken")
        .or_else(|| item.get("access_token"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        out.insert("accessToken".into(), json!(at));
    }
    if let Some(r) = item.get("region").and_then(|v| v.as_str()) {
        out.insert("region".into(), json!(r));
    }
    copy_str(item, &mut out, "profileArn");
    copy_str(item, &mut out, "machineId");
    Ok(Value::Object(out))
}

fn build_antigravity_data(item: &Value) -> Result<Value, String> {
    let mut out = serde_json::Map::new();
    if let Some(rt) = item
        .get("refreshToken")
        .or_else(|| item.get("refresh_token"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        out.insert("refreshToken".into(), json!(rt));
    }
    if let Some(at) = item
        .get("accessToken")
        .or_else(|| item.get("access_token"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        out.insert("accessToken".into(), json!(at));
    }
    if !out.contains_key("refreshToken") && !out.contains_key("accessToken") {
        return Err("antigravity: missing refreshToken (or accessToken)".into());
    }
    // The access token arrives on first refresh; the project is provisioned
    // lazily by loadCodeAssist.
    if !out.contains_key("accessToken") {
        out.insert("accessToken".into(), json!(""));
    }
    copy_str(item, &mut out, "expiresAt");
    copy_str(item, &mut out, "projectId");
    Ok(Value::Object(out))
}

fn build_cline_data(item: &Value) -> Result<Value, String> {
    let mut out = serde_json::Map::new();

    if let Some(rt) = item
        .get("refreshToken")
        .or_else(|| item.get("refresh_token"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        out.insert("refreshToken".into(), json!(rt));
    }

    if let Some(at) = item
        .get("accessToken")
        .or_else(|| item.get("access_token"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        out.insert("accessToken".into(), json!(at));
        out.insert("credentialKind".into(), json!("oauth"));
    }

    if out.is_empty() {
        return Err("cline: missing refreshToken (or accessToken)".into());
    }
    // A bare refresh token is the import-only path; the access token arrives
    // on first refresh.
    if !out.contains_key("accessToken") {
        out.insert("accessToken".into(), json!(""));
        out.insert("credentialKind".into(), json!("oauth"));
    }
    copy_str(item, &mut out, "expiresAt");
    Ok(Value::Object(out))
}

fn build_commandcode_data(item: &Value) -> Result<Value, String> {
    let api_key = item
        .get("apiKey")
        .or_else(|| item.get("api_key"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or("commandcode: missing apiKey")?;

    let mut out = serde_json::Map::new();
    out.insert("apiKey".into(), json!(api_key));

    Ok(Value::Object(out))
}

fn copy_str(src: &Value, dst: &mut serde_json::Map<String, Value>, key: &str) {
    if let Some(s) = src.get(key).and_then(|v| v.as_str()) {
        if !s.is_empty() {
            dst.insert(key.to_string(), json!(s));
        }
    }
}

/// Auto-detect whether a JSON value looks like a 9Router full backup.
/// Returns true if it has a `providerConnections` array.
pub fn is_9router_backup(v: &Value) -> bool {
    v.get("providerConnections")
        .and_then(|x| x.as_array())
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn grok_item() -> Value {
        json!({
            "id": "aaaa-1111",
            "provider": "grok-cli",
            "email": "test@example.com",
            "name": "Test Grok",
            "isActive": true,
            "priority": 5,
            "accessToken": "at_grok",
            "refreshToken": "rt_grok",
            "idToken": "it_grok",
            "clientId": "b1a00492-073a-47ea-816f-4c329264a828",
            "expiresAt": "2026-08-01T00:00:00Z",
            "expiresIn": 21600,
            "scope": "openid",
            "createdAt": "2026-07-01T00:00:00Z",
            "updatedAt": "2026-07-25T00:00:00Z"
        })
    }

    fn qoder_item() -> Value {
        json!({
            "id": "bbbb-2222",
            "provider": "qoder",
            "email": "qoder@example.com",
            "name": "qoder@example.com",
            "isActive": true,
            "priority": 1,
            "accessToken": "at_qoder",
            "refreshToken": "rt_qoder",
            "expiresAt": "2026-08-01T00:00:00Z",
            "providerSpecificData": {
                "personalToken": "pt_secret",
                "machineId": "m-uuid",
                "machineToken": "mt_secret",
                "machineType": "5",
                "userId": "u-uuid",
                "organizationId": "",
                "plan": "PLAN_TIER_FREE",
                "authMethod": "device",
                "securityOauthToken": "sot_secret"
            }
        })
    }

    #[test]
    fn parse_grok_account() {
        let acc = map_connection(&grok_item()).unwrap();
        assert_eq!(acc.provider, "grok-cli");
        assert_eq!(acc.email.as_deref(), Some("test@example.com"));
        assert_eq!(acc.is_active, 1);
        assert_eq!(acc.priority, 5);
        let data: Value = acc.data_json();
        assert_eq!(data["accessToken"], "at_grok");
        assert_eq!(data["backoffLevel"], 0);
        // meta fields must NOT be in data
        assert!(data.get("provider").is_none());
        assert!(data.get("email").is_none());
    }

    #[test]
    fn parse_qoder_account() {
        let acc = map_connection(&qoder_item()).unwrap();
        assert_eq!(acc.provider, "qoder");
        let data: Value = acc.data_json();
        assert_eq!(data["personalToken"], "pt_secret");
        assert_eq!(data["machineId"], "m-uuid");
        assert_eq!(data["accessToken"], "at_qoder");
        assert_eq!(data["securityOauthToken"], "sot_secret");
        assert!(data.get("providerSpecificData").is_none()); // must be flat
    }

    #[test]
    fn skip_unsupported_provider() {
        let item = json!({ "provider": "openai", "accessToken": "x" });
        assert!(map_connection(&item).is_err());
    }

    #[test]
    fn parse_backup_filters_providers() {
        let backup = json!({
            "providerConnections": [
                grok_item(),
                qoder_item(),
                json!({ "provider": "openai", "id": "x", "accessToken": "y" })
            ]
        });
        let accounts = parse_9router_backup(&backup);
        assert_eq!(accounts.len(), 2);
        assert!(accounts.iter().all(|a| SUPPORTED_PROVIDERS.contains(&a.provider.as_str())));
    }

    #[test]
    fn is_backup_detection() {
        let backup = json!({ "providerConnections": [] });
        assert!(is_9router_backup(&backup));
        let not_backup = json!([{ "provider": "grok-cli" }]);
        assert!(!is_9router_backup(&not_backup));
    }

    #[test]
    fn qoder_missing_personal_token_skipped() {
        let mut item = qoder_item();
        item["providerSpecificData"].as_object_mut().unwrap().remove("personalToken");
        let result = map_connection(&item);
        assert!(result.is_err());
    }

    #[test]
    fn qoder_expiretime_from_psd() {
        let mut item = qoder_item();
        item["providerSpecificData"]["expireTime"] = json!(1893456000000i64);
        let acc = map_connection(&item).unwrap();
        let data: Value = acc.data_json();
        assert_eq!(
            data["expireTime"],
            json!(1893456000000i64),
            "expireTime must be copied from providerSpecificData"
        );
    }

    #[test]
    fn qoder_expiretime_from_toplevel() {
        let mut item = qoder_item();
        // No expireTime in providerSpecificData (qoder_item() doesn't have one).
        // Place it at the top level of the connection object.
        item["expireTime"] = json!(1893456000000i64);
        let acc = map_connection(&item).unwrap();
        let data: Value = acc.data_json();
        assert_eq!(
            data["expireTime"],
            json!(1893456000000i64),
            "expireTime must fall back to top-level connection field"
        );
    }

    #[test]
    fn qoder_expiretime_absent_ok() {
        // qoder_item() has no expireTime anywhere.
        let acc = map_connection(&qoder_item()).unwrap();
        let data: Value = acc.data_json();
        assert!(
            data.get("expireTime").is_none(),
            "must NOT invent expireTime when absent"
        );
    }
}
