use std::{
    path::Path,
    time::{Duration, SystemTime},
};

use tokio::{fs, time};
use tracing::{info, warn};
use uuid::Uuid;

use crate::session::{PlaybackStatus, SessionManager, session_dir};

pub async fn remove_session(cache_dir: &Path, sessions: &SessionManager, id: Uuid) {
    sessions.remove(id).await;
    let directory = session_dir(cache_dir, id);
    match fs::remove_dir_all(&directory).await {
        Ok(()) => info!(session_id = %id, "cleaned temporary media"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => warn!(session_id = %id, %error, "failed to clean temporary media"),
    }
}

pub async fn cleanup_stale_cache(cache_dir: &Path, timeout: Duration) -> std::io::Result<()> {
    fs::create_dir_all(cache_dir).await?;
    let now = SystemTime::now();
    let mut entries = fs::read_dir(cache_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_dir() {
            continue;
        }
        let modified = entry
            .metadata()
            .await?
            .modified()
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if now.duration_since(modified).unwrap_or_default() > timeout {
            fs::remove_dir_all(entry.path()).await?;
        }
    }
    Ok(())
}

pub fn spawn(
    sessions: SessionManager,
    cache_dir: std::path::PathBuf,
    timeout: Duration,
    interval: Duration,
) {
    tokio::spawn(async move {
        let mut ticker = time::interval(interval);
        loop {
            ticker.tick().await;
            for session in sessions.expired(timeout).await {
                let id = session.id;
                let _ = sessions
                    .update(id, |session| session.status = PlaybackStatus::Expired)
                    .await;
                info!(session_id = %id, "playback session expired");
                remove_session(&cache_dir, &sessions, id).await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn removes_stale_directories_only() {
        let cache = tempfile::tempdir().unwrap();
        let stale = cache.path().join("stale");
        tokio::fs::create_dir(&stale).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        cleanup_stale_cache(cache.path(), Duration::from_millis(1))
            .await
            .unwrap();
        assert!(!stale.exists());
    }
}
