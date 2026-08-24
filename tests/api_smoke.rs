use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use marionette::api;
use marionette::config::Config;
use marionette::db;
use marionette::state::AppState;
use serde_json::Value;
use std::path::PathBuf;
use tower::ServiceExt;

async fn test_app() -> (axum::Router, PathBuf) {
    let dir = std::env::temp_dir().join(format!("marionette-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("test.sqlite");
    let mut cfg = Config::from_env();
    cfg.db_path = db_path.clone();
    cfg.api_key = "test-pool-key".into();
    cfg.admin_key = "test-admin-key".into();
    cfg.cors_origin = "http://localhost:1941".into();
    let pool = db::connect(&cfg.db_path).await.unwrap();
    let state = AppState::new(pool, cfg);
    (api::router(state), dir)
}

async fn body_json(res: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

#[tokio::test]
async fn s1_health_ok() {
    let (app, _dir) = test_app().await;
    let res = app
        .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["status"], "ok");
}

#[tokio::test]
async fn s3_models_requires_key() {
    let (app, _dir) = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn s2_models_with_pool_key() {
    let (app, _dir) = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header(header::AUTHORIZATION, "Bearer test-pool-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let data = v["data"].as_array().expect("data array");
    assert!(
        data.iter().any(|m| m["id"].as_str().unwrap_or("").starts_with("gcli/")),
        "expected gcli/* model"
    );
}

#[tokio::test]
async fn s5_admin_wrong_key() {
    let (app, _dir) = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .uri("/admin/stats")
                .header(header::AUTHORIZATION, "Bearer wrong")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn s4_admin_stats_ok() {
    let (app, _dir) = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .uri("/admin/stats")
                .header(header::AUTHORIZATION, "Bearer test-admin-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert!(v.get("total").is_some());
    assert!(v.get("bound").is_some());
}

#[tokio::test]
async fn mask_token_unit() {
    assert_eq!(marionette::db::mask_token("short"), "****");
    let m = marionette::db::mask_token("abcdefghijklmnop");
    assert!(m.starts_with("abcd"));
    assert!(m.ends_with("mnop"));
    assert!(m.contains('…') || m.contains("..."));
}

#[tokio::test]
async fn provider_routing() {
    use marionette::openai::{ChatCompletionRequest, ChatMessage};
    let req = ChatCompletionRequest {
        model: "gcli/grok-4.5".into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: serde_json::json!("hi"),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }],
        stream: None,
        temperature: None,
        max_tokens: None,
        top_p: None,
        tools: None,
        tool_choice: None,
        parallel_tool_calls: None,
        extra: serde_json::json!({}),
    };
    assert_eq!(req.provider_id(), Some("grok-cli"));
    assert_eq!(req.upstream_model(), "grok-4.5");
}

#[tokio::test]
async fn provider_routing_blackbox() {
    use marionette::openai::provider_id_for_model;
    assert_eq!(provider_id_for_model("bb/z-ai/glm-5.2"), Some("blackbox"));
    assert_eq!(
        provider_id_for_model("bb/blackboxai/blackbox-pro"),
        Some("blackbox")
    );
    // bare blackboxai/* routes to blackbox
    assert_eq!(
        provider_id_for_model("blackboxai/x-ai/grok-4.3"),
        Some("blackbox")
    );
    // grok-containing blackbox id must NOT route to grok-cli
    assert_eq!(
        provider_id_for_model("bb/blackboxai/x-ai/grok-4.3"),
        Some("blackbox")
    );
    // grok routing unchanged
    assert_eq!(provider_id_for_model("gcli/grok-4.5"), Some("grok-cli"));
    assert_eq!(provider_id_for_model("grok-3"), Some("grok-cli"));
}

#[tokio::test]
async fn images_generations_requires_key() {
    let (app, _dir) = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/images/generations")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"prompt":"a cat"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn images_generations_missing_prompt() {
    let (app, _dir) = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/images/generations")
                .header(header::AUTHORIZATION, "Bearer test-pool-key")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"grok-imagine-image"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn images_edits_requires_image() {
    let (app, _dir) = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/images/edits")
                .header(header::AUTHORIZATION, "Bearer test-pool-key")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"prompt":"make it blue","model":"grok-imagine-image-edit"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn images_generations_no_accounts() {
    let (app, _dir) = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/images/generations")
                .header(header::AUTHORIZATION, "Bearer test-pool-key")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"prompt":"a cat","model":"grok-imagine-image"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::OK);
    assert_ne!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn models_lists_imagine() {
    let (app, _dir) = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header(header::AUTHORIZATION, "Bearer test-pool-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let data = v["data"].as_array().expect("data array");
    assert!(
        data.iter().any(|m| m["id"].as_str() == Some("gcli/grok-imagine-image")),
        "expected gcli/grok-imagine-image in catalog"
    );
}

#[tokio::test]
async fn combos_require_admin_key() {
    let (app, _dir) = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .uri("/admin/combos")
                .header(header::AUTHORIZATION, "Bearer test-pool-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

async fn post_combo(app: &axum::Router, body: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/combos")
                .header(header::AUTHORIZATION, "Bearer test-admin-key")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn combo_crud_and_models_listing() {
    let (app, _dir) = test_app().await;

    let created = post_combo(
        &app,
        r#"{"slug":"coding","name":"Coding","targets":["gcli/grok-4.5","qd/ultimate"]}"#,
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let v = body_json(created).await;
    assert_eq!(v["id"], "combo/coding");
    assert_eq!(v["targets"].as_array().unwrap().len(), 2);

    let listed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header(header::AUTHORIZATION, "Bearer test-pool-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mv = body_json(listed).await;
    assert!(
        mv["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"].as_str() == Some("combo/coding") && m["owned_by"] == "combo"),
        "combo should appear in /v1/models"
    );

    let deleted = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/admin/combos/coding")
                .header(header::AUTHORIZATION, "Bearer test-admin-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    let dv = body_json(deleted).await;
    assert_eq!(dv["ok"], true);
    assert_eq!(dv["id"], "combo/coding");
}

#[tokio::test]
async fn combo_create_rejects_invalid_target() {
    let (app, _dir) = test_app().await;
    let res = post_combo(
        &app,
        r#"{"slug":"bad","name":"Bad","targets":["qd/not-real"]}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn combo_create_rejects_nested_combo_target() {
    let (app, _dir) = test_app().await;
    let res = post_combo(
        &app,
        r#"{"slug":"nested","name":"Nested","targets":["combo/other"]}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn api_key_lifecycle_create_auth_revoke() {
    let (app, _dir) = test_app().await;

    let created = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/keys")
                .header(header::AUTHORIZATION, "Bearer test-admin-key")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"name":"smoke"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let v = body_json(created).await;
    let plaintext = v["key"].as_str().expect("plaintext returned once").to_string();
    assert!(plaintext.starts_with("mk-"));
    assert!(plaintext.len() >= 43, "mk- + 40+ hex chars");
    let key_id = v["key_view"]["id"].as_str().unwrap().to_string();
    assert_eq!(v["key_view"]["key_prefix"], &plaintext[..8]);
    assert!(v["key_view"].get("key_hash").is_none(), "never leak hash");

    let ok = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header(header::AUTHORIZATION, format!("Bearer {plaintext}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK, "fresh db key authenticates");

    let revoked = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/admin/keys/{key_id}"))
                .header(header::AUTHORIZATION, "Bearer test-admin-key")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"is_active":false}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::OK);

    let rejected = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header(header::AUTHORIZATION, format!("Bearer {plaintext}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn api_key_admin_endpoints_require_admin_key() {
    let (app, _dir) = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .uri("/admin/keys")
                .header(header::AUTHORIZATION, "Bearer test-pool-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

async fn post_byok(app: &axum::Router, body: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/byok")
                .header(header::AUTHORIZATION, "Bearer test-admin-key")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn byok_create_rejects_bad_slug() {
    let (app, _dir) = test_app().await;
    let res = post_byok(
        &app,
        r#"{"slug":"Has Space","base_url":"https://openrouter.ai/api/v1","api_key":"sk-test","auto_fetch":false}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let res = post_byok(
        &app,
        r#"{"slug":"-lead","base_url":"https://openrouter.ai/api/v1","api_key":"sk-test","auto_fetch":false}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn byok_create_rejects_reserved_slug() {
    let (app, _dir) = test_app().await;
    for slug in ["bb", "gcli", "qd", "combo", "blackbox-x", "my-grok-api"] {
        let res = post_byok(
            &app,
            &format!(
                r#"{{"slug":"{slug}","base_url":"https://openrouter.ai/api/v1","api_key":"sk-test","auto_fetch":false}}"#
            ),
        )
        .await;
        assert_eq!(
            res.status(),
            StatusCode::BAD_REQUEST,
            "reserved slug '{slug}' must be rejected"
        );
    }
}

#[tokio::test]
async fn byok_create_rejects_bad_base_url_and_empty_key() {
    let (app, _dir) = test_app().await;
    let res = post_byok(
        &app,
        r#"{"slug":"ok","base_url":"ftp://x.ai","api_key":"sk-test","auto_fetch":false}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let res = post_byok(
        &app,
        r#"{"slug":"ok","base_url":"https://openrouter.ai/api/v1","api_key":"   ","auto_fetch":false}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn byok_create_happy_path_and_duplicate_409() {
    let (app, _dir) = test_app().await;
    let body = r#"{"slug":"openrouter","name":"OpenRouter","base_url":"https://openrouter.ai/api/v1","api_key":"sk-test-long-key","auto_fetch":false}"#;

    let created = post_byok(&app, body).await;
    assert_eq!(created.status(), StatusCode::OK);
    let v = body_json(created).await;
    assert_eq!(v["provider"], "byok");
    assert_eq!(v["email"], "openrouter");
    assert_eq!(v["models_count"], 0);
    assert!(v.get("models_fetch_error").is_none());
    let key = v["data"]["apiKey"].as_str().unwrap();
    assert!(key.contains('…') || key.contains("..."), "apiKey must be masked");
    assert_eq!(v["data"]["baseUrl"], "https://openrouter.ai/api/v1");

    let listed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/accounts")
                .header(header::AUTHORIZATION, "Bearer test-admin-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let lv = body_json(listed).await;
    assert!(
        lv["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["provider"] == "byok" && a["email"] == "openrouter"),
        "created byok endpoint appears in /admin/accounts"
    );

    let dup = post_byok(&app, body).await;
    assert_eq!(dup.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn provider_routing_freebuff() {
    use marionette::openai::provider_id_for_model;
    assert_eq!(provider_id_for_model("fb/deepseek/deepseek-v4-flash"), Some("freebuff"));
    assert_eq!(provider_id_for_model("fb/z-ai/glm-5.2"), Some("freebuff"));
    // bare freebuff prefix routes to freebuff
    assert_eq!(provider_id_for_model("freebuff-x"), Some("freebuff"));
    // existing providers unchanged
    assert_eq!(provider_id_for_model("gcli/grok-4.5"), Some("grok-cli"));
    assert_eq!(provider_id_for_model("grok-3"), Some("grok-cli"));
    assert_eq!(provider_id_for_model("bb/z-ai/glm-5.2"), Some("blackbox"));
    assert_eq!(provider_id_for_model("qd/auto"), Some("qoder"));
    assert_eq!(provider_id_for_model("unknown-model"), None);
}

#[tokio::test]
async fn freebuff_models_in_catalog() {
    let (app, _dir) = test_app().await;
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header(header::AUTHORIZATION, "Bearer test-pool-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let data = v["data"].as_array().expect("data array");
    let fb_ids: Vec<&str> = data
        .iter()
        .filter(|m| m["owned_by"] == "freebuff")
        .filter_map(|m| m["id"].as_str())
        .collect();
    assert_eq!(fb_ids.len(), 9, "expected 9 freebuff catalog entries (2026-08-22 refresh)");
    assert!(fb_ids.contains(&"fb/deepseek/deepseek-v4-flash"));
    assert!(fb_ids.contains(&"fb/openai/gpt-5.6-luna"));
    // new models from the 2026-08-22 catalog refresh
    assert!(fb_ids.contains(&"fb/mimo/mimo-v2.5"));
    assert!(fb_ids.contains(&"fb/stealth/ox-alpha"));
    assert!(fb_ids.contains(&"fb/anthropic/claude-fable-5"));
    // minimax-m3 was withdrawn upstream on 2026-08-20
    assert!(!fb_ids.contains(&"fb/minimax/minimax-m3"), "minimax-m3 must be withdrawn");
}

#[tokio::test]
async fn freebuff_import_mask_and_refresh_paths() {
    let (app, _dir) = test_app().await;

    // import a freebuff account via POST /admin/accounts (token inferred)
    let imported = admin_request(
        &app,
        "POST",
        "/admin/accounts",
        Some(r#"{"provider":"freebuff","email":"fb-user-1","token":"cb_test-token-abcdef123456"}"#),
    )
    .await;
    assert_eq!(imported.status(), StatusCode::OK);
    let iv = body_json(imported).await;
    assert_eq!(iv["inserted"], 1);

    // GET /admin/accounts masks the token
    let listed = admin_request(&app, "GET", "/admin/accounts?provider=freebuff", None).await;
    assert_eq!(listed.status(), StatusCode::OK);
    let lv = body_json(listed).await;
    let accounts = lv["accounts"].as_array().expect("accounts array");
    assert_eq!(accounts.len(), 1);
    let acc = &accounts[0];
    assert_eq!(acc["provider"], "freebuff");
    assert_eq!(acc["email"], "fb-user-1");
    let masked = acc["data"]["token"].as_str().expect("token present (masked)");
    assert!(
        masked.contains('…') || masked.contains("..."),
        "token must be masked in admin listing, got: {masked}"
    );
    assert_eq!(acc["quota_kind"], "none");

    let account_id = acc["id"].as_str().unwrap().to_string();

    // refresh of an account WITHOUT a token → 400 (no network)
    let no_token = admin_request(
        &app,
        "POST",
        "/admin/accounts",
        Some(r#"{"provider":"freebuff","email":"fb-user-2","token":""}"#),
    )
    .await;
    assert_eq!(no_token.status(), StatusCode::OK);
    let nv = body_json(no_token).await;
    assert_eq!(nv["inserted"], 1);

    let listed2 = admin_request(&app, "GET", "/admin/accounts?provider=freebuff", None).await;
    let lv2 = body_json(listed2).await;
    let empty_id = lv2["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["email"] == "fb-user-2")
        .expect("fb-user-2 imported")["id"]
        .as_str()
        .unwrap()
        .to_string();

    let refresh_bad = admin_request(&app, "POST", &format!("/admin/accounts/{empty_id}/refresh"), None).await;
    assert_eq!(refresh_bad.status(), StatusCode::BAD_REQUEST);

    // refresh of a non-existent account → 404
    let refresh_404 = admin_request(&app, "POST", "/admin/accounts/ghost-id/refresh", None).await;
    assert_eq!(refresh_404.status(), StatusCode::NOT_FOUND);

    let deleted = admin_request(&app, "DELETE", &format!("/admin/accounts/{account_id}"), None).await;
    assert_eq!(deleted.status(), StatusCode::OK);
}

#[tokio::test]
async fn freebuff_provider_settings_patchable() {
    let (app, _dir) = test_app().await;
    let res = admin_request(
        &app,
        "PATCH",
        "/admin/providers/freebuff",
        Some(r#"{"load_balance":"least_used"}"#),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["provider"], "freebuff");
    assert_eq!(v["load_balance"], "least_used");
}

#[tokio::test]
async fn freebuff_byok_slug_reserved() {
    let (app, _dir) = test_app().await;
    for slug in ["fb", "freebuff-x"] {
        let res = post_byok(
            &app,
            &format!(
                r#"{{"slug":"{slug}","base_url":"https://openrouter.ai/api/v1","api_key":"sk-test","auto_fetch":false}}"#
            ),
        )
        .await;
        assert_eq!(
            res.status(),
            StatusCode::BAD_REQUEST,
            "reserved freebuff slug '{slug}' must be rejected"
        );
    }
}

async fn admin_request(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<&str>,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, "Bearer test-admin-key");
    let body = match body {
        Some(b) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    app.clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn byok_multi_key_lifecycle() {
    let (app, _dir) = test_app().await;

    // (a) first key creates the provider
    let first = post_byok(
        &app,
        r#"{"slug":"multi","name":"Multi","base_url":"https://openrouter.ai/api/v1","api_key":"sk-first-key-111","auto_fetch":false}"#,
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    let v = body_json(first).await;
    assert_eq!(v["new_provider"], true, "first key creates the slug");

    // (a) second key under the same slug, no base_url (reused)
    let second = post_byok(
        &app,
        r#"{"slug":"multi","api_key":"sk-second-key-222","auto_fetch":false}"#,
    )
    .await;
    assert_eq!(second.status(), StatusCode::OK);
    let v = body_json(second).await;
    assert_eq!(v["new_provider"], false, "slug already existed");
    assert_eq!(v["name"], "Multi", "name defaults to the provider name");
    assert_eq!(v["data"]["baseUrl"], "https://openrouter.ai/api/v1");

    let listed = admin_request(&app, "GET", "/admin/accounts?provider=byok&slug=multi", None).await;
    assert_eq!(listed.status(), StatusCode::OK);
    let lv = body_json(listed).await;
    assert_eq!(
        lv["accounts"].as_array().unwrap().len(),
        2,
        "slug filter returns both keys"
    );

    // (a) slug filter is case-insensitive
    let listed = admin_request(&app, "GET", "/admin/accounts?provider=byok&slug=MULTI", None).await;
    let lv = body_json(listed).await;
    assert_eq!(lv["accounts"].as_array().unwrap().len(), 2);

    // (b) same key twice → 409
    let dup = post_byok(
        &app,
        r#"{"slug":"multi","api_key":"  sk-first-key-111  ","auto_fetch":false}"#,
    )
    .await;
    assert_eq!(dup.status(), StatusCode::CONFLICT);
    let dv = body_json(dup).await;
    assert!(dv["error"]["message"]
        .as_str()
        .unwrap()
        .contains("key already added for byok endpoint 'multi'"));

    // (c) new slug without base_url → 400
    let res = post_byok(
        &app,
        r#"{"slug":"fresh","api_key":"sk-x","auto_fetch":false}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // (c) existing slug with an invalid base_url → 400
    let res = post_byok(
        &app,
        r#"{"slug":"multi","base_url":"ftp://bad","api_key":"sk-y","auto_fetch":false}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // (d) grouped provider summary
    let listed = admin_request(&app, "GET", "/admin/byok", None).await;
    assert_eq!(listed.status(), StatusCode::OK);
    let pv = body_json(listed).await;
    let providers = pv["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0]["slug"], "multi");
    assert_eq!(providers[0]["name"], "Multi");
    assert_eq!(providers[0]["base_url"], "https://openrouter.ai/api/v1");
    assert_eq!(providers[0]["keys"], 2);
    assert_eq!(providers[0]["bound"], 2);
    assert_eq!(providers[0]["sealed"], 0);
    assert_eq!(providers[0]["cut"], 0);
    assert_eq!(providers[0]["fallen"], 0);
    assert_eq!(providers[0]["inactive"], 0);

    // (e) delete removes every key of the slug
    let deleted = admin_request(&app, "DELETE", "/admin/byok/multi", None).await;
    assert_eq!(deleted.status(), StatusCode::OK);
    assert_eq!(body_json(deleted).await["deleted"], 2);

    let listed = admin_request(&app, "GET", "/admin/byok", None).await;
    assert!(body_json(listed).await["providers"].as_array().unwrap().is_empty());

    let again = admin_request(&app, "DELETE", "/admin/byok/multi", None).await;
    assert_eq!(again.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn byok_models_deduped_across_keys_of_one_slug() {
    let (app, dir) = test_app().await;

    for key in ["sk-one-111", "sk-two-222"] {
        let res = post_byok(
            &app,
            &format!(
                r#"{{"slug":"dedup","base_url":"https://openrouter.ai/api/v1","api_key":"{key}","auto_fetch":false}}"#
            ),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
    }

    let pool = db::connect(&dir.join("test.sqlite")).await.unwrap();
    let rows = db::list_accounts(&pool, Some("byok"), None, Some("dedup"))
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    for mut acc in rows {
        let mut data = acc.data_json();
        data["models"] = serde_json::json!(["m1"]);
        data["modelsFetchedAt"] = serde_json::json!("2026-01-01T00:00:00.000Z");
        acc.set_data_json(&data);
        db::update_account(&pool, &acc).await.unwrap();
    }

    let listed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header(header::AUTHORIZATION, "Bearer test-pool-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mv = body_json(listed).await;
    let byok_ids: Vec<String> = mv["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["owned_by"] == "byok")
        .filter_map(|m| m["id"].as_str().map(|s| s.to_string()))
        .collect();
    assert_eq!(
        byok_ids,
        vec!["dedup/m1"],
        "each slug's model set is emitted exactly once"
    );
}

#[tokio::test]
async fn byok_models_listed_only_after_models_present() {
    let (app, dir) = test_app().await;
    let created = post_byok(
        &app,
        r#"{"slug":"myapi","base_url":"https://openrouter.ai/api/v1","api_key":"sk-test-long-key","auto_fetch":false}"#,
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let v = body_json(created).await;
    let id = v["id"].as_str().unwrap().to_string();
    assert_eq!(v["name"], "myapi", "display name defaults to slug");

    let list_ids = |v: &Value| -> Vec<String> {
        v["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| {
                if m["owned_by"] == "byok" {
                    m["id"].as_str().map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect()
    };

    let listed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header(header::AUTHORIZATION, "Bearer test-pool-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mv = body_json(listed).await;
    assert!(
        list_ids(&mv).is_empty(),
        "no byok models until endpoint has a fetched model list"
    );

    let pool = db::connect(&dir.join("test.sqlite")).await.unwrap();
    let mut acc = db::get_account(&pool, &id).await.unwrap();
    let mut data = acc.data_json();
    data["models"] = serde_json::json!(["anthropic/claude-x", "openai/gpt-x"]);
    data["modelsFetchedAt"] = serde_json::json!("2026-01-01T00:00:00.000Z");
    acc.set_data_json(&data);
    db::update_account(&pool, &acc).await.unwrap();

    let listed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header(header::AUTHORIZATION, "Bearer test-pool-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mv = body_json(listed).await;
    let ids = list_ids(&mv);
    assert_eq!(ids, vec!["myapi/anthropic/claude-x", "myapi/openai/gpt-x"]);

    let fetched = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/admin/accounts/{id}"))
                .header(header::AUTHORIZATION, "Bearer test-admin-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(fetched.status(), StatusCode::OK);
    let fv = body_json(fetched).await;
    let masked = fv["data"]["apiKey"].as_str().unwrap();
    assert!(masked.contains('…') || masked.contains("..."));
    assert_eq!(fv["data"]["baseUrl"], "https://openrouter.ai/api/v1");
    assert_eq!(fv["quota_kind"], "none");

    let patched = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/admin/accounts/{id}"))
                .header(header::AUTHORIZATION, "Bearer test-admin-key")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"name":"renamed","base_url":"https://other.dev","api_key":"sk-new-long-key"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);
    let pv = body_json(patched).await;
    assert_eq!(pv["name"], "renamed");
    assert_eq!(pv["data"]["baseUrl"], "https://other.dev");
    assert_eq!(pv["data"]["models"].as_array().unwrap().len(), 2, "patch preserves data.models");

    let byok_models_404 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/ghost/byok-models")
                .header(header::AUTHORIZATION, "Bearer test-admin-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(byok_models_404.status(), StatusCode::NOT_FOUND);

    let deleted = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/admin/accounts/{id}"))
                .header(header::AUTHORIZATION, "Bearer test-admin-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
}

#[tokio::test]
async fn commandcode_dynamic_models_dedupe_against_static_catalog() {
    let (app, dir) = test_app().await;
    let pool = db::connect(&dir.join("test.sqlite")).await.unwrap();
    let data = serde_json::json!({
        "apiKey": "user_test_commandcode_key",
        "models": ["xiaomi/mimo-v2.5", "zai-org/GLM-5.3"],
        "modelsFetchedAt": "2026-08-24T00:00:00.000Z"
    });
    let acc = db::Account {
        id: "cc-test-1".into(),
        provider: "commandcode".into(),
        email: Some("cc-test".into()),
        name: None,
        is_active: 1,
        priority: 0,
        data: data.to_string(),
        cooldown_until: None,
        last_error: None,
        last_used_at: None,
        created_at: "t".into(),
        updated_at: "t".into(),
        quota_limit: 0,
        quota_remaining: 0,
    };
    db::upsert_account(&pool, &acc).await.unwrap();

    let res = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header(header::AUTHORIZATION, "Bearer test-pool-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let ids: Vec<&str> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    assert_eq!(
        ids.iter().filter(|i| **i == "cmc/xiaomi/mimo-v2.5").count(),
        1,
        "static entry must not duplicate when the dynamic catalog lists it"
    );
    assert_eq!(
        ids.iter().filter(|i| **i == "cmc/zai-org/GLM-5.3").count(),
        1,
        "dynamic-only entry must appear exactly once"
    );
}
