//! Runtime client-version resolution for providers that gate on it.
//!
//! Some upstreams reject a request outright when the client version stamped
//! into its headers is older than a threshold. grok-cli is the sharpest case:
//! at version 1.0.5 every dispatch returns `426 Upgrade Required`, and the
//! provider is simply dead until someone edits a constant and redeploys.
//!
//! Vendors publish the current version to a public, unauthenticated endpoint
//! (a GCS release pointer, an npm dist-tag, a download page). Reading it is
//! cheaper than tracking releases by hand and never goes stale, so the value
//! is resolved at runtime and refreshed on a timer.
//!
//! Two rules keep this from becoming a liability:
//!
//! * **Reads on the request path never fetch.** `get` is synchronous and
//!   returns the last known good value; only the background worker performs
//!   I/O, so a slow or dead version endpoint cannot add latency to a chat.
//! * **A pinned fallback always exists.** If every source fails, is slow, or
//!   returns something that is not a version, the pinned constant is used. A
//!   stale version is far better than blocking dispatch, and a malformed one
//!   would be worse than either — hence the shape check before storing.

use parking_lot::RwLock;
use reqwest::Client;
use std::time::Duration;
use tracing::{debug, info, warn};

/// How to pull a version out of a response body.
#[derive(Debug, Clone, Copy)]
pub enum Extract {
    /// The body *is* the version, possibly with surrounding whitespace.
    PlainText,
    /// npm registry document: `{"version": "1.2.3", ...}`.
    NpmLatest,
    /// YAML/text line of the form `<field>: <version>`.
    YamlField(&'static str),
    /// Find `field`, then take the first dotted version that follows it.
    ///
    /// For HTML pages whose payload is JSON with escaped quotes, e.g.
    /// `currentVersion\":\"1.2.37\"`. Scoped to the field so a version in an
    /// unrelated URL earlier in the page cannot win.
    AfterField(&'static str),
}

/// One ordered upstream probe. Earlier entries win.
#[derive(Debug, Clone, Copy)]
pub struct VersionSource {
    pub url: &'static str,
    pub extract: Extract,
}

/// A version that is resolved from upstream, with a pinned fallback.
pub struct ClientVersion {
    name: &'static str,
    pinned: &'static str,
    sources: &'static [VersionSource],
    current: RwLock<String>,
    /// Why the last refresh failed, if it did. Surfaced in logs so an operator
    /// can tell "no network" from "the endpoint changed shape".
    last_error: RwLock<Option<String>>,
}

impl ClientVersion {
    pub const fn new(
        name: &'static str,
        pinned: &'static str,
        sources: &'static [VersionSource],
    ) -> Self {
        Self {
            name,
            pinned,
            sources,
            current: RwLock::new(String::new()),
            last_error: RwLock::new(None),
        }
    }

    /// The version to stamp into request headers.
    ///
    /// Never blocks on network I/O: this is the resolved value when the worker
    /// has one, otherwise the pinned fallback.
    pub fn get(&self) -> String {
        let held = self.current.read().clone();
        if held.is_empty() {
            self.pinned.to_string()
        } else {
            held
        }
    }

    /// Resolved value and whether it came from upstream, for diagnostics.
    pub fn snapshot(&self) -> (String, bool) {
        let held = self.current.read().clone();
        if held.is_empty() {
            (self.pinned.to_string(), false)
        } else {
            (held, true)
        }
    }

    pub fn pinned(&self) -> &'static str {
        self.pinned
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Why the last refresh did not resolve a version, if it did not.
    pub fn last_error(&self) -> Option<String> {
        self.last_error.read().clone()
    }

    /// Try each source in order and adopt the first usable version.
    ///
    /// Returns the version that ended up in effect. Any failure leaves the
    /// previous value untouched.
    pub async fn refresh(&self, client: &Client) -> String {
        let mut failures: Vec<String> = Vec::new();
        for source in self.sources {
            match fetch_version(client, source).await {
                Ok(v) => {
                    if !is_semverish(&v) {
                        // A captive portal or error page can return 200 with
                        // HTML. Storing that would put garbage in a header.
                        let msg = format!(
                            "{} returned a non-version ({})",
                            source.url,
                            v.chars().take(60).collect::<String>()
                        );
                        warn!(provider = self.name, "{msg}");
                        failures.push(msg);
                        continue;
                    }
                    let changed = {
                        let mut w = self.current.write();
                        let changed = *w != v;
                        *w = v.clone();
                        changed
                    };
                    *self.last_error.write() = None;
                    if changed {
                        info!(
                            provider = self.name,
                            version = %v,
                            pinned = self.pinned,
                            "resolved client version from upstream"
                        );
                    } else {
                        debug!(provider = self.name, version = %v, "client version unchanged");
                    }
                    return v;
                }
                Err(e) => {
                    let msg = format!("{}: {e}", source.url);
                    debug!(provider = self.name, "{msg}");
                    failures.push(msg);
                }
            }
        }
        // Every source failed: keep whatever was already resolved (or the
        // pinned fallback) and remember why, so the next refresh and the
        // startup log can explain it.
        let detail = if failures.is_empty() {
            "no version sources configured".to_string()
        } else {
            failures.join("; ")
        };
        *self.last_error.write() = Some(detail);
        self.get()
    }
}

const FETCH_TIMEOUT: Duration = Duration::from_secs(6);

/// A browser-shaped header set.
///
/// Some vendor hosts sit behind a WAF that rejects a bare programmatic fetch.
/// kiro.dev answers 200 to a browser and 403 to a minimal client, so the fetch
/// presents ordinary browser headers rather than reqwest's defaults.
const BROWSER_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

async fn fetch_version(client: &Client, source: &VersionSource) -> Result<String, String> {
    let resp = client
        .get(source.url)
        .timeout(FETCH_TIMEOUT)
        .header("User-Agent", BROWSER_UA)
        .header("Accept", "text/html,application/json,text/plain,*/*")
        .header("Accept-Language", "en-US,en;q=0.9")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let body = resp.text().await.map_err(|e| e.to_string())?;
    extract_version(&body, source.extract).ok_or_else(|| "no version in body".to_string())
}

/// Pull a version out of a body according to `kind`.
pub fn extract_version(body: &str, kind: Extract) -> Option<String> {
    match kind {
        Extract::PlainText => Some(body.trim().to_string()),
        Extract::NpmLatest => {
            let v: serde_json::Value = serde_json::from_str(body).ok()?;
            v.get("version")?.as_str().map(|s| s.trim().to_string())
        }
        Extract::YamlField(field) => body.lines().find_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix(field)?.trim_start();
            let rest = rest.strip_prefix(':')?.trim();
            if rest.is_empty() {
                None
            } else {
                Some(rest.to_string())
            }
        }),
        Extract::AfterField(field) => {
            let at = body.find(field)?;
            first_version_after(&body[at + field.len()..])
        }
    }
}

