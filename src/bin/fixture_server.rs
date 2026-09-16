use std::{net::SocketAddr, path::PathBuf, process::Stdio, time::Duration};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use tokio::{fs, net::TcpListener, process::Command, time};
use tokio_util::io::ReaderStream;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Clone)]
struct FixtureState {
    media_path: PathBuf,
}

#[derive(Serialize)]
struct FixtureMetadata<'a> {
    id: &'a str,
    title: &'a str,
    description: Option<&'a str>,
    thumbnail_url: Option<&'a str>,
    duration_seconds: Option<u64>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("fixture_server=info")),
        )
        .init();

    let fixture_dir = std::env::temp_dir().join("localtube-fixture");
    fs::create_dir_all(&fixture_dir).await?;
    let media_path = fixture_dir.join("demo.mp4");
    generate_fixture(&media_path).await?;

    let app = Router::new()
        .route("/metadata/{id}", get(metadata))
        .route("/media/{id}", get(media))
        .with_state(FixtureState { media_path });
    let address: SocketAddr = "127.0.0.1:6970".parse()?;
    let listener = TcpListener::bind(address).await?;
    info!(%address, "LocalTube fixture server starting");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn generate_fixture(path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    if fs::metadata(path)
        .await
        .map(|metadata| metadata.len() > 0)
        .unwrap_or(false)
    {
        return Ok(());
    }
    let output = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=640x360:rate=30",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "5",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-movflags",
            "+faststart",
        ])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;
    if !output.status.success() {
        return Err(format!(
            "unable to generate fixture: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}

async fn metadata(Path(id): Path<String>) -> Response {
    match id.as_str() {
        "demo" => Json(FixtureMetadata {
            id: "demo",
            title: "LocalTube Demo Video",
            description: Some("Deterministic test media"),
            thumbnail_url: None,
            duration_seconds: Some(5),
        })
        .into_response(),
        "invalid-json" => {
            ([(header::CONTENT_TYPE, "application/json")], "{invalid").into_response()
        }
        "server-error" => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        "timeout" => {
            time::sleep(Duration::from_secs(60)).await;
            StatusCode::REQUEST_TIMEOUT.into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn media(State(state): State<FixtureState>, Path(id): Path<String>) -> Response {
    match id.as_str() {
        "demo" => match fs::File::open(&state.media_path).await {
            Ok(file) => (
                [(header::CONTENT_TYPE, "video/mp4")],
                Body::from_stream(ReaderStream::new(file)),
            )
                .into_response(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        "empty" => ([(header::CONTENT_TYPE, "video/mp4")], Body::empty()).into_response(),
        "invalid-media" => (
            [(header::CONTENT_TYPE, "video/mp4")],
            Body::from("not media"),
        )
            .into_response(),
        "server-error" => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        "timeout" => {
            time::sleep(Duration::from_secs(60)).await;
            StatusCode::REQUEST_TIMEOUT.into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
