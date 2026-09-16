use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use async_trait::async_trait;
use reqwest::Url;
use serde::Deserialize;
use tokio::{
    fs,
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::mpsc,
};

use crate::{
    error::AppError,
    models::{AcquisitionProgress, AcquisitionStage, VideoMetadata, valid_video_id},
};

use super::MediaProvider;

#[derive(Clone, Debug)]
pub struct YtDlpProvider {
    executable: PathBuf,
    youtube_base_url: String,
    trusted_scheme: String,
    trusted_host: String,
    cookies_from_browser: Option<String>,
    cookies_file: Option<PathBuf>,
    js_runtime: Option<String>,
}

#[derive(Debug, Deserialize)]
struct YtDlpMetadata {
    id: Option<String>,
    title: Option<String>,
    description: Option<String>,
    duration: Option<f64>,
    thumbnail: Option<String>,
}

impl YtDlpProvider {
    pub fn new(
        executable: PathBuf,
        youtube_base_url: String,
        cookies_from_browser: Option<String>,
        cookies_file: Option<PathBuf>,
        _request_timeout: Duration,
    ) -> Result<Self, AppError> {
        let youtube_base_url = if youtube_base_url.contains("{video_id}") {
            youtube_base_url
        } else {
            format!("{youtube_base_url}{{video_id}}")
        };

        let probe_url = youtube_base_url.replace("{video_id}", "demo");
        let parsed = Url::parse(&probe_url).map_err(|error| {
            AppError::Configuration(format!("invalid LOCALTUBE_YOUTUBE_BASE_URL: {error}"))
        })?;
        let scheme = parsed.scheme().to_string();
        let host = parsed
            .host_str()
            .ok_or_else(|| {
                AppError::Configuration("LOCALTUBE_YOUTUBE_BASE_URL must include a host".into())
            })?
            .to_string();
        if !matches!(scheme.as_str(), "http" | "https") {
            return Err(AppError::Configuration(
                "LOCALTUBE_YOUTUBE_BASE_URL must use http or https".into(),
            ));
        }

        if cookies_from_browser.is_some() && cookies_file.is_some() {
            return Err(AppError::Configuration(
                "set only one of yt-dlp cookies-from-browser or cookies file".into(),
            ));
        }

        Ok(Self {
            executable,
            youtube_base_url,
            trusted_scheme: scheme,
            trusted_host: host,
            cookies_from_browser,
            cookies_file,
            js_runtime: None,
        })
    }

    pub fn with_js_runtime(mut self, js_runtime: String) -> Self {
        self.js_runtime = Some(js_runtime);
        self
    }

    fn source_url(&self, video_id: &str) -> Result<String, AppError> {
        if !valid_video_id(video_id) {
            return Err(AppError::InvalidVideoId);
        }
        let source = self.youtube_base_url.replace("{video_id}", video_id);
        let parsed = Url::parse(&source).map_err(|error| {
            AppError::YtDlpMetadataFailed(format!("invalid source URL: {error}"))
        })?;
        if parsed.scheme() != self.trusted_scheme
            || parsed.host_str() != Some(self.trusted_host.as_str())
        {
            return Err(AppError::YtDlpMetadataFailed(
                "source URL host/scheme mismatch".into(),
            ));
        }
        Ok(source)
    }

