use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;
use tokio::{
    fs,
    io::AsyncWriteExt,
    process::Command,
    sync::mpsc,
    time::{self, timeout},
};

use crate::{
    error::AppError,
    models::{AcquisitionProgress, VideoMetadata, valid_video_id},
};

#[async_trait]
pub trait MediaProvider: Send + Sync {
    async fn metadata(&self, video_id: &str) -> Result<VideoMetadata, AppError>;
    async fn acquire(
        &self,
        video_id: &str,
        output: &Path,
        progress: Option<mpsc::UnboundedSender<AcquisitionProgress>>,
    ) -> Result<(), AppError>;
}

#[derive(Clone, Debug)]
pub struct LocalFileProvider {
    root: PathBuf,
}

impl LocalFileProvider {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    async fn source(&self, video_id: &str) -> Result<PathBuf, AppError> {
        for extension in ["mp4", "webm", "mkv", "mov"] {
            let candidate = self.root.join(format!("{video_id}.{extension}"));
            if fs::metadata(&candidate)
                .await
                .map(|metadata| metadata.is_file())
                .unwrap_or(false)
            {
                return Ok(candidate);
            }
        }
        Err(AppError::MetadataUnavailable)
    }
}

#[async_trait]
impl MediaProvider for LocalFileProvider {
    async fn metadata(&self, video_id: &str) -> Result<VideoMetadata, AppError> {
        let source = self.source(video_id).await?;
        let probe = probe_media(&source).await.map_err(|error| {
            tracing::error!(video_id, %error, "ffprobe metadata inspection failed");
            AppError::MetadataUnavailable
        })?;
        let duration_seconds = probe.duration_seconds();
        let tags = probe.format.and_then(|format| format.tags);
        let title = tags
            .as_ref()
            .and_then(|tags| tags.title.clone())
            .unwrap_or_else(|| video_id.replace(['-', '_'], " "));

        Ok(VideoMetadata {
            id: video_id.to_owned(),
            title,
            description: tags.and_then(|tags| tags.comment),
            thumbnail_url: None,
            duration_seconds,
        })
    }

    async fn acquire(
        &self,
        video_id: &str,
        output: &Path,
        _progress: Option<mpsc::UnboundedSender<AcquisitionProgress>>,
    ) -> Result<(), AppError> {
        let source = self.source(video_id).await?;
        let result = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(&source)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "0:a:0?",
                "-c",
                "copy",
                "-movflags",
                "+faststart",
            ])
            .arg(output)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|error| {
                AppError::MediaAcquisitionFailed(format!("unable to start ffmpeg: {error}"))
            })?;

        if !result.status.success() {
            let diagnostic = String::from_utf8_lossy(&result.stderr);
            tracing::error!(video_id, stderr = %diagnostic, "ffmpeg acquisition failed");
            return Err(AppError::MediaAcquisitionFailed(
                "ffmpeg exited unsuccessfully".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct HttpFixtureProvider {
    client: Client,
    base_url: Url,
    request_timeout: Duration,
}

impl HttpFixtureProvider {
    pub fn new(
        base_url: &str,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Self, AppError> {
        let mut base_url = Url::parse(base_url).map_err(|error| {
            AppError::Configuration(format!("invalid fixture base URL: {error}"))
        })?;
        if !matches!(base_url.scheme(), "http" | "https") || base_url.host_str().is_none() {
            return Err(AppError::Configuration(
                "fixture base URL must be an absolute HTTP(S) URL".into(),
            ));
        }
        base_url.set_query(None);
        base_url.set_fragment(None);
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }
        let client = Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                AppError::Configuration(format!("invalid HTTP client configuration: {error}"))
            })?;
        Ok(Self {
            client,
            base_url,
            request_timeout,
        })
    }

    fn endpoint(&self, resource: &str, video_id: &str) -> Result<Url, AppError> {
        if !valid_video_id(video_id) {
            return Err(AppError::InvalidVideoId);
        }
        self.base_url
            .join(&format!("{resource}/{video_id}"))
            .map_err(|error| {
                AppError::Internal(format!("unable to construct fixture URL: {error}"))
            })
    }

    async fn remove_partial(output: &Path) {
        if let Err(error) = fs::remove_file(output).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %output.display(), %error, "failed to remove partial media");
        }
    }
}

