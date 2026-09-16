use std::{path::PathBuf, sync::Arc};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use serde::Serialize;
use tokio::{fs, sync::Semaphore};
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    services::ServeDir,
};
use tracing::{error, info};
use uuid::Uuid;

use crate::{
    cleanup,
    config::Config,
    error::AppError,
    media::{MediaProvider, acquire_with_timeout, validate_media},
    models::{AcquisitionProgress, AcquisitionStage, HealthResponse, WatchQuery, valid_video_id},
    session::{PlaybackSession, PlaybackStatus, SessionManager, session_dir},
};

#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub sessions: SessionManager,
    pub provider: Arc<dyn MediaProvider>,
    pub acquisition_limit: Arc<Semaphore>,
}

impl AppState {
    pub fn new(config: Config, provider: Arc<dyn MediaProvider>) -> Self {
        let limit = config.max_concurrent_downloads;
        Self {
            config,
            sessions: SessionManager::default(),
            provider,
            acquisition_limit: Arc::new(Semaphore::new(limit)),
        }
    }
}

pub fn router(state: AppState) -> Router {
    let cors = cors_layer(&state.config.allowed_origins);
    Router::new()
        .route("/health", get(health))
        .route("/watch", get(watch))
        .route("/api/sessions/{id}", get(session_status))
        .route("/api/sessions/{id}/playing", post(mark_playing))
        .route("/api/sessions/{id}/completed", post(mark_completed))
        .route("/media/{id}", get(media))
        .nest_service("/static", ServeDir::new("static"))
        .layer(cors)
        .with_state(state)
}

fn cors_layer(configured_origins: &[String]) -> CorsLayer {
    let configured: Vec<HeaderValue> = configured_origins
        .iter()
        .filter_map(|origin| match origin.parse() {
            Ok(origin) => Some(origin),
            Err(error) => {
                tracing::warn!(%origin, %error, "ignoring invalid configured CORS origin");
                None
            }
        })
        .collect();

    CorsLayer::new()
        .allow_methods([Method::GET])
        .allow_origin(AllowOrigin::predicate(move |origin, _request| {
            let extension_origin = origin
                .to_str()
                .map(|origin| {
                    origin.starts_with("moz-extension://")
                        || origin.starts_with("chrome-extension://")
                })
                .unwrap_or(false);
            extension_origin || configured.contains(origin)
        }))
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn watch(
    State(state): State<AppState>,
    Query(query): Query<WatchQuery>,
) -> Result<Html<String>, AppError> {
    if !valid_video_id(&query.v) {
        return Err(AppError::InvalidVideoId);
    }
    info!(video_id = %query.v, "playback request received");
    let metadata = state.provider.metadata(&query.v).await?;
    let session = state
        .sessions
        .create(query.v.clone(), metadata.clone())
        .await;
    let id = session.id;
    info!(session_id = %id, video_id = %query.v, "created playback session");
    spawn_acquisition(state.clone(), id);

    let template = include_str!("../templates/player.html");
    let duration = metadata
        .duration_seconds
        .map(format_duration)
        .unwrap_or_else(|| "Unknown".into());
    let html = template
        .replace("{{TITLE}}", &escape_html(&metadata.title))
        .replace("{{SESSION_ID}}", &id.to_string())
        .replace(
            "{{THUMBNAIL}}",
            metadata.thumbnail_url.as_deref().unwrap_or(""),
        )
        .replace("{{DURATION}}", &duration)
        .replace(
            "{{DESCRIPTION}}",
            &escape_html(metadata.description.as_deref().unwrap_or("")),
        );
    Ok(Html(html))
}

fn spawn_acquisition(state: AppState, id: Uuid) {
    tokio::spawn(async move {
        let permit = match state.acquisition_limit.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => return,
        };
        let session = match state
            .sessions
            .update(id, |session| session.status = PlaybackStatus::Downloading)
            .await
        {
            Ok(session) => session,
            Err(_) => return,
        };
        info!(session_id = %id, "acquiring media");
        let directory = session_dir(&state.config.cache_dir, id);
        let output = directory.join("video.mp4");
        let result = async {
            fs::create_dir_all(&directory)
                .await
                .map_err(AppError::internal)?;
            let started = tokio::time::Instant::now();
            let (progress_tx, mut progress_rx) =
                tokio::sync::mpsc::unbounded_channel::<AcquisitionProgress>();
            let progress_sessions = state.sessions.clone();
            tokio::spawn(async move {
                while let Some(progress) = progress_rx.recv().await {
                    let _ = progress_sessions
                        .update(id, |session| {
                            if matches!(
                                session.status,
                                PlaybackStatus::Downloading | PlaybackStatus::Processing
                            ) {
                                session.status = match progress.stage {
                                    AcquisitionStage::Downloading => PlaybackStatus::Downloading,
                                    AcquisitionStage::Processing => PlaybackStatus::Processing,
                                };
                                session.progress = Some(progress);
                            }
                        })
                        .await;
                }
            });
            acquire_with_timeout(
                state.provider.as_ref(),
                &session.video_id,
                &output,
                state.config.acquisition_timeout,
                Some(progress_tx),
            )
            .await?;
            let _ = state
                .sessions
                .update(id, |session| {
                    session.status = PlaybackStatus::Processing;
                    session.progress = Some(AcquisitionProgress::processing());
                })
                .await;
            let remaining = state
                .config
                .acquisition_timeout
                .checked_sub(started.elapsed())
                .ok_or(AppError::MediaAcquisitionTimeout)?;
            tokio::time::timeout(remaining, validate_media(&output))
                .await
                .map_err(|_| AppError::MediaAcquisitionTimeout)??;
            Ok::<(), AppError>(())
        }
        .await;
        drop(permit);

        match result {
            Ok(()) => {
                let _ = state
                    .sessions
                    .update(id, |session| {
                        session.status = PlaybackStatus::Ready;
                        session.media_path = Some(output);
                        session.progress = None;
                    })
                    .await;
                info!(session_id = %id, "media acquisition completed");
            }
            Err(err) => {
                error!(session_id = %id, error = %err, "media acquisition failed");
                let _ = fs::remove_dir_all(&directory).await;
                let _ = state
                    .sessions
                    .update(id, |session| {
                        session.status = PlaybackStatus::Failed;
                        session.error = Some("The media could not be acquired.".into());
                        session.media_path = None;
                        session.progress = None;
                    })
                    .await;
            }
        }
    });
}