    fn metadata_command(&self, source_url: &str) -> Command {
        let mut command = Command::new(&self.executable);
        command.args([
            "--ignore-config",
            "--dump-single-json",
            "--skip-download",
            "--ignore-no-formats-error",
            "--no-playlist",
        ]);
        self.apply_auth_args(&mut command);
        command
            .arg(source_url)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    fn acquisition_command(&self, source_url: &str, output: &Path) -> Command {
        let mut command = Command::new(&self.executable);
        command.args([
            "--ignore-config",
            "--no-playlist",
            "--newline",
            "--progress",
            "--progress-template",
            "download:localtube:download:%(progress)j",
            "--progress-template",
            "postprocess:localtube:processing:%(progress)j",
            "--no-part",
            "--merge-output-format",
            "mp4",
            "--remux-video",
            "mp4",
        ]);
        self.apply_auth_args(&mut command);
        command
            .arg("-o")
            .arg(output)
            .arg(source_url)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    fn apply_auth_args(&self, command: &mut Command) {
        if let Some(runtime) = self.js_runtime.as_deref() {
            command.arg("--js-runtimes").arg(runtime);
        }
        if let Some(source) = self.cookies_from_browser.as_deref() {
            command.arg("--cookies-from-browser").arg(source);
        }
        if let Some(file) = self.cookies_file.as_ref() {
            command.arg("--cookies").arg(file);
        }
    }

    async fn cleanup_partial_files(output: &Path) {
        let mut candidates = vec![output.to_path_buf()];
        if let Some(name) = output.file_name().and_then(|name| name.to_str()) {
            candidates.push(output.with_file_name(format!("{name}.part")));
            candidates.push(output.with_file_name(format!("{name}.ytdl")));
        }
        for candidate in candidates {
            if let Err(error) = fs::remove_file(&candidate).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = %candidate.display(), %error, "failed cleaning yt-dlp artifact");
            }
        }
    }
}

#[async_trait]
impl MediaProvider for YtDlpProvider {
    async fn metadata(&self, video_id: &str) -> Result<VideoMetadata, AppError> {
        let source_url = self.source_url(video_id)?;
        tracing::info!(video_id, "requesting yt-dlp metadata");
        let output = self
            .metadata_command(&source_url)
            .output()
            .await
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    AppError::YtDlpNotInstalled
                } else {
                    AppError::YtDlpExecutionFailed(format!("unable to execute yt-dlp: {error}"))
                }
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!(video_id, stderr = %stderr, "yt-dlp metadata command failed");
            let normalized = stderr.to_ascii_lowercase();
            if normalized.contains("unsupported url")
                || normalized.contains("video unavailable")
                || normalized.contains("unable to extract")
            {
                return Err(AppError::MetadataUnavailable);
            }
            if normalized.contains("requested format is not available") {
                return Err(AppError::YtDlpMetadataFailed(
                    "yt-dlp format selection failed; ensure custom yt-dlp config is compatible"
                        .into(),
                ));
            }
            return Err(AppError::YtDlpMetadataFailed(
                "yt-dlp could not fetch metadata".into(),
            ));
        }

        let parsed: YtDlpMetadata = serde_json::from_slice(&output.stdout).map_err(|error| {
            tracing::error!(video_id, %error, "invalid yt-dlp metadata JSON");
            AppError::YtDlpMetadataFailed("invalid yt-dlp metadata JSON".into())
        })?;

        let id = parsed.id.unwrap_or_else(|| video_id.to_string());
        let title = parsed
            .title
            .ok_or_else(|| AppError::YtDlpMetadataFailed("yt-dlp metadata missing title".into()))?;
        if id != video_id {
            return Err(AppError::YtDlpMetadataFailed(
                "yt-dlp metadata id mismatch".into(),
            ));
        }

        tracing::info!(video_id, "metadata acquired via yt-dlp");
        Ok(VideoMetadata {
            id,
            title,
            description: parsed.description,
            thumbnail_url: parsed.thumbnail,
            duration_seconds: parsed.duration.map(|value| value.max(0.0).round() as u64),
        })
    }

    async fn acquire(
        &self,
        video_id: &str,
        output: &Path,
        progress: Option<mpsc::UnboundedSender<AcquisitionProgress>>,
    ) -> Result<(), AppError> {
        let source_url = self.source_url(video_id)?;
        tracing::info!(video_id, path = %output.display(), "starting yt-dlp acquisition");

        let mut child = self
            .acquisition_command(&source_url, output)
            .spawn()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    AppError::YtDlpNotInstalled
                } else {
                    AppError::YtDlpExecutionFailed(format!("unable to execute yt-dlp: {error}"))
                }
            })?;

        // yt-dlp writes machine-readable progress to stdout; stderr only
        // carries warnings and errors. Both pipes must be drained so the
        // child never blocks on a full buffer.
        let stdout = child.stdout.take().ok_or_else(|| {
            AppError::YtDlpExecutionFailed("unable to capture yt-dlp stdout".into())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            AppError::YtDlpExecutionFailed("unable to capture yt-dlp stderr".into())
        })?;

        let progress_task = tokio::spawn(drain_output(stdout, progress));
        let stderr_task = tokio::spawn(drain_output(stderr, None));

        let status = child.wait().await.map_err(|error| {
            AppError::YtDlpExecutionFailed(format!("failed waiting for yt-dlp: {error}"))
        })?;
        let mut diagnostics = progress_task.await.unwrap_or_default();
        diagnostics.push_str(&stderr_task.await.unwrap_or_default());

        if !status.success() {
            tracing::error!(video_id, stderr = %diagnostics, "yt-dlp acquisition command failed");
            Self::cleanup_partial_files(output).await;
            return Err(AppError::YtDlpAcquisitionFailed(
                "yt-dlp acquisition failed".into(),
            ));
        }

        tracing::info!(video_id, "yt-dlp acquisition completed");
        Ok(())
    }
}

