use crate::auth::PoolAuth;
use crate::error::AppResult;
use crate::pool;
use crate::providers::ChatOutcome;
use crate::responses::{
    ResponsesRequest, collect_stream, from_chat_json, translate_stream,
    translate_stream_from_json,
};
use crate::state::AppState;
use axum::{Json, extract::State, response::IntoResponse};

pub async fn responses(
    State(state): State<AppState>,
    auth: PoolAuth,
    Json(req): Json<ResponsesRequest>,
) -> AppResult<impl IntoResponse> {
    let model = req.model.clone();
    let want_stream = req.stream_enabled();
    let chat = req.to_chat_request()?;

    // Same pool path as /v1/chat/completions — only the wire shape differs.
    match pool::handle_chat(&state, chat, auth.key_id).await? {
        ChatOutcome::Json(v) => {
            if want_stream {
                return Ok(translate_stream_from_json(&v, &model));
            }
            Ok(Json(from_chat_json(&v, &model)).into_response())
        }
        ChatOutcome::Stream { response, .. } => {
            if !want_stream {
                return Ok(collect_stream(response, &model).await);
            }
            Ok(translate_stream(model, response))
        }
    }
}
