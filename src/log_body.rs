use serde_json::Value;

const DEFAULT_MAX_BYTES: usize = 262_144;

pub fn log_body_max_bytes() -> usize {
    std::env::var("MARIONETTE_LOG_BODY_MAX_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_BYTES)
        .max(1024)
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
    let serialized = match serde_json::to_string(value) {
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
}