async fn drain_output<R>(
    read: R,
    progress: Option<mpsc::UnboundedSender<AcquisitionProgress>>,
) -> String
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut reader = BufReader::new(read).lines();
    let mut diagnostics = String::new();
    while let Ok(Some(line)) = reader.next_line().await {
        if let Some(event) = parse_progress_event(&line) {
            tracing::debug!(stage = ?event.stage, percent = ?event.percent, "yt-dlp progress");
            if let Some(progress) = progress.as_ref() {
                let _ = progress.send(event);
            }
        } else {
            tracing::debug!(line = %line, "yt-dlp");
        }
        if diagnostics.len() < 16_384 {
            diagnostics.push_str(&line);
            diagnostics.push('\n');
        }
    }
    diagnostics
}

fn parse_progress_event(line: &str) -> Option<AcquisitionProgress> {
    const DOWNLOAD_PREFIX: &str = "localtube:download:";
    const PROCESSING_PREFIX: &str = "localtube:processing:";

    let (stage, json) = if let Some(json) = line.strip_prefix(DOWNLOAD_PREFIX) {
        (AcquisitionStage::Downloading, json)
    } else if let Some(json) = line.strip_prefix(PROCESSING_PREFIX) {
        (AcquisitionStage::Processing, json)
    } else {
        return None;
    };

    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    if stage == AcquisitionStage::Processing {
        return Some(AcquisitionProgress::processing());
    }

    let downloaded_bytes = json_u64(&value, "downloaded_bytes");
    let total_bytes =
        json_u64(&value, "total_bytes").or_else(|| json_u64(&value, "total_bytes_estimate"));
    let percent = match (downloaded_bytes, total_bytes) {
        (Some(downloaded), Some(total)) if total > 0 => {
            Some((downloaded as f64 * 100.0 / total as f64).clamp(0.0, 100.0))
        }
        _ => None,
    };

    Some(AcquisitionProgress {
        stage,
        percent,
        downloaded_bytes,
        total_bytes,
        speed_bytes_per_second: json_f64(&value, "speed"),
        eta_seconds: json_u64(&value, "eta"),
    })
}

fn json_u64(value: &serde_json::Value, key: &str) -> Option<u64> {
    value.get(key)?.as_u64().or_else(|| {
        value
            .get(key)?
            .as_f64()
            .filter(|number| number.is_finite() && *number >= 0.0)
            .map(|number| number.round() as u64)
    })
}