#[derive(Serialize)]
struct SessionResponse {
    id: Uuid,
    video_id: String,
    title: String,
    status: PlaybackStatus,
    status_label: &'static str,
    created_at: chrono::DateTime<Utc>,
    last_activity: chrono::DateTime<Utc>,
    progress: Option<AcquisitionProgress>,
    error: Option<String>,
}

impl From<PlaybackSession> for SessionResponse {
    fn from(session: PlaybackSession) -> Self {
        Self {
            id: session.id,
            video_id: session.video_id,
            title: session.metadata.title,
            status: session.status,
            status_label: status_label(session.status),
            created_at: session.created_at,
            last_activity: session.last_activity,
            progress: session.progress,
            error: session.error,
        }
    }
}

async fn session_status(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<SessionResponse>, AppError> {
    let session = state.sessions.update(id, |_| {}).await?;
    Ok(Json(session.into()))
}

async fn mark_playing(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    state
        .sessions
        .update(id, |session| {
            if session.status == PlaybackStatus::Ready {
                session.status = PlaybackStatus::Playing;
            }
        })
        .await?;
    info!(session_id = %id, "playback started");
    Ok(StatusCode::NO_CONTENT)
}

async fn mark_completed(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    state
        .sessions
        .update(id, |session| session.status = PlaybackStatus::Completed)
        .await?;
    info!(session_id = %id, "playback completed");
    cleanup::remove_session(&state.config.cache_dir, &state.sessions, id).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn media(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let session = state.sessions.get(id).await?;
    if !matches!(
        session.status,
        PlaybackStatus::Ready | PlaybackStatus::Playing
    ) {
        return Err(AppError::MediaNotReady);
    }
    let path = session.media_path.ok_or(AppError::MediaNotReady)?;
    let metadata = fs::metadata(&path).await.map_err(AppError::internal)?;
    let length = metadata.len();
    let range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok());
    let (start, end, status) = match range.and_then(|value| parse_range(value, length)) {
        Some((start, end)) => (start, end, StatusCode::PARTIAL_CONTENT),
        None => (0, length.saturating_sub(1), StatusCode::OK),
    };
    let body = read_range(&path, start, end).await?;
    let mut response = (status, body).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, "video/mp4".parse().unwrap());
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        (end - start + 1).to_string().parse().unwrap(),
    );
    if status == StatusCode::PARTIAL_CONTENT {
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{length}").parse().unwrap(),
        );
    }
    Ok(response)
}

