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