fn json_f64(value: &serde_json::Value, key: &str) -> Option<f64> {
    value
        .get(key)?
        .as_f64()
        .filter(|number| number.is_finite() && *number >= 0.0)
}

#[cfg(test)]
mod tests {
    use std::{
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use tokio::fs;

    use super::*;

    async fn fake_ytdlp_script(contents: &str) -> PathBuf {
        let script = std::env::temp_dir().join(format!(
            "fake-ytdlp-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&script, contents).await.unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        script
    }

    #[test]
    fn parses_machine_readable_download_progress() {
        let progress = parse_progress_event(
            r#"localtube:download:{"downloaded_bytes":5242880,"total_bytes":10485760,"speed":2097152.5,"eta":3}"#,
        )
        .unwrap();
        assert_eq!(progress.stage, AcquisitionStage::Downloading);
        assert_eq!(progress.percent, Some(50.0));
        assert_eq!(progress.downloaded_bytes, Some(5_242_880));
        assert_eq!(progress.total_bytes, Some(10_485_760));
        assert_eq!(progress.speed_bytes_per_second, Some(2_097_152.5));
        assert_eq!(progress.eta_seconds, Some(3));
    }

    #[test]
    fn handles_unknown_total_and_estimated_total() {
        let unknown = parse_progress_event(
            r#"localtube:download:{"downloaded_bytes":1024,"total_bytes":null,"speed":512.0,"eta":null}"#,
        )
        .unwrap();
        assert_eq!(unknown.percent, None);
        assert_eq!(unknown.total_bytes, None);

        let estimated = parse_progress_event(
            r#"localtube:download:{"downloaded_bytes":512,"total_bytes_estimate":1024}"#,
        )
        .unwrap();
        assert_eq!(estimated.percent, Some(50.0));
        assert_eq!(estimated.total_bytes, Some(1024));
    }

    #[test]
    fn processing_event_clears_stale_download_values() {
        let progress =
            parse_progress_event(r#"localtube:processing:{"status":"started"}"#).unwrap();
        assert_eq!(progress, AcquisitionProgress::processing());
    }

    #[test]
    fn malformed_and_human_progress_are_ignored() {
        assert!(parse_progress_event("localtube:download:{bad json").is_none());
        assert!(parse_progress_event("[download] 43.2% of 10MiB at 2MiB/s ETA 00:03").is_none());
        assert!(parse_progress_event("unrelated diagnostic").is_none());
    }

    #[tokio::test]
    async fn source_url_uses_template_and_validated_id() {
        let provider = YtDlpProvider::new(
            PathBuf::from("yt-dlp"),
            "https://www.youtube.com/watch?v={video_id}".into(),
            None,
            None,
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(
            provider.source_url("abc123").unwrap(),
            "https://www.youtube.com/watch?v=abc123"
        );
        assert!(provider.source_url("../bad").is_err());

        let base_form = YtDlpProvider::new(
            PathBuf::from("yt-dlp"),
            "https://www.youtube.com/watch?v=".into(),
            None,
            None,
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(
            base_form.source_url("abc123").unwrap(),
            "https://www.youtube.com/watch?v=abc123"
        );
    }

    #[tokio::test]
    async fn metadata_parses_json_from_fake_executable() {
        let script = fake_ytdlp_script(
            "#!/bin/sh\nfound=0\nfor arg in \"$@\"; do\n  if [ \"$arg\" = \"--dump-single-json\" ]; then found=1; fi\ndone\nif [ \"$found\" = \"1\" ]; then\n  echo '{\"id\":\"demo\",\"title\":\"Demo\",\"duration\":5,\"thumbnail\":null}'\n  exit 0\nfi\nexit 1\n",
        )
        .await;
        let provider = YtDlpProvider::new(
            script,
            "https://www.youtube.com/watch?v={video_id}".into(),
            None,
            None,
            Duration::from_secs(5),
        )
        .unwrap();
        let metadata = provider.metadata("demo").await.unwrap();
        assert_eq!(metadata.title, "Demo");
        assert_eq!(metadata.duration_seconds, Some(5));
    }

    #[tokio::test]
    async fn metadata_maps_invalid_json() {
        let script = fake_ytdlp_script(
            "#!/bin/sh\nfound=0\nfor arg in \"$@\"; do\n  if [ \"$arg\" = \"--dump-single-json\" ]; then found=1; fi\ndone\nif [ \"$found\" = \"1\" ]; then\n  echo '{invalid'\n  exit 0\nfi\nexit 1\n",
        )
        .await;
        let provider = YtDlpProvider::new(
            script,
            "https://www.youtube.com/watch?v={video_id}".into(),
            None,
            None,
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(matches!(
            provider.metadata("demo").await,
            Err(AppError::YtDlpMetadataFailed(_))
        ));
    }

    #[tokio::test]
    async fn metadata_non_zero_exit_is_reported() {
        let script = fake_ytdlp_script(
            "#!/bin/sh\nfound=0\nfor arg in \"$@\"; do\n  if [ \"$arg\" = \"--dump-single-json\" ]; then found=1; fi\ndone\nif [ \"$found\" = \"1\" ]; then\n  echo 'failed' 1>&2\n  exit 1\nfi\nexit 1\n",
        )
        .await;
        let provider = YtDlpProvider::new(
            script,
            "https://www.youtube.com/watch?v={video_id}".into(),
            None,
            None,
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(provider.metadata("demo").await.is_err());
    }

    #[tokio::test]
    async fn metadata_uses_node_runtime_arg() {
        let script = fake_ytdlp_script(
            "#!/bin/sh\nfound=0\ndump=0\nprev=''\nfor arg in \"$@\"; do\n  if [ \"$arg\" = \"--dump-single-json\" ]; then dump=1; fi\n  if [ \"$prev\" = \"--js-runtimes\" ] && [ \"$arg\" = \"node\" ]; then found=1; fi\n  prev=\"$arg\"\ndone\nif [ \"$dump\" = \"1\" ] && [ \"$found\" = \"1\" ]; then\n  echo '{\"id\":\"demo\",\"title\":\"Demo\"}'\n  exit 0\nfi\nexit 1\n",
        )
        .await;
        let provider = YtDlpProvider::new(
            script,
            "https://www.youtube.com/watch?v={video_id}".into(),
            None,
            None,
            Duration::from_secs(5),
        )
        .unwrap()
        .with_js_runtime("node".into());
        provider.metadata("demo").await.unwrap();
    }

    #[tokio::test]
    async fn metadata_uses_cookies_from_browser_arg() {
        let script = fake_ytdlp_script(
            "#!/bin/sh\nfound=0\ndump=0\nprev=''\nfor arg in \"$@\"; do\n  if [ \"$arg\" = \"--dump-single-json\" ]; then dump=1; fi\n  if [ \"$prev\" = \"--cookies-from-browser\" ] && [ \"$arg\" = \"firefox\" ]; then found=1; fi\n  prev=\"$arg\"\ndone\nif [ \"$dump\" = \"1\" ] && [ \"$found\" = \"1\" ]; then\n  echo '{\"id\":\"demo\",\"title\":\"Demo\"}'\n  exit 0\nfi\nexit 1\n",
        )
        .await;
        let provider = YtDlpProvider::new(
            script,
            "https://www.youtube.com/watch?v={video_id}".into(),
            Some("firefox".into()),
            None,
            Duration::from_secs(5),
        )
        .unwrap();
        provider.metadata("demo").await.unwrap();
    }

    #[tokio::test]
    async fn metadata_uses_cookies_file_arg() {
        let script = fake_ytdlp_script(
            "#!/bin/sh\nfound=0\ndump=0\nprev=''\nfor arg in \"$@\"; do\n  if [ \"$arg\" = \"--dump-single-json\" ]; then dump=1; fi\n  if [ \"$prev\" = \"--cookies\" ] && [ \"$arg\" = \"/tmp/localtube-cookies.txt\" ]; then found=1; fi\n  prev=\"$arg\"\ndone\nif [ \"$dump\" = \"1\" ] && [ \"$found\" = \"1\" ]; then\n  echo '{\"id\":\"demo\",\"title\":\"Demo\"}'\n  exit 0\nfi\nexit 1\n",
        )
        .await;
        let provider = YtDlpProvider::new(
            script,
            "https://www.youtube.com/watch?v={video_id}".into(),
            None,
            Some(PathBuf::from("/tmp/localtube-cookies.txt")),
            Duration::from_secs(5),
        )
        .unwrap();
        provider.metadata("demo").await.unwrap();
    }

    #[tokio::test]
    async fn reject_conflicting_auth_configuration() {
        let result = YtDlpProvider::new(
            PathBuf::from("yt-dlp"),
            "https://www.youtube.com/watch?v={video_id}".into(),
            Some("firefox".into()),
            Some(PathBuf::from("cookies.txt")),
            Duration::from_secs(5),
        );
        assert!(matches!(result, Err(AppError::Configuration(_))));
    }

    #[tokio::test]
    async fn acquire_writes_output_with_fake_executable() {
        // Real yt-dlp writes progress template output to stdout.
        let script = fake_ytdlp_script(
            "#!/bin/sh\nout=''\nprev=''\nfor arg in \"$@\"; do\n  if [ \"$prev\" = \"-o\" ]; then out=\"$arg\"; fi\n  prev=\"$arg\"\ndone\nif [ -n \"$out\" ]; then\n  echo 'bytes' > \"$out\"\n  echo 'localtube:download:{\"downloaded_bytes\":5,\"total_bytes\":10,\"speed\":1.0,\"eta\":5}'\n  exit 0\nfi\nexit 1\n",
        )
        .await;
        let provider = YtDlpProvider::new(
            script,
            "https://www.youtube.com/watch?v={video_id}".into(),
            None,
            None,
            Duration::from_secs(5),
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("video.mp4");
        let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
        provider
            .acquire("demo", &output, Some(progress_tx))
            .await
            .unwrap();
        assert!(output.exists());
        let event = progress_rx.recv().await.unwrap();
        assert_eq!(event.stage, AcquisitionStage::Downloading);
        assert_eq!(event.percent, Some(50.0));
    }

    #[tokio::test]
    async fn acquire_failure_cleans_partial_artifacts() {
        let script = fake_ytdlp_script(
            "#!/bin/sh\nout=''\nprev=''\nfor arg in \"$@\"; do\n  if [ \"$prev\" = \"-o\" ]; then out=\"$arg\"; fi\n  prev=\"$arg\"\ndone\nif [ -n \"$out\" ]; then\n  echo 'partial' > \"$out.part\"\n  exit 1\nfi\nexit 1\n",
        )
        .await;
        let provider = YtDlpProvider::new(
            script,
            "https://www.youtube.com/watch?v={video_id}".into(),
            None,
            None,
            Duration::from_secs(5),
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("video.mp4");
        assert!(matches!(
            provider.acquire("demo", &output, None).await,
            Err(AppError::YtDlpAcquisitionFailed(_))
        ));
        assert!(!output.exists());
        assert!(!output.with_file_name("video.mp4.part").exists());
    }

    #[tokio::test]
    async fn missing_executable_is_reported() {
        let provider = YtDlpProvider::new(
            PathBuf::from("/definitely/missing/yt-dlp"),
            "https://www.youtube.com/watch?v={video_id}".into(),
            None,
            None,
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(matches!(
            provider.metadata("demo").await,
            Err(AppError::YtDlpNotInstalled)
        ));
    }
}