async fn read_range(path: &PathBuf, start: u64, end: u64) -> Result<Vec<u8>, AppError> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut file = fs::File::open(path).await.map_err(AppError::internal)?;
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(AppError::internal)?;
    let mut bytes = vec![0; (end - start + 1) as usize];
    file.read_exact(&mut bytes)
        .await
        .map_err(AppError::internal)?;
    Ok(bytes)
}

fn parse_range(value: &str, length: u64) -> Option<(u64, u64)> {
    let value = value.strip_prefix("bytes=")?;
    if value.contains(',') || length == 0 {
        return None;
    }
    let (start, end) = value.split_once('-')?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().ok()?.min(length);
        return Some((length - suffix, length - 1));
    }
    let start = start.parse::<u64>().ok()?;
    if start >= length {
        return None;
    }
    let end = if end.is_empty() {
        length - 1
    } else {
        end.parse::<u64>().ok()?.min(length - 1)
    };
    (start <= end).then_some((start, end))
}

fn format_duration(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn status_label(status: PlaybackStatus) -> &'static str {
    match status {
        PlaybackStatus::Queued => "Queued",
        PlaybackStatus::Downloading => "Downloading",
        PlaybackStatus::Processing => "Processing",
        PlaybackStatus::Ready => "Ready",
        PlaybackStatus::Playing => "Playing",
        PlaybackStatus::Completed => "Completed",
        PlaybackStatus::Failed => "Failed",
        PlaybackStatus::Expired => "Expired",
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tempfile::TempDir;
    use tower::ServiceExt;

    use super::*;
    use crate::models::VideoMetadata;

    struct TestProvider;
    #[async_trait]
    impl MediaProvider for TestProvider {
        async fn metadata(&self, id: &str) -> Result<VideoMetadata, AppError> {
            if id == "missing" {
                return Err(AppError::MetadataUnavailable);
            }
            Ok(VideoMetadata {
                id: id.into(),
                title: "Test Video".into(),
                description: None,
                thumbnail_url: None,
                duration_seconds: Some(60),
            })
        }
        async fn acquire(
            &self,
            _id: &str,
            output: &std::path::Path,
            _progress: Option<tokio::sync::mpsc::UnboundedSender<AcquisitionProgress>>,
        ) -> Result<(), AppError> {
            tokio::fs::write(output, b"test video bytes")
                .await
                .map_err(AppError::internal)
        }
    }

    fn state(temp: &TempDir) -> AppState {
        let config = Config {
            host: "127.0.0.1".parse().unwrap(),
            port: 6969,
            cache_dir: temp.path().join("cache"),
            media_dir: temp.path().join("media"),
            session_timeout: std::time::Duration::from_secs(30),
            cleanup_interval: std::time::Duration::from_secs(5),
            max_concurrent_downloads: 1,
            allowed_origins: Vec::new(),
            media_provider: crate::config::MediaProviderKind::Local,
            fixture_base_url: "http://127.0.0.1:6970".into(),
            ytdlp_path: "yt-dlp".into(),
            youtube_base_url: "https://www.youtube.com/watch?v={video_id}".into(),
            ytdlp_cookies_from_browser: None,
            ytdlp_cookies_file: None,
            ytdlp_js_runtime: "node".into(),
            acquisition_timeout: std::time::Duration::from_secs(30),
            fixture_connect_timeout: std::time::Duration::from_secs(1),
        };
        AppState::new(config, Arc::new(TestProvider))
    }

    #[tokio::test]
    async fn health_is_json() {
        let temp = tempfile::tempdir().unwrap();
        let response = router(state(&temp))
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            r#"{"status":"ok"}"#
        );
    }

    #[tokio::test]
    async fn health_allows_firefox_extension_origin() {
        let temp = tempfile::tempdir().unwrap();
        let response = router(state(&temp))
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .header(header::ORIGIN, "moz-extension://development-uuid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static(
                "moz-extension://development-uuid"
            ))
        );
    }

    #[tokio::test]
    async fn health_allows_chromium_extension_origin() {
        let temp = tempfile::tempdir().unwrap();
        let response = router(state(&temp))
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .header(header::ORIGIN, "chrome-extension://abcdefghijklmnop")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static(
                "chrome-extension://abcdefghijklmnop"
            ))
        );
    }

    #[tokio::test]
    async fn health_denies_arbitrary_web_origin() {
        let temp = tempfile::tempdir().unwrap();
        let response = router(state(&temp))
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .header(header::ORIGIN, "https://evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none()
        );
    }

    #[tokio::test]
    async fn health_allows_explicitly_configured_origin() {
        let temp = tempfile::tempdir().unwrap();
        let mut state = state(&temp);
        state.config.allowed_origins = vec!["https://trusted.example".into()];
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .header(header::ORIGIN, "https://trusted.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("https://trusted.example"))
        );
    }

    #[tokio::test]
    async fn watch_rejects_traversal() {
        let temp = tempfile::tempdir().unwrap();
        let response = router(state(&temp))
            .oneshot(
                Request::builder()
                    .uri("/watch?v=..%2Fsecret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn watch_creates_player_session() {
        let temp = tempfile::tempdir().unwrap();
        let response = router(state(&temp))
            .oneshot(
                Request::builder()
                    .uri("/watch?v=demo")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&body).contains("Test Video"));
    }

    #[tokio::test]
    async fn unknown_session_cannot_access_media() {
        let temp = tempfile::tempdir().unwrap();
        let uri = format!("/media/{}", Uuid::new_v4());
        let response = router(state(&temp))
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn sessions_only_serve_their_own_paths() {
        let temp = tempfile::tempdir().unwrap();
        let state = state(&temp);
        let first = state
            .sessions
            .create("one".into(), TestProvider.metadata("one").await.unwrap())
            .await;
        let second = state
            .sessions
            .create("two".into(), TestProvider.metadata("two").await.unwrap())
            .await;
        let first_dir = session_dir(&state.config.cache_dir, first.id);
        let second_dir = session_dir(&state.config.cache_dir, second.id);
        tokio::fs::create_dir_all(&first_dir).await.unwrap();
        tokio::fs::create_dir_all(&second_dir).await.unwrap();
        let first_path = first_dir.join("video.mp4");
        let second_path = second_dir.join("video.mp4");
        tokio::fs::write(&first_path, b"first").await.unwrap();
        tokio::fs::write(&second_path, b"second").await.unwrap();
        state
            .sessions
            .update(first.id, |s| {
                s.status = PlaybackStatus::Ready;
                s.media_path = Some(first_path);
            })
            .await
            .unwrap();
        state
            .sessions
            .update(second.id, |s| {
                s.status = PlaybackStatus::Ready;
                s.media_path = Some(second_path);
            })
            .await
            .unwrap();
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri(format!("/media/{}", first.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "first"
        );
    }

    #[test]
    fn parses_byte_ranges() {
        assert_eq!(parse_range("bytes=2-4", 10), Some((2, 4)));
        assert_eq!(parse_range("bytes=5-", 10), Some((5, 9)));
        assert_eq!(parse_range("bytes=-3", 10), Some((7, 9)));
    }
}
