use std::{env, net::IpAddr, path::PathBuf, str::FromStr, time::Duration};

use crate::error::AppError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaProviderKind {
    Local,
    HttpFixture,
    YtDlp,
}

impl FromStr for MediaProviderKind {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "local" => Ok(Self::Local),
            "http-fixture" => Ok(Self::HttpFixture),
            "yt-dlp" => Ok(Self::YtDlp),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub host: IpAddr,
    pub port: u16,
    pub cache_dir: PathBuf,
    pub media_dir: PathBuf,
    pub session_timeout: Duration,
    pub cleanup_interval: Duration,
    pub max_concurrent_downloads: usize,
    pub allowed_origins: Vec<String>,
    pub media_provider: MediaProviderKind,
    pub fixture_base_url: String,
    pub ytdlp_path: PathBuf,
    pub youtube_base_url: String,
    pub ytdlp_cookies_from_browser: Option<String>,
    pub ytdlp_cookies_file: Option<PathBuf>,
    pub ytdlp_js_runtime: String,
    pub acquisition_timeout: Duration,
    pub fixture_connect_timeout: Duration,
}

impl Config {
    pub fn from_env() -> Result<Self, AppError> {
        let host = value("LOCALTUBE_HOST", "127.0.0.1").parse().map_err(|_| {
            AppError::Configuration("LOCALTUBE_HOST must be a valid IP address".into())
        })?;
        let port = parse("LOCALTUBE_PORT", 6969_u16)?;
        let session_timeout = Duration::from_secs(parse("LOCALTUBE_SESSION_TIMEOUT", 1800_u64)?);
        let cleanup_default = session_timeout.as_secs().min(300).max(1);

        let ytdlp_cookies_from_browser = optional_value("LOCALTUBE_YTDLP_COOKIES_FROM_BROWSER");
        let ytdlp_cookies_file = optional_value("LOCALTUBE_YTDLP_COOKIES_FILE").map(PathBuf::from);
        let ytdlp_js_runtime = value("LOCALTUBE_YTDLP_JS_RUNTIME", "node");

        if ytdlp_cookies_from_browser.is_some() && ytdlp_cookies_file.is_some() {
            return Err(AppError::Configuration(
                "set only one of LOCALTUBE_YTDLP_COOKIES_FROM_BROWSER or LOCALTUBE_YTDLP_COOKIES_FILE".into(),
            ));
        }
        if let Some(source) = ytdlp_cookies_from_browser.as_deref()
            && !valid_cookies_source(source)
        {
            return Err(AppError::Configuration(
                "LOCALTUBE_YTDLP_COOKIES_FROM_BROWSER contains invalid characters".into(),
            ));
        }

        if !valid_js_runtime(&ytdlp_js_runtime) {
            return Err(AppError::Configuration(
                "LOCALTUBE_YTDLP_JS_RUNTIME must be node or node:/absolute/path".into(),
            ));
        }

        Ok(Self {
            host,
            port,
            cache_dir: value("LOCALTUBE_CACHE_DIR", "cache").into(),
            media_dir: value("LOCALTUBE_MEDIA_DIR", "media").into(),
            session_timeout,
            cleanup_interval: Duration::from_secs(parse(
                "LOCALTUBE_CLEANUP_INTERVAL",
                cleanup_default,
            )?),
            max_concurrent_downloads: parse("LOCALTUBE_MAX_CONCURRENT_DOWNLOADS", 2_usize)?.max(1),
            allowed_origins: comma_separated("LOCALTUBE_ALLOWED_ORIGINS"),
            media_provider: parse("LOCALTUBE_MEDIA_PROVIDER", MediaProviderKind::Local)?,
            fixture_base_url: value("LOCALTUBE_FIXTURE_BASE_URL", "http://127.0.0.1:6970"),
            ytdlp_path: value("LOCALTUBE_YTDLP_PATH", "yt-dlp").into(),
            youtube_base_url: value(
                "LOCALTUBE_YOUTUBE_BASE_URL",
                "https://www.youtube.com/watch?v={video_id}",
            ),
            ytdlp_cookies_from_browser,
            ytdlp_cookies_file,
            ytdlp_js_runtime,
            acquisition_timeout: Duration::from_secs(parse(
                "LOCALTUBE_ACQUISITION_TIMEOUT",
                session_timeout.as_secs(),
            )?),
            fixture_connect_timeout: Duration::from_secs(parse(
                "LOCALTUBE_FIXTURE_CONNECT_TIMEOUT",
                3_u64,
            )?),
        })
    }
}

fn comma_separated(name: &str) -> Vec<String> {
    env::var(name)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn value(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn optional_value(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn valid_js_runtime(value: &str) -> bool {
    value == "node"
        || value
            .strip_prefix("node:")
            .map(|path| path.starts_with('/') && !path.contains(['\n', '\r']))
            .unwrap_or(false)
}

fn valid_cookies_source(value: &str) -> bool {
    value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':' | b',')
        })
}

fn parse<T>(name: &str, default: T) -> Result<T, AppError>
where
    T: std::str::FromStr,
{
    match env::var(name) {
        Ok(value) => value
            .parse()
            .map_err(|_| AppError::Configuration(format!("{name} has an invalid value"))),
        Err(_) => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{MediaProviderKind, valid_cookies_source, valid_js_runtime};

    #[test]
    fn parses_media_provider_selection() {
        assert_eq!(
            MediaProviderKind::from_str("local"),
            Ok(MediaProviderKind::Local)
        );
        assert_eq!(
            MediaProviderKind::from_str("http-fixture"),
            Ok(MediaProviderKind::HttpFixture)
        );
        assert_eq!(
            MediaProviderKind::from_str("yt-dlp"),
            Ok(MediaProviderKind::YtDlp)
        );
        assert!(MediaProviderKind::from_str("arbitrary").is_err());
    }

    #[test]
    fn validates_node_runtime_values() {
        assert!(valid_js_runtime("node"));
        assert!(valid_js_runtime("node:/usr/bin/node"));
        assert!(!valid_js_runtime("deno"));
        assert!(!valid_js_runtime("node:relative"));
    }

    #[test]
    fn validates_cookie_source_values() {
        assert!(valid_cookies_source("firefox"));
        assert!(valid_cookies_source("firefox:default-release"));
        assert!(valid_cookies_source("chromium,Default"));
        assert!(!valid_cookies_source("firefox profile"));
        assert!(!valid_cookies_source("bad;value"));
    }
}
