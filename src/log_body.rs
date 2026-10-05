use serde_json::Value;

const DEFAULT_MAX_BYTES: usize = 262_144;

pub fn log_body_max_bytes() -> usize {
    std::env::var("MARIONETTE_LOG_BODY_MAX_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_BYTES)
        .max(1024)
}

/// Marker written in place of a credential-shaped value. Tagged rather than a
/// bare `***` so an operator reading the log can tell *why* the value is gone.
const REDACTED: &str = "***REDACTED***[credential]";

/// Substrings that mark a JSON key as holding auth material. Compared against
/// the lowercased key.
const SECRET_KEY_PARTS: &[&str] = &[
    "apikey",
    "api_key",
    "accesstoken",
    "access_token",
    "refreshtoken",
    "refresh_token",
    "password",
    "secret",
    "credential",
    "privatekey",
    "private_key",
    "clientsecret",
    "client_secret",
    "sessiontoken",
    "session_token",
    "bearer",
    "authorization",
];

fn is_secret_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    // Exact match on the header-ish names...
    if lower == "authorization" || lower == "x-api-key" || lower == "api-key" {
        return true;
    }
    // Suffix rule: `*Token` is how most providers name the bearer material
    // (personalToken, machineToken, securityOauthToken, refresh_token). The
    // plural is excluded so usage counters (`total_tokens`, `free_tokens`) —
    // which are the whole point of keeping the log — are not wiped.
    if lower.ends_with("token") && !lower.ends_with("tokens") {
        return true;
    }
    SECRET_KEY_PARTS.iter().any(|p| lower.contains(p))
}

/// True when `s` *as a whole* is credential-shaped. Applied both to whole
/// values and to delimiter-split slices of a longer string.
fn is_credential_token(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 8 {
        return false;
    }
    if s.starts_with("sk-") || s.starts_with("sk_") || s.starts_with("rk_") {
        return s.len() >= 12;
    }
    if let Some(rest) = s
        .strip_prefix("Bearer ")
        .or_else(|| s.strip_prefix("bearer "))
    {
        return rest.trim().len() >= 8;
    }
    if let Some(rest) = s
        .strip_prefix("OAuth ")
        .or_else(|| s.strip_prefix("oauth "))
    {
        return rest.trim().len() >= 8;
    }
    // JWT: three base64url segments joined by dots.
    let mut parts = s.split('.');
    if let (Some(a), Some(b), Some(c)) = (parts.next(), parts.next(), parts.next()) {
        if parts.next().is_none()
            && a.len() >= 8
            && b.len() >= 8
            && c.len() >= 4
            && a.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
            && b.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        {
            return true;
        }
    }
    false
}

/// True for strings that *look like* a credential — including one embedded in
/// ordinary prose.
///
/// The whole-string shape test alone misses `"use sk-abc123... ok"`, where the
/// secret is only part of the value. So the value is scanned slice by slice on
/// non-token characters and each slice faces the same strict shape rules. The
/// whole string is replaced rather than splicing the token out: surrounding
/// text often quotes the same secret, and a partial redaction is worse than
/// none because it looks clean.
fn looks_like_credential(s: &str) -> bool {
    if is_credential_token(s) {
        return true;
    }
    // Cheap gate: without a candidate prefix there is no embedded credential.
    if !(s.contains("sk-")
        || s.contains("sk_")
        || s.contains("rk_")
        || s.contains("Bearer ")
        || s.contains("bearer ")
        || s.contains("OAuth ")
        || s.contains("oauth ")
        || s.contains("eyJ"))
    {
        return false;
    }
    s.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'))
        .any(is_credential_token)
}

/// Recursively replace secret-bearing values. Returns a new `Value`.
///
/// Runs before serialization so redaction happens *before* truncation: a
/// truncated preview is what an operator actually reads, and a secret that
/// survives only in the preview is still leaked.
fn redact_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| {
                // A key that names a secret is redacted by name, even when the
                // value is numeric/null — the contract is "this field never
                // reaches the log", not "this field is shaped like a secret".
                if is_secret_key(k) {
                    (k.clone(), Value::String(REDACTED.into()))
                } else {
                    (k.clone(), redact_value(v))
                }
            })
            .collect(),
        Value::Array(items) => items.iter().map(redact_value).collect(),
        Value::String(s) => {
            if looks_like_credential(s) {
                Value::String(REDACTED.into())
            } else {
                Value::String(s.clone())
            }
        }
        other => other.clone(),
    }
}

pub fn log_body_enabled() -> bool {
    match std::env::var("MARIONETTE_LOG_BODY_ENABLED") {
        Ok(s) => {
            let t = s.trim().to_ascii_lowercase();
            !(t == "0" || t == "false" || t == "no" || t == "off")
        }
        Err(_) => true,
    }
}

pub fn prepare_log_body(value: &Value) -> Option<String> {
    if !log_body_enabled() {
        return None;
    }
    let max = log_body_max_bytes();
    // Redact before serializing so the truncation preview (what an operator
    // actually reads) is already clean.
    let value = redact_value(value);
    let serialized = match serde_json::to_string(&value) {
        Ok(s) => s,
        Err(e) => {
            return Some(
                serde_json::json!({
                    "unserializable": true,
                    "reason": e.to_string(),
                })
                .to_string(),
            );
        }
    };
    let bytes = serialized.len();
    if bytes <= max {
        return Some(serialized);
    }
    let preview = truncate_utf8(&serialized, max.saturating_sub(128));
    Some(
        serde_json::json!({
            "truncated": true,
            "original_bytes": bytes,
            "max_bytes": max,
            "preview": preview,
        })
        .to_string(),
    )
}

