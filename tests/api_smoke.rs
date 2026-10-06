use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use marionette::api;
use marionette::config::Config;
use marionette::db;
use marionette::state::AppState;
use serde_json::json;
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
    for slug in ["gcli", "qd", "combo", "my-grok-api"] {
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
        "modelMeta": [
            { "id": "xiaomi/mimo-v2.5", "name": "MiMo V2.5", "context_length": 1000000 },
            { "id": "zai-org/GLM-5.3", "name": "GLM-5.3", "context_length": 1000000 }
        ],
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
    let by_id = |id: &str| {
        v["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"].as_str() == Some(id))
            .cloned()
            .expect("model present")
    };
    assert_eq!(
        by_id("cmc/xiaomi/mimo-v2.5")["max_input"].as_str(),
        Some("1M"),
        "static entry enriched with live context_length"
    );
    assert_eq!(
        by_id("cmc/xiaomi/mimo-v2.5")["display_name"].as_str(),
        Some("MiMo V2.5"),
        "static entry display_name from live meta"
    );
    assert_eq!(
        by_id("cmc/zai-org/GLM-5.3")["max_input"].as_str(),
        Some("1M"),
        "dynamic entry carries max_input"
    );
}

// ── Log retention (workers/retention.rs) ──────────────────────────────

async fn retention_pool() -> sqlx::SqlitePool {
    let dir = std::env::temp_dir().join(format!("marionette-retention-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    db::connect(&dir.join("test.sqlite")).await.unwrap()
}

fn days_ago(d: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::days(d)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

async fn seed_log(pool: &sqlx::SqlitePool, created_at: &str, with_body: bool) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    let body = if with_body { Some("{\"messages\":[]}") } else { None };
    sqlx::query(
        "INSERT INTO request_logs (id, created_at, provider, model, status, stream, request_body, response_body)
         VALUES (?,?,?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(created_at)
    .bind("grok-cli")
    .bind("m1")
    .bind("success")
    .bind(0)
    .bind(body)
    .bind(body)
    .execute(pool)
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn retention_bodies_expire_before_rows() {
    let pool = retention_pool().await;
    let old = seed_log(&pool, &days_ago(40), true).await;
    let recent = seed_log(&pool, &days_ago(1), true).await;

    // Bodies have their own, shorter window: 7 days vs the row's 30.
    let nulled = db::null_old_log_bodies(&pool, &days_ago(7), 500).await.unwrap();
    assert_eq!(nulled, 1, "only the 40-day-old row loses its body");

    let (old_body, recent_body) = (
        sqlx::query_scalar::<_, Option<String>>("SELECT request_body FROM request_logs WHERE id = ?")
            .bind(&old).fetch_one(&pool).await.unwrap(),
        sqlx::query_scalar::<_, Option<String>>("SELECT request_body FROM request_logs WHERE id = ?")
            .bind(&recent).fetch_one(&pool).await.unwrap(),
    );
    assert!(old_body.is_none(), "expired body must be dropped");
    assert!(recent_body.is_some(), "body inside the window must survive");

    // The metadata row outlives its body.
    let still: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_logs")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(still, 2, "body expiry must not delete the row");
}

#[tokio::test]
async fn retention_deletes_rows_past_long_window() {
    let pool = retention_pool().await;
    seed_log(&pool, &days_ago(40), true).await;
    let keep = seed_log(&pool, &days_ago(5), true).await;

    let deleted = db::delete_old_request_logs(&pool, &days_ago(30), 500).await.unwrap();
    assert_eq!(deleted, 1);

    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_logs")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(left, 1);
    let survivor: String = sqlx::query_scalar("SELECT id FROM request_logs")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(survivor, keep);
}

#[tokio::test]
async fn retention_batch_loops_until_drained() {
    let pool = retention_pool().await;
    for _ in 0..7 {
        seed_log(&pool, &days_ago(60), true).await;
    }
    // Batch of 2 against 7 rows: the fn must loop, not stop after one batch.
    let deleted = db::delete_old_request_logs(&pool, &days_ago(30), 2).await.unwrap();
    assert_eq!(deleted, 7);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM request_logs").fetch_one(&pool).await.unwrap(),
        0
    );
}

#[tokio::test]
async fn retention_never_touches_rows_inside_window() {
    let pool = retention_pool().await;
    for d in [0, 1, 10, 29] {
        seed_log(&pool, &days_ago(d), true).await;
    }
    assert_eq!(db::delete_old_request_logs(&pool, &days_ago(30), 500).await.unwrap(), 0);
    assert_eq!(db::null_old_log_bodies(&pool, &days_ago(30), 500).await.unwrap(), 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM request_logs").fetch_one(&pool).await.unwrap(),
        4
    );
}

#[tokio::test]
async fn retention_reclaims_file_space() {
    let dir = std::env::temp_dir().join(format!("marionette-shrink-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.sqlite");
    let pool = db::connect(&path).await.unwrap();

    // 30 rows x ~200 KB of body = ~6 MB of payload, mimicking production rows
    // (measured avg: 257 KB, and bodies are ~99% of the file).
    let body = "z".repeat(200_000);
    for i in 0..30 {
        seed_log(&pool, &days_ago(100 + i), true).await;
    }
    sqlx::query("UPDATE request_logs SET request_body = ?, response_body = ?")
        .bind(&body).bind(&body).execute(&pool).await.unwrap();

    let before = std::fs::metadata(&path).unwrap().len();
    assert!(before > 10_000_000, "sanity: seeded file should be >10MB, got {before}");

    let nulled = db::null_old_log_bodies(&pool, &days_ago(7), 500).await.unwrap();
    assert_eq!(nulled, 30);
    db::delete_old_request_logs(&pool, &days_ago(30), 500).await.unwrap();

    // Without auto_vacuum the pages only become reusable, so assert on the
    // freelist rather than the file size: that is what caps future growth.
    let (_pages, freelist) = db::db_page_stats(&pool).await.unwrap();
    assert!(freelist > 0, "freed pages must return to the free list");

    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_logs")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(left, 0);
}

// ── Credential encryption at rest (src/crypto.rs) ─────────────────────

#[test]
fn sealed_blob_hides_credentials_from_the_file() {
    let k = vec![7u8; 32];
    let plain = r#"{"apiKey":"sk-abcdefgh12345678","refreshToken":"rt_zzzzzzzzzzzz"}"#;
    let sealed = marionette::crypto::seal_with(&k, plain).expect("seal");

    // The whole point: the stored text must not contain the secrets.
    assert!(!sealed.contains("sk-abcdefgh12345678"));
    assert!(!sealed.contains("rt_zzzzzzzzzzzz"));
    assert!(!sealed.contains("apiKey"), "structure must be opaque too");
}

#[test]
fn wrong_key_yields_no_plaintext() {
    let k = vec![7u8; 32];
    let wrong = vec![9u8; 32];
    let sealed = marionette::crypto::seal_with(&k, r#"{"apiKey":"sk-secret123456"}"#).unwrap();
    assert!(
        marionette::crypto::open_with(&wrong, &sealed).is_none(),
        "a wrong key must fail, never return garbage"
    );
}

#[test]
fn legacy_rows_read_without_a_key() {
    // Simulates a DB written before MARIONETTE_DATA_KEY existed.
    let legacy = r#"{"apiKey":"x"}"#;
    assert_eq!(
        marionette::crypto::open_or_passthrough(legacy),
        legacy,
        "pre-encryption rows must still parse"
    );
}

#[tokio::test]
async fn account_roundtrip_preserves_data_blob() {
    // Guards the helper pair used by every provider: whatever goes in must
    // come back out byte-identical, encrypted or not.
    let mut acc = db::Account {
        id: "id1".into(),
        provider: "qoder".into(),
        email: Some("a@b.c".into()),
        name: None,
        is_active: 1,
        priority: 0,
        data: "{}".into(),
        cooldown_until: None,
        last_error: None,
        last_used_at: None,
        created_at: "2026-01-01T00:00:00.000Z".into(),
        updated_at: "2026-01-01T00:00:00.000Z".into(),
        quota_limit: 0,
        quota_remaining: 0,
    };
    let v = serde_json::json!({"apiKey": "sk-abcdefgh12345678", "n": 42});
    acc.set_data_json(&v);
    let back = acc.data_json();
    assert_eq!(back["apiKey"], "sk-abcdefgh12345678");
    assert_eq!(back["n"], 42);
}

// ── Dead vs cut status (src/db.rs::Account::status_label) ─────────────

fn acct(active: i64, last_error: Option<&str>) -> db::Account {
    db::Account {
        id: "a".into(),
        provider: "grok-cli".into(),
        email: None,
        name: None,
        is_active: active,
        priority: 0,
        data: "{}".into(),
        cooldown_until: None,
        last_error: last_error.map(|s| s.to_string()),
        last_used_at: None,
        created_at: "2026-01-01T00:00:00.000Z".into(),
        updated_at: "2026-01-01T00:00:00.000Z".into(),
        quota_limit: 0,
        quota_remaining: 0,
    }
}

#[test]
fn revoked_and_missing_tokens_are_dead_not_cut() {
    // Terminal: needs a new credential, not just time.
    assert_eq!(acct(0, Some("auth invalid: no tokens")).status_label(), "dead");
    assert_eq!(acct(0, Some(r#"auth invalid: {"error":"invalid_grant"}"#)).status_label(), "dead");
    assert_eq!(acct(0, Some("Unauthorized")).status_label(), "dead");
}

#[test]
fn rate_limited_stays_cut() {
    // Recoverable: the account is fine, the limit is not.
    assert_eq!(acct(0, Some("provider: rate limited")).status_label(), "cut");
    assert_eq!(acct(0, Some("upstream error (502)")).status_label(), "cut");
    assert_eq!(acct(0, None).status_label(), "cut");
}

#[test]
fn dead_status_only_applies_to_inactive_accounts() {
    // An active account with a stale auth error is still serving: it is
    // "fallen", not "dead".
    assert_eq!(acct(1, Some("auth invalid: no tokens")).status_label(), "fallen");
}

#[test]
fn healthy_account_is_bound() {
    assert_eq!(acct(1, None).status_label(), "bound");
}

// ── Versioned schema migrations ───────────────────────────────────────

#[tokio::test]
async fn migration_stamps_version_and_is_idempotent() {
    let dir = std::env::temp_dir().join(format!("marionette-mig-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.sqlite");

    // First boot: applies v1.
    let pool = db::connect(&path).await.unwrap();
    let v: i64 = sqlx::query_scalar("SELECT version FROM schema_version WHERE id = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(v, 1, "fresh database must stamp version 1");
    pool.close().await;

    // Second boot on the same file: must not re-apply or fail.
    let pool2 = db::connect(&path).await.unwrap();
    let v2: i64 = sqlx::query_scalar("SELECT version FROM schema_version WHERE id = 1")
        .fetch_one(&pool2)
        .await
        .unwrap();
    assert_eq!(v2, 1, "reconnecting must not change the version");
}

#[tokio::test]
async fn migration_adds_every_expected_column() {
    let dir = std::env::temp_dir().join(format!("marionette-cols-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let pool = db::connect(&dir.join("test.sqlite")).await.unwrap();

    let expected: &[(&str, &str)] = &[
        ("accounts", "quota_limit"),
        ("accounts", "quota_remaining"),
        ("request_logs", "credits_used"),
        ("request_logs", "request_body"),
        ("request_logs", "response_body"),
        ("request_logs", "requested_model"),
        ("request_logs", "combo_id"),
        ("request_logs", "fallback_count"),
        ("request_logs", "attempt_trace"),
        ("request_logs", "api_key_id"),
        ("api_keys", "rate_limit_rpm"),
        ("api_keys", "request_limit"),
        ("api_keys", "requests_used"),
        ("api_keys", "token_limit"),
        ("api_keys", "tokens_used"),
        ("api_keys", "model_allowlist"),
        ("api_keys", "key_prefix"),
        ("api_keys", "last_used_at"),
        ("provider_settings", "pick_mode"),
        ("provider_settings", "sticky_pinned"),
    ];
    for (table, col) in expected {
        let cols: Vec<String> =
            sqlx::query_scalar::<_, String>("SELECT name FROM pragma_table_info(?)")
                .bind(table)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(
            cols.iter().any(|c| c == col),
            "{table}.{col} missing after migration (have: {cols:?})"
        );
    }
}

// ── Anthropic Messages surface (/v1/messages) ──────────────────────────

fn messages_body(v: Value) -> Body {
    Body::from(serde_json::to_vec(&v).unwrap())
}

#[tokio::test]
async fn messages_requires_pool_key() {
    let (app, _dir) = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header(header::CONTENT_TYPE, "application/json")
                .body(messages_body(json!({
                    "model": "qd/ultimate", "max_tokens": 16,
                    "messages": [{"role":"user","content":"hi"}]
                })))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn messages_rejects_empty_messages() {
    let (app, _dir) = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header(header::AUTHORIZATION, "Bearer test-pool-key")
                .header(header::CONTENT_TYPE, "application/json")
                .body(messages_body(json!({"model":"qd/ultimate","max_tokens":16,"messages":[]})))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn messages_rejects_unknown_role() {
    let (app, _dir) = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header(header::AUTHORIZATION, "Bearer test-pool-key")
                .header(header::CONTENT_TYPE, "application/json")
                .body(messages_body(json!({
                    "model":"qd/ultimate","max_tokens":16,
                    "messages":[{"role":"wizard","content":"hi"}]
                })))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn messages_translates_and_reaches_the_pool() {
    // No accounts in a fresh DB, so the pool reports no healthy account. The
    // point is that the Anthropic shape was accepted and translated rather
    // than rejected before routing — a parse failure would be 400.
    let (app, _dir) = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header(header::AUTHORIZATION, "Bearer test-pool-key")
                .header(header::CONTENT_TYPE, "application/json")
                .body(messages_body(json!({
                    "model": "qd/ultimate",
                    "max_tokens": 16,
                    "system": "be brief",
                    "messages": [{"role":"user","content":"hi"}]
                })))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::BAD_REQUEST, "request must be translated, not rejected");
    assert_ne!(res.status(), StatusCode::NOT_FOUND, "route must exist");
}
