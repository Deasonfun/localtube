use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("invalid video identifier")]
    InvalidVideoId,
    #[error("video metadata is unavailable")]
    MetadataUnavailable,
    #[error("media acquisition failed: {0}")]
    MediaAcquisitionFailed(String),
    #[error("media processing failed: {0}")]
    MediaProcessingFailed(String),
    #[error("media acquisition timed out")]
    MediaAcquisitionTimeout,
    #[error("yt-dlp executable was not found")]
    YtDlpNotInstalled,
    #[error("yt-dlp metadata failed: {0}")]
    YtDlpMetadataFailed(String),
    #[error("yt-dlp acquisition failed: {0}")]
    YtDlpAcquisitionFailed(String),
    #[error("yt-dlp execution failed: {0}")]
    YtDlpExecutionFailed(String),
    #[error("unsupported media")]
    UnsupportedMedia,
    #[error("playback session was not found")]
    SessionNotFound,
    #[error("media is not ready")]
    MediaNotReady,
    #[error("invalid application configuration: {0}")]
    Configuration(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl AppError {
    pub fn internal(error: impl std::fmt::Display) -> Self {
        Self::Internal(error.to_string())
    }

    fn public_message(&self) -> &'static str {
        match self {
            Self::InvalidVideoId => "The supplied video identifier is invalid.",
            Self::MetadataUnavailable => "No authorized media matched that video identifier.",
            Self::MediaAcquisitionFailed(_)
            | Self::MediaProcessingFailed(_)
            | Self::MediaAcquisitionTimeout
            | Self::YtDlpMetadataFailed(_)
            | Self::YtDlpAcquisitionFailed(_)
            | Self::YtDlpExecutionFailed(_)
            | Self::UnsupportedMedia => "The media could not be acquired.",
            Self::YtDlpNotInstalled => "The yt-dlp executable is not available on this system.",
            Self::SessionNotFound => "The playback session does not exist or has expired.",
            Self::MediaNotReady => "The media is still being prepared.",
            Self::Configuration(_) | Self::Internal(_) => "An internal error occurred.",
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::InvalidVideoId => StatusCode::BAD_REQUEST,
            Self::MetadataUnavailable | Self::SessionNotFound => StatusCode::NOT_FOUND,
            Self::MediaNotReady => StatusCode::CONFLICT,
            Self::MediaAcquisitionFailed(_)
            | Self::MediaProcessingFailed(_)
            | Self::MediaAcquisitionTimeout
            | Self::YtDlpMetadataFailed(_)
            | Self::YtDlpAcquisitionFailed(_)
            | Self::UnsupportedMedia => StatusCode::UNPROCESSABLE_ENTITY,
            Self::YtDlpExecutionFailed(_) | Self::YtDlpNotInstalled => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            Self::Configuration(_) | Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        tracing::error!(error = %self, "request failed");
        let html = format!(
            "<!doctype html><html><body><h1>Unable to prepare video</h1><p>{}</p></body></html>",
            self.public_message()
        );
        (status, Html(html)).into_response()
    }
}