pub fn prepare_log_body_from_serializable<T: serde::Serialize>(value: &T) -> Option<String> {
    match serde_json::to_value(value) {
        Ok(v) => prepare_log_body(&v),
        Err(e) => Some(
            serde_json::json!({
                "unserializable": true,
                "reason": e.to_string(),
            })
            .to_string(),
        ),
    }
}

fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if max_bytes == 0 {
        return String::new();
    }
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prepare_small_body_roundtrips() {
        let v = json!({"model": "qd/ultimate", "messages": [{"role": "user", "content": "hi"}]});
        let s = prepare_log_body(&v).expect("enabled by default");
        let parsed: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed["model"], "qd/ultimate");
        assert!(parsed.get("truncated").is_none());
    }

    #[test]
    fn prepare_truncates_large_body() {
        let big = "x".repeat(400_000);
        let v = json!({"content": big});
        let s = prepare_log_body(&v).unwrap();
        let parsed: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed["truncated"], true);
        assert!(parsed["original_bytes"].as_u64().unwrap() > 100_000);
        assert!(parsed["preview"].as_str().unwrap().len() < 400_000);
    }

    #[test]
    fn redacts_secret_keys_by_name() {
        let v = json!({
            "apiKey": "user_abc123def456",
            "refreshToken": "rt_zzzzzzzzzzzz",
            "Authorization": "Bearer whatever",
            "model": "qd/ultimate"
        });
        let s = prepare_log_body(&v).unwrap();
        let parsed: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed["apiKey"], REDACTED);
        assert_eq!(parsed["refreshToken"], REDACTED);
        assert_eq!(parsed["Authorization"], REDACTED);
        assert_eq!(parsed["model"], "qd/ultimate", "non-secret keys untouched");
    }

    #[test]
    fn redacts_credential_shaped_strings_in_message_content() {
        // A secret pasted into ordinary content has no revealing key name, so
        // the value shape has to catch it.
        let v = json!({"messages": [{"role": "user", "content": "use sk-abcdefgh12345678 ok"}]});
        let s = prepare_log_body(&v).unwrap();
        let parsed: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed["messages"][0]["content"], REDACTED);
    }

    #[test]
    fn redacts_jwt_shaped_strings() {
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1g";
        assert!(looks_like_credential(jwt));
        let v = json!({"messages": [{"role": "user", "content": jwt}]});
        let s = prepare_log_body(&v).unwrap();
        assert!(!s.contains(jwt));
    }

    #[test]
    fn does_not_redact_short_or_ordinary_strings() {
        assert!(!looks_like_credential("hi"));
        assert!(!looks_like_credential("Bearer x"));
        assert!(!looks_like_credential("Please review the following code"));
        // A dotted string that is not a JWT must survive (e.g. "v1.2.3").
        assert!(!looks_like_credential("version 1.2.3"));
    }

    #[test]
    fn redaction_survives_truncation() {
        // The secret sits past the truncation point in the serialized form.
        // Redaction runs first, so it must not survive in the preview.
        let mut messages = Vec::new();
        for _ in 0..2000 {
            messages.push(json!({"role": "user", "content": "y".repeat(200)}));
        }
        messages.push(json!({"role": "user", "content": "key is sk-abcdefgh12345678"}));
        let v = json!({"messages": messages});
        let s = prepare_log_body(&v).unwrap();
        assert!(
            !s.contains("sk-abcdefgh12345678"),
            "secret must not leak via preview"
        );
    }

    #[test]
    fn redacts_nested_and_array_shapes() {
        let v = json!({
            "data": {"personalToken": "abc", "nested": {"password": "p"}},
            "list": [{"secret": "s"}, {"ok": "fine"}]
        });
        let s = prepare_log_body(&v).unwrap();
        let parsed: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed["data"]["personalToken"], REDACTED);
        assert_eq!(parsed["data"]["nested"]["password"], REDACTED);
        assert_eq!(parsed["list"][0]["secret"], REDACTED);
        assert_eq!(parsed["list"][1]["ok"], "fine");
    }

    #[test]
    fn usage_counters_survive_redaction() {
        // The suffix rule must not wipe the numbers that make the log useful.
        let v = json!({
            "total_tokens": 1234,
            "free_tokens": 5,
            "prompt_tokens": 100,
            "completion_tokens": 200,
            "model": "qd/ultimate"
        });
        let s = prepare_log_body(&v).unwrap();
        let parsed: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed["total_tokens"], 1234);
        assert_eq!(parsed["free_tokens"], 5);
        assert_eq!(parsed["prompt_tokens"], 100);
        assert_eq!(parsed["completion_tokens"], 200);
    }

    #[test]
    fn ordinary_prose_is_not_redacted() {
        let prose = "Please review the following code and explain version 1.2.3";
        let v = json!({"messages": [{"role": "user", "content": prose}]});
        let s = prepare_log_body(&v).unwrap();
        let parsed: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed["messages"][0]["content"], prose);
    }
}
