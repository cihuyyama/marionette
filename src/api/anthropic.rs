use crate::anthropic::{MessagesRequest, collect_stream, from_chat_json, translate_stream, translate_stream_from_json};
use crate::auth::PoolAuth;
use crate::error::AppResult;
use crate::pool;
use crate::providers::ChatOutcome;
use crate::state::AppState;
use axum::{Json, extract::State, response::IntoResponse};

pub async fn messages(
    State(state): State<AppState>,
    auth: PoolAuth,
    Json(req): Json<MessagesRequest>,
) -> AppResult<impl IntoResponse> {
    let model = req.model.clone();
    let want_stream = req.stream_enabled();
    let chat = req.to_chat_request()?;

    // Everything below is the ordinary pool path — account selection,
    // failover, quota, and request logging behave exactly as they do for
    // /v1/chat/completions. Only the wire shape differs.
    match pool::handle_chat(&state, chat, auth.key_id).await? {
        ChatOutcome::Json(v) => {
            if want_stream {
                // A streaming client got a buffered result (provider does not
                // stream): emit it as one complete event sequence rather than
                // a bare JSON body, or the client would fail to parse it.
                return Ok(translate_stream_from_json(&v, &model));
            }
            Ok(Json(from_chat_json(&v, &model)).into_response())
        }
        ChatOutcome::Stream { response, .. } => {
            if !want_stream {
                // Symmetric case: buffer the stream so a non-streaming client
                // gets the JSON body it asked for.
                return Ok(collect_stream(response, &model).await);
            }
            Ok(translate_stream(model, response))
        }
    }
}
