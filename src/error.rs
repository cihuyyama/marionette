use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("no healthy accounts available for provider {0}")]
    NoAccounts(String),
    #[error("provider not implemented: {0}")]
    NotImplemented(String),
    #[error("upstream error ({status}): {body}")]
    Upstream { status: u16, body: String },
    #[error("provider: {0}")]
    Provider(String),
    #[error("API key {0}")]
    ApiKeyUnauthorized(String),
    #[error("API key {0}")]
    ApiKeyForbidden(String),
    #[error("API key {0}")]
    ApiKeyRateLimited(String),
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
    #[error("http client: {0}")]
    Http(#[from] reqwest::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("internal: {0}")]
    Internal(String),
}

impl AppError {
    pub fn status_code(&self) -> StatusCode {
        match self {
            AppError::Unauthorized | AppError::ApiKeyUnauthorized(_) => StatusCode::UNAUTHORIZED,
            AppError::Forbidden | AppError::ApiKeyForbidden(_) => StatusCode::FORBIDDEN,
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::NoAccounts(_) => StatusCode::SERVICE_UNAVAILABLE,
            AppError::NotImplemented(_) => StatusCode::NOT_IMPLEMENTED,
            AppError::Upstream { status, .. } => {
                StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
            }
            AppError::ApiKeyRateLimited(_) => StatusCode::TOO_MANY_REQUESTS,
            AppError::Provider(_) => StatusCode::BAD_GATEWAY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let message = self.to_string();
        let body = Json(json!({
            "error": {
                "message": message,
                "type": "marionette_error",
                "code": status.as_u16()
            }
        }));
        (status, body).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("auth expired")]
    AuthExpired,
    #[error("auth invalid: {0}")]
    AuthInvalid(String),
    #[error("rate limited")]
    RateLimited { retry_after_secs: Option<u64> },
    #[error("payment required")]
    PaymentRequired,
    #[error("access denied")]
    AccessDenied,
    #[error("upstream {status}: {body}")]
    Upstream { status: u16, body: String },
    #[error("transport: {0}")]
    Transport(String),
    #[error("other: {0}")]
    Other(String),
}

impl From<ProviderError> for AppError {
    fn from(e: ProviderError) -> Self {
        match e {
            ProviderError::AuthExpired | ProviderError::AuthInvalid(_) => {
                AppError::Provider(e.to_string())
            }
            ProviderError::RateLimited { .. } => AppError::Provider(e.to_string()),
            ProviderError::PaymentRequired => AppError::Provider(e.to_string()),
            ProviderError::AccessDenied => AppError::Provider(e.to_string()),
            ProviderError::Upstream { status, body } => AppError::Upstream { status, body },
            ProviderError::Transport(s) | ProviderError::Other(s) => AppError::Provider(s),
        }
    }
}
