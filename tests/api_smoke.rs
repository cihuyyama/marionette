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
