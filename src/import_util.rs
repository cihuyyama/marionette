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
///     { "id": "...", "provider": "blackbox", ...,
///       "apiKey": "sk-...", "password": "..." }
///   ]
/// }
/// ```
///
/// We support "grok-cli", "qoder", and "blackbox". Other providers are silently skipped.
use crate::db::{self, Account};
use serde_json::{Value, json};
use uuid::Uuid;

pub const SUPPORTED_PROVIDERS: &[&str] = &["grok-cli", "qoder", "blackbox"];

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
fn build_data(item: &Value, provider: &str) -> Result<Value, String> {
    match provider {
        "grok-cli" => build_grok_data(item),
        "qoder" => build_qoder_data(item),
        "blackbox" => build_blackbox_data(item),
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

/// blackbox data: a static API key — no OAuth, no refresh. Requires `apiKey`
/// (accepts the `api_key` alias); optional `password` is kept so the account
/// can be re-registered upstream if the key is ever rotated.
fn build_blackbox_data(item: &Value) -> Result<Value, String> {
    let api_key = item
        .get("apiKey")
        .or_else(|| item.get("api_key"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or("blackbox: missing apiKey")?;

    let mut out = serde_json::Map::new();
    out.insert("apiKey".into(), json!(api_key));
    copy_str(item, &mut out, "password");

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

    fn blackbox_item() -> Value {
        json!({
            "id": "cccc-3333",
            "provider": "blackbox",
            "email": "bb@example.com",
            "name": "bb worker",
            "isActive": true,
            "priority": 2,
            "apiKey": "sk-blackbox-secret",
            "password": "signup-password"
        })
    }

    #[test]
    fn parse_grok_account() {
        let acc = map_connection(&grok_item()).unwrap();
        assert_eq!(acc.provider, "grok-cli");
        assert_eq!(acc.email.as_deref(), Some("test@example.com"));
        assert_eq!(acc.is_active, 1);
        assert_eq!(acc.priority, 5);
        let data: Value = serde_json::from_str(&acc.data).unwrap();
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
        let data: Value = serde_json::from_str(&acc.data).unwrap();
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
                blackbox_item(),
                json!({ "provider": "openai", "id": "x", "accessToken": "y" })
            ]
        });
        let accounts = parse_9router_backup(&backup);
        assert_eq!(accounts.len(), 3);
        assert!(accounts.iter().all(|a| SUPPORTED_PROVIDERS.contains(&a.provider.as_str())));
        assert!(accounts.iter().any(|a| a.provider == "blackbox"));
    }

    #[test]
    fn parse_blackbox_account() {
        let acc = map_connection(&blackbox_item()).unwrap();
        assert_eq!(acc.provider, "blackbox");
        assert_eq!(acc.email.as_deref(), Some("bb@example.com"));
        assert_eq!(acc.is_active, 1);
        assert_eq!(acc.priority, 2);
        assert_eq!(acc.quota_limit, 0);
        assert_eq!(acc.quota_remaining, 0);
        let data: Value = serde_json::from_str(&acc.data).unwrap();
        assert_eq!(data["apiKey"], "sk-blackbox-secret");
        assert_eq!(data["password"], "signup-password");
        assert!(data.get("email").is_none());
    }

    #[test]
    fn blackbox_api_key_alias_accepted() {
        let mut item = blackbox_item();
        item.as_object_mut().unwrap().remove("apiKey");
        item["api_key"] = json!("sk-alias-secret");
        let acc = map_connection(&item).unwrap();
        let data: Value = serde_json::from_str(&acc.data).unwrap();
        assert_eq!(data["apiKey"], "sk-alias-secret");
    }

    #[test]
    fn blackbox_missing_api_key_error() {
        let mut item = blackbox_item();
        item.as_object_mut().unwrap().remove("apiKey");
        let err = map_connection(&item).unwrap_err();
        assert_eq!(err, "blackbox: missing apiKey");

        let mut blank = blackbox_item();
        blank["apiKey"] = json!("   ");
        assert_eq!(map_connection(&blank).unwrap_err(), "blackbox: missing apiKey");
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
        let data: Value = serde_json::from_str(&acc.data).unwrap();
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
        let data: Value = serde_json::from_str(&acc.data).unwrap();
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
        let data: Value = serde_json::from_str(&acc.data).unwrap();
        assert!(
            data.get("expireTime").is_none(),
            "must NOT invent expireTime when absent"
        );
    }
}
