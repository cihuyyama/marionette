use crate::auth::PoolAuth;
use crate::error::{AppError, AppResult};
use crate::images::{
    DEFAULT_IMAGE_MODEL, ImageEditRequest, ImageGenerationRequest, ImageResponseFormat,
    collect_image_refs, image_provider_id, require_prompt,
};
use crate::pool;
use crate::state::AppState;
use axum::{Json, extract::State, response::IntoResponse};

// Image endpoints share the chat pre-flight: allowlist + request budget + RPM.
// Token budget decrement is not applicable here (image responses carry no
// usage object), so only the request counter is charged.
pub async fn images_generations(
    State(state): State<AppState>,
    auth: PoolAuth,
    Json(req): Json<ImageGenerationRequest>,
) -> AppResult<impl IntoResponse> {
    let prompt = require_prompt(req.prompt.as_deref())
        .map_err(AppError::BadRequest)?;
    let model = req
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_IMAGE_MODEL)
        .to_string();
    if image_provider_id(&model).is_none() {
        return Err(AppError::BadRequest(format!(
            "unknown or unsupported image model: {model}"
        )));
    }
    pool::check_and_register_key(&state, auth.key_id.as_deref(), &model).await?;
    let refs = collect_image_refs(req.image.as_ref(), req.images.as_ref());
    let format = ImageResponseFormat::parse(req.response_format.as_deref());
    let n = req.n.unwrap_or(1).clamp(1, 4);
    let body = pool::handle_image(
        &state,
        pool::ImageJob {
            model,
            prompt,
            refs,
            format,
            n,
            require_refs: false,
        },
        auth.key_id,
    )
    .await?;
    Ok(Json(body).into_response())
}

pub async fn images_edits(
    State(state): State<AppState>,
    auth: PoolAuth,
    Json(req): Json<ImageEditRequest>,
) -> AppResult<impl IntoResponse> {
    let prompt = require_prompt(req.prompt.as_deref())
        .map_err(AppError::BadRequest)?;
    let model = req
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("grok-imagine-image-edit")
        .to_string();
    if image_provider_id(&model).is_none() {
        return Err(AppError::BadRequest(format!(
            "unknown or unsupported image model: {model}"
        )));
    }
    pool::check_and_register_key(&state, auth.key_id.as_deref(), &model).await?;
    let mut refs = collect_image_refs(req.image.as_ref(), req.images.as_ref());
    // Optional mask as extra ref (best-effort; upstream tool may ignore).
    if let Some(mask) = req.mask.as_ref() {
        if let Some(s) = crate::images::normalize_image_ref(mask) {
            if !refs.iter().any(|e| e == &s) {
                refs.push(s);
            }
        }
    }
    if refs.is_empty() {
        return Err(AppError::BadRequest(
            "image edits require at least one image (image or images as string or {url})"
                .into(),
        ));
    }
    let format = ImageResponseFormat::parse(req.response_format.as_deref());
    let n = req.n.unwrap_or(1).clamp(1, 4);
    let body = pool::handle_image(
        &state,
        pool::ImageJob {
            model,
            prompt,
            refs,
            format,
            n,
            require_refs: true,
        },
        auth.key_id,
    )
    .await?;
    Ok(Json(body).into_response())
}