#[async_trait]
impl MediaProvider for HttpFixtureProvider {
    async fn metadata(&self, video_id: &str) -> Result<VideoMetadata, AppError> {
        let url = self.endpoint("metadata", video_id)?;
        let response = self.client.get(url.clone()).send().await.map_err(|error| {
            if error.is_timeout() {
                tracing::error!(video_id, endpoint = %url, %error, "fixture metadata request timed out");
                AppError::MediaAcquisitionTimeout
            } else {
                tracing::error!(video_id, endpoint = %url, %error, "fixture metadata request failed");
                AppError::MetadataUnavailable
            }
        })?;
        if response.status() == StatusCode::NOT_FOUND {
            tracing::warn!(video_id, endpoint = %url, "fixture metadata was not found");
            return Err(AppError::MetadataUnavailable);
        }
        if !response.status().is_success() {
            tracing::error!(video_id, endpoint = %url, status = %response.status(), "fixture metadata request failed");
            return Err(AppError::MetadataUnavailable);
        }
        let metadata: VideoMetadata = response.json().await.map_err(|error| {
            tracing::error!(video_id, endpoint = %url, %error, "fixture metadata response was invalid");
            AppError::MetadataUnavailable
        })?;
        if metadata.id != video_id || metadata.title.trim().is_empty() {
            tracing::error!(video_id, returned_id = %metadata.id, endpoint = %url, "fixture metadata did not match the request");
            return Err(AppError::MetadataUnavailable);
        }
        Ok(metadata)
    }

    async fn acquire(
        &self,
        video_id: &str,
        output: &Path,
        progress: Option<mpsc::UnboundedSender<AcquisitionProgress>>,
    ) -> Result<(), AppError> {
        let result = timeout(self.request_timeout, async {
            let url = self.endpoint("media", video_id)?;
            let response = self.client.get(url).send().await.map_err(|error| {
                if error.is_timeout() {
                    AppError::MediaAcquisitionTimeout
                } else {
                    AppError::MediaAcquisitionFailed("fixture media request failed".into())
                }
            })?;
            if response.status() == StatusCode::NOT_FOUND {
                return Err(AppError::MediaAcquisitionFailed(
                    "fixture media is unavailable".into(),
                ));
            }
            if !response.status().is_success() {
                tracing::error!(status = %response.status(), "fixture media request failed");
                return Err(AppError::MediaAcquisitionFailed(
                    "fixture server returned an unsuccessful response".into(),
                ));
            }

            let expected = response.content_length();
            let mut stream = response.bytes_stream();
            let mut file = fs::File::create(output).await.map_err(AppError::internal)?;
            let mut downloaded = 0_u64;
            let mut next_log_percent = 10_u64;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|error| {
                    AppError::MediaAcquisitionFailed(format!(
                        "fixture media stream failed: {error}"
                    ))
                })?;
                file.write_all(&chunk).await.map_err(AppError::internal)?;
                downloaded += chunk.len() as u64;
                if let Some(progress) = progress.as_ref() {
                    let _ = progress.send(AcquisitionProgress::downloading(downloaded, expected));
                }
                if let Some(total) = expected.filter(|total| *total > 0) {
                    let percent = downloaded.saturating_mul(100) / total;
                    if percent >= next_log_percent {
                        tracing::debug!(
                            video_id,
                            downloaded,
                            total,
                            percent,
                            "fixture download progress"
                        );
                        next_log_percent = (percent / 10 + 1) * 10;
                    }
                }
            }
            file.flush().await.map_err(AppError::internal)?;
            drop(file);

            if downloaded == 0 {
                return Err(AppError::MediaProcessingFailed(
                    "downloaded media is empty".into(),
                ));
            }
            Ok(())
        })
        .await
        .map_err(|_| AppError::MediaAcquisitionTimeout)
        .and_then(|result| result);

        if result.is_err() {
            Self::remove_partial(output).await;
        }
        result
    }
}

