use crate::auth::PoolAuth;
use crate::error::AppResult;
use crate::openai::ChatCompletionRequest;
use crate::pool;
use crate::providers::ChatOutcome;
use crate::state::AppState;
use axum::{Json, extract::State, response::IntoResponse};

pub async fn chat_completions(
    State(state): State<AppState>,
    _auth: PoolAuth,
    Json(req): Json<ChatCompletionRequest>,
) -> AppResult<impl IntoResponse> {
    if req.messages.is_empty() {
        return Err(crate::error::AppError::BadRequest(
            "messages must not be empty".into(),
        ));
    }
    match pool::handle_chat(&state, req).await? {
        ChatOutcome::Json(v) => Ok(Json(v).into_response()),
        ChatOutcome::Stream { response, .. } => Ok(response),
    }
}
