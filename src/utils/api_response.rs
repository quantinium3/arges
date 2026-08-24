use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

pub type ApiResult<T> = Result<ApiResponse<T>, ApiError>;

#[derive(Serialize)]
pub struct ApiResponse<T>
where
    T: Serialize,
{
    success: bool,
    message: String,
    data: T,
}

impl<T> ApiResponse<T>
where
    T: Serialize,
{
    pub fn ok(data: T, message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
            data,
        }
    }
}

impl<T> IntoResponse for ApiResponse<T>
where
    T: Serialize,
{
    fn into_response(self) -> Response {
        (StatusCode::OK, Json(self)).into_response()
    }
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("{0}")]
    NotFound(String),

    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl ApiError {
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound(message.into())
    }

    pub fn internal(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }

    fn status_code(&self) -> StatusCode {
        match self {
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            ApiError::NotFound(_) => "not_found",
            ApiError::Internal(_) => "internal",
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let code = self.code();

        let (message, error_id) = match &self {
            ApiError::Internal(error) => {
                let error_id = Uuid::new_v4();
                tracing::error!(%error_id, error = ?error, "internal server error");
                ("Internal server error".to_string(), Some(error_id))
            }
            other => (other.to_string(), None),
        };

        (
            status,
            Json(ApiErrorResponse {
                success: false,
                message,
                code,
                error_id,
                data: None,
            }),
        )
            .into_response()
    }
}

#[derive(Serialize)]
struct ApiErrorResponse {
    success: bool,
    message: String,
    code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_id: Option<Uuid>,
    data: Option<()>,
}
