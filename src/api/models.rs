use crate::auth::{AdminAuth, PoolAuth};
use crate::db;
use crate::error::AppResult;
use crate::openai::default_models;
use crate::state::AppState;
use axum::{Json, extract::State};
use serde_json::{Value, json};

async fn models_payload(state: &AppState) -> AppResult<Value> {
    let mut data = serde_json::to_value(default_models().data)?
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for combo in db::list_model_combos(&state.pool).await? {
        if !combo.is_active {
            continue;
        }
        let targets: Vec<&str> = combo.targets.iter().map(|t| t.model.as_str()).collect();
        data.push(json!({
            "id": combo.id,
            "object": "model",
            "owned_by": "combo",
            "display_name": combo.name,
            "reasoning": false,
            "vision": false,
            "is_default": false,
            "targets": targets,
        }));
    }
    let mut seen_ids: std::collections::HashSet<String> = data
        .iter()
        .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect();
    for acc in db::list_accounts(&state.pool, Some("commandcode"), None, None).await? {
        if acc.is_active == 0 {
            continue;
        }
        let acc_data = acc.data_json();
        let meta: std::collections::HashMap<String, crate::providers::commandcode::CcModelInfo> =
            acc_data
                .get("modelMeta")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|m| {
                            serde_json::from_value::<
                                crate::providers::commandcode::CcModelInfo,
                            >(m.clone())
                            .ok()
                            .map(|info| (info.id.clone(), info))
                        })
                        .collect()
                })
                .unwrap_or_default();
        let models = acc_data.get("models").and_then(|v| v.as_array()).cloned();
        let Some(models) = models else { continue };
        for m in models {
            let Some(id) = m.as_str() else { continue };
            let full = format!("cmc/{id}");
            let info = meta.get(id);
            let max_input = info
                .and_then(|i| i.context_length)
                .map(crate::providers::commandcode::fmt_context_length);
            let display = info
                .and_then(|i| i.name.clone())
                .unwrap_or_else(|| id.to_string());
            if !seen_ids.insert(full.clone()) {
                // Static entry already listed: enrich it with live meta.
                if let Some(entry) = data.iter_mut().find(|e| e["id"] == full) {
                    if let Some(mi) = &max_input {
                        entry["max_input"] = json!(mi);
                    }
                    entry["display_name"] = json!(display);
                }
                continue;
            }
            data.push(json!({
                "id": full,
                "object": "model",
                "owned_by": "commandcode",
                "display_name": display,
                "max_input": max_input,
                "reasoning": true,
                "vision": true,
                "is_default": false,
            }));
        }
    }
    for acc in db::list_accounts(&state.pool, Some("byok"), None, None).await? {
        if acc.is_active == 0 {
            continue;
        }
        let email = match acc.email.as_deref().filter(|s| !s.trim().is_empty()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let slug_key = email.to_ascii_lowercase();
        if seen.contains(&slug_key) {
            continue;
        }
        let models = acc
            .data_json()
            .get("models")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if models.is_empty() {
            continue;
        }
        seen.insert(slug_key);
        for m in models {
            let Some(id) = m.as_str() else { continue };
            data.push(json!({
                "id": format!("{email}/{id}"),
                "object": "model",
                "owned_by": "byok",
                "display_name": id,
                "slug": email,
                "reasoning": false,
                "vision": false,
                "is_default": false,
            }));
        }
    }
    Ok(json!({ "object": "list", "data": data }))
}

pub async fn list_models(
    State(state): State<AppState>,
    _auth: PoolAuth,
) -> AppResult<Json<Value>> {
    Ok(Json(models_payload(&state).await?))
}

pub async fn list_models_admin(
    State(state): State<AppState>,
    _auth: AdminAuth,
) -> AppResult<Json<Value>> {
    Ok(Json(models_payload(&state).await?))
}