#[derive(Debug, Deserialize)]
struct ProbeResult {
    format: Option<ProbeFormat>,
    #[serde(default)]
    streams: Vec<ProbeStream>,
}

impl ProbeResult {
    fn duration_seconds(&self) -> Option<u64> {
        self.format
            .as_ref()
            .and_then(|format| format.duration.as_deref())
            .and_then(|duration| duration.parse::<f64>().ok())
            .map(|duration| duration.ceil() as u64)
    }

    fn has_video(&self) -> bool {
        self.streams
            .iter()
            .any(|stream| stream.codec_type == "video")
    }
}

#[derive(Debug, Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
    tags: Option<ProbeTags>,
}

#[derive(Debug, Deserialize)]
struct ProbeTags {
    title: Option<String>,
    comment: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProbeStream {
    codec_type: String,
}

async fn probe_media(path: &Path) -> Result<ProbeResult, AppError> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_format",
            "-show_streams",
            "-of",
            "json",
        ])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|error| {
            AppError::MediaProcessingFailed(format!("unable to start ffprobe: {error}"))
        })?;
    if !output.status.success() {
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        tracing::error!(stderr = %diagnostic, "ffprobe validation failed");
        return Err(AppError::MediaProcessingFailed(
            "ffprobe rejected the acquired media".into(),
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        AppError::MediaProcessingFailed(format!("invalid ffprobe output: {error}"))
    })
}

pub async fn validate_media(path: &Path) -> Result<(), AppError> {
    let metadata = fs::metadata(path).await.map_err(|error| {
        AppError::MediaProcessingFailed(format!("media is unreadable: {error}"))
    })?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(AppError::MediaProcessingFailed(
            "media file is missing or empty".into(),
        ));
    }
    let probe = probe_media(path).await?;
    if !probe.has_video() {
        return Err(AppError::UnsupportedMedia);
    }
    Ok(())
}

