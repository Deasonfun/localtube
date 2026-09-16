mod cleanup;
mod config;
mod error;
mod media;
mod models;
mod routes;
mod session;

use std::sync::Arc;

use config::{Config, MediaProviderKind};
use error::AppError;
use media::{HttpFixtureProvider, LocalFileProvider, MediaProvider, YtDlpProvider};
use routes::AppState;
use tokio::{fs, net::TcpListener};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), AppError> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("localtube=info,tower_http=info")),
        )
        .init();

    let config = Config::from_env()?;
    fs::create_dir_all(&config.cache_dir)
        .await
        .map_err(AppError::internal)?;
    fs::create_dir_all(&config.media_dir)
        .await
        .map_err(AppError::internal)?;
    cleanup::cleanup_stale_cache(&config.cache_dir, config.session_timeout)
        .await
        .map_err(AppError::internal)?;

    let provider: Arc<dyn MediaProvider> = match config.media_provider {
        MediaProviderKind::Local => Arc::new(LocalFileProvider::new(config.media_dir.clone())),
        MediaProviderKind::HttpFixture => {
            info!(base_url = %config.fixture_base_url, "configuring HTTP fixture provider");
            Arc::new(HttpFixtureProvider::new(
                &config.fixture_base_url,
                config.fixture_connect_timeout,
                config.acquisition_timeout,
            )?)
        }
        MediaProviderKind::YtDlp => {
            info!(path = %config.ytdlp_path.display(), base = %config.youtube_base_url, "configuring yt-dlp provider");
            Arc::new(
                YtDlpProvider::new(
                    config.ytdlp_path.clone(),
                    config.youtube_base_url.clone(),
                    config.ytdlp_cookies_from_browser.clone(),
                    config.ytdlp_cookies_file.clone(),
                    config.acquisition_timeout,
                )?
                .with_js_runtime(config.ytdlp_js_runtime.clone()),
            )
        }
    };
    info!(provider = ?config.media_provider, "selected media provider");
    let state = AppState::new(config.clone(), provider);
    cleanup::spawn(
        state.sessions.clone(),
        config.cache_dir.clone(),
        config.session_timeout,
        config.cleanup_interval,
    );

    let address = (config.host, config.port);
    let listener = TcpListener::bind(address)
        .await
        .map_err(AppError::internal)?;
    info!(host = %config.host, port = config.port, cache = %config.cache_dir.display(), "LocalTube server starting");
    axum::serve(listener, routes::router(state))
        .await
        .map_err(AppError::internal)
}