/// The first `\d+\.\d+\.\d+` run in `haystack`, bounded to a short window so a
/// field is not matched against a version from somewhere far below it.
fn first_version_after(haystack: &str) -> Option<String> {
    let window = &haystack[..haystack.len().min(120)];
    let bytes = window.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        let mut dots = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'0'..=b'9' => i += 1,
                b'.' => {
                    dots += 1;
                    i += 1;
                }
                _ => break,
            }
        }
        if dots >= 2 {
            let candidate = &window[start..i];
            // Trailing dot: `1.2.` from a truncated run.
            if !candidate.ends_with('.') {
                return Some(candidate.to_string());
            }
        }
    }
    None
}

/// `1.2.3`, optionally with a fourth segment or a `-`/`+` suffix.
///
/// Deliberately strict: this guards a header value, so anything that is not
/// recognisably a version is rejected rather than passed upstream.
pub fn is_semverish(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    let core = value.split(['-', '+']).next().unwrap_or("");
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() < 3 {
        return false;
    }
    parts
        .iter()
        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semverish_accepts_real_versions() {
        for v in ["1.0.46", "4.1.22", "2.19.1", "1.2.3.4", "1.0.0-beta.1", "1.0.46+build"] {
            assert!(is_semverish(v), "{v} should be accepted");
        }
    }

    #[test]
    fn semverish_rejects_junk() {
        // A 200 response from a captive portal or an error page must never end
        // up in a version header.
        for v in ["", " ", "not-a-version", "<!doctype html>", "1.0", "v1.0.46", "1.x.3"] {
            assert!(!is_semverish(v), "{v} should be rejected");
        }
    }

    #[test]
    fn extract_plain_text() {
        assert_eq!(
            extract_version("1.0.46\n", Extract::PlainText).as_deref(),
            Some("1.0.46")
        );
    }

    #[test]
    fn extract_npm_latest() {
        let body = r#"{"name":"@xai-official/grok","version":"1.0.46","other":1}"#;
        assert_eq!(
            extract_version(body, Extract::NpmLatest).as_deref(),
            Some("1.0.46")
        );
        assert_eq!(extract_version("not json", Extract::NpmLatest), None);
        assert_eq!(extract_version(r#"{"name":"x"}"#, Extract::NpmLatest), None);
    }

    #[test]
    fn extract_yaml_field() {
        let body = "version: 2.19.1\nfiles:\n  - url: x\n";
        assert_eq!(
            extract_version(body, Extract::YamlField("version")).as_deref(),
            Some("2.19.1")
        );
        assert_eq!(extract_version("other: 1", Extract::YamlField("version")), None);
    }

    #[test]
    fn extract_after_field_skips_versions_that_precede_it() {
        // The real kiro page carries release URLs before the field; only the
        // value after `currentVersion` may win.
        let body = r#"<a href="/releases/1.0.9/x.dmg">old</a>currentVersion\":\"1.2.37\"<span>IDE 1.2.37</span>"#;
        assert_eq!(
            extract_version(body, Extract::AfterField("currentVersion")).as_deref(),
            Some("1.2.37")
        );
        assert_eq!(extract_version("no such field here", Extract::AfterField("currentVersion")), None);
        // Two-part numbers are not versions for this purpose.
        assert_eq!(extract_version("currentVersion\":\"1.2\"", Extract::AfterField("currentVersion")), None);
    }

    #[test]
    fn get_falls_back_to_pinned_before_first_refresh() {
        static SOURCES: &[VersionSource] = &[];
        let v = ClientVersion::new("test", "1.0.46", SOURCES);
        assert_eq!(v.get(), "1.0.46");
        assert_eq!(v.snapshot(), ("1.0.46".to_string(), false));
    }

    #[test]
    fn get_prefers_resolved_value_once_set() {
        static SOURCES: &[VersionSource] = &[];
        let v = ClientVersion::new("test", "1.0.46", SOURCES);
        *v.current.write() = "9.9.9".to_string();
        assert_eq!(v.get(), "9.9.9");
        assert_eq!(v.snapshot(), ("9.9.9".to_string(), true));
    }
}