pub async fn acquire_with_timeout(
    provider: &dyn MediaProvider,
    video_id: &str,
    output: &Path,
    duration: Duration,
    progress: Option<mpsc::UnboundedSender<AcquisitionProgress>>,
) -> Result<(), AppError> {
    let result = match time::timeout(duration, provider.acquire(video_id, output, progress)).await {
        Ok(result) => result,
        Err(_) => Err(AppError::MediaAcquisitionTimeout),
    };
    if result.is_err() {
        let _ = fs::remove_file(output).await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_only_safe_fixture_paths() {
        let provider = HttpFixtureProvider::new(
            "http://127.0.0.1:6970/api/",
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(
            provider.endpoint("metadata", "demo-1").unwrap().as_str(),
            "http://127.0.0.1:6970/api/metadata/demo-1"
        );
        assert!(provider.endpoint("media", "../secret").is_err());
        assert!(provider.endpoint("media", "https://evil.example").is_err());
    }

    async fn test_server() -> String {
        use axum::{
            Json, Router, body::Body, extract::Path as AxumPath, http::StatusCode,
            response::IntoResponse, routing::get,
        };

        async fn metadata(AxumPath(id): AxumPath<String>) -> axum::response::Response {
            match id.as_str() {
                "demo" => Json(VideoMetadata {
                    id,
                    title: "Fixture".into(),
                    description: None,
                    thumbnail_url: None,
                    duration_seconds: Some(1),
                })
                .into_response(),
                "bad-id" => Json(VideoMetadata {
                    id: "other".into(),
                    title: "Fixture".into(),
                    description: None,
                    thumbnail_url: None,
                    duration_seconds: None,
                })
                .into_response(),
                "invalid-json" => ([("content-type", "application/json")], "{").into_response(),
                "server-error" => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                "timeout" => {
                    time::sleep(Duration::from_secs(1)).await;
                    StatusCode::OK.into_response()
                }
                _ => StatusCode::NOT_FOUND.into_response(),
            }
        }

        async fn media(AxumPath(id): AxumPath<String>) -> axum::response::Response {
            match id.as_str() {
                "demo" => Body::from(vec![1_u8, 2, 3, 4]).into_response(),
                "empty" => Body::empty().into_response(),
                "server-error" => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                "timeout" => {
                    time::sleep(Duration::from_secs(1)).await;
                    Body::empty().into_response()
                }
                _ => StatusCode::NOT_FOUND.into_response(),
            }
        }

        let app = Router::new()
            .route("/metadata/{id}", get(metadata))
            .route("/media/{id}", get(media));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn parses_fixture_metadata() {
        let base = test_server().await;
        let provider =
            HttpFixtureProvider::new(&base, Duration::from_secs(1), Duration::from_secs(1))
                .unwrap();
        let metadata = provider.metadata("demo").await.unwrap();
        assert_eq!(metadata.id, "demo");
        assert_eq!(metadata.title, "Fixture");
    }

    #[tokio::test]
    async fn maps_fixture_metadata_failures() {
        let base = test_server().await;
        let provider =
            HttpFixtureProvider::new(&base, Duration::from_millis(100), Duration::from_millis(10))
                .unwrap();
        for id in ["missing", "invalid-json", "server-error", "bad-id"] {
            assert!(matches!(
                provider.metadata(id).await,
                Err(AppError::MetadataUnavailable)
            ));
        }
        assert!(matches!(
            provider.metadata("timeout").await,
            Err(AppError::MediaAcquisitionTimeout)
        ));
    }

    #[tokio::test]
    async fn streams_media_and_cleans_failed_downloads() {
        let base = test_server().await;
        let provider =
            HttpFixtureProvider::new(&base, Duration::from_secs(1), Duration::from_millis(10))
                .unwrap();
        let directory = tempfile::tempdir().unwrap();

        let downloaded = directory.path().join("downloaded.mp4");
        provider.acquire("demo", &downloaded, None).await.unwrap();
        assert_eq!(fs::read(&downloaded).await.unwrap(), [1_u8, 2, 3, 4]);

        for id in ["missing", "empty", "server-error", "timeout"] {
            let output = directory.path().join(format!("{id}.mp4"));
            assert!(provider.acquire(id, &output, None).await.is_err());
            assert!(!output.exists(), "failed download must be removed");
        }
    }

    #[tokio::test]
    async fn rejects_empty_media_before_probing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("empty.mp4");
        fs::write(&path, []).await.unwrap();
        assert!(matches!(
            validate_media(&path).await,
            Err(AppError::MediaProcessingFailed(_))
        ));
    }

    struct SlowProvider;

    #[async_trait]
    impl MediaProvider for SlowProvider {
        async fn metadata(&self, _video_id: &str) -> Result<VideoMetadata, AppError> {
            unreachable!()
        }

        async fn acquire(
            &self,
            _video_id: &str,
            _output: &Path,
            _progress: Option<mpsc::UnboundedSender<AcquisitionProgress>>,
        ) -> Result<(), AppError> {
            time::sleep(Duration::from_secs(1)).await;
            Ok(())
        }
    }

    #[tokio::test]
    async fn acquisition_timeout_is_enforced() {
        let error = acquire_with_timeout(
            &SlowProvider,
            "demo",
            Path::new("unused.mp4"),
            Duration::from_millis(1),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, AppError::MediaAcquisitionTimeout));
    }
}
