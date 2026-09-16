use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{
    error::AppError,
    models::{AcquisitionProgress, VideoMetadata},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackStatus {
    Queued,
    Downloading,
    Processing,
    Ready,
    Playing,
    Completed,
    Failed,
    Expired,
}

#[derive(Clone, Debug)]
pub struct PlaybackSession {
    pub id: Uuid,
    pub video_id: String,
    pub metadata: VideoMetadata,
    pub status: PlaybackStatus,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub media_path: Option<PathBuf>,
    pub progress: Option<AcquisitionProgress>,
    pub error: Option<String>,
}

impl PlaybackSession {
    pub fn new(video_id: String, metadata: VideoMetadata) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            video_id,
            metadata,
            status: PlaybackStatus::Queued,
            created_at: now,
            last_activity: now,
            media_path: None,
            progress: None,
            error: None,
        }
    }

    pub fn is_expired(&self, timeout: Duration, now: DateTime<Utc>) -> bool {
        now.signed_duration_since(self.last_activity)
            .to_std()
            .map(|elapsed| elapsed > timeout)
            .unwrap_or(false)
    }
}

#[derive(Clone, Default)]
pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<Uuid, PlaybackSession>>>,
}

impl SessionManager {
    pub async fn create(&self, video_id: String, metadata: VideoMetadata) -> PlaybackSession {
        let session = PlaybackSession::new(video_id, metadata);
        self.sessions
            .write()
            .await
            .insert(session.id, session.clone());
        session
    }

    pub async fn get(&self, id: Uuid) -> Result<PlaybackSession, AppError> {
        self.sessions
            .read()
            .await
            .get(&id)
            .cloned()
            .ok_or(AppError::SessionNotFound)
    }

    pub async fn update<F>(&self, id: Uuid, update: F) -> Result<PlaybackSession, AppError>
    where
        F: FnOnce(&mut PlaybackSession),
    {
        let mut sessions = self.sessions.write().await;
        let session = sessions.get_mut(&id).ok_or(AppError::SessionNotFound)?;
        update(session);
        session.last_activity = Utc::now();
        Ok(session.clone())
    }

    pub async fn remove(&self, id: Uuid) -> Option<PlaybackSession> {
        self.sessions.write().await.remove(&id)
    }

    pub async fn expired(&self, timeout: Duration) -> Vec<PlaybackSession> {
        let now = Utc::now();
        self.sessions
            .read()
            .await
            .values()
            .filter(|session| session.is_expired(timeout, now))
            .cloned()
            .collect()
    }
}

pub fn session_dir(cache_dir: &Path, id: Uuid) -> PathBuf {
    cache_dir.join(id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;

    fn metadata() -> VideoMetadata {
        VideoMetadata {
            id: "demo".into(),
            title: "Demo".into(),
            description: None,
            thumbnail_url: None,
            duration_seconds: None,
        }
    }

    #[tokio::test]
    async fn creates_and_finds_sessions() {
        let manager = SessionManager::default();
        let session = manager.create("demo".into(), metadata()).await;
        assert_eq!(manager.get(session.id).await.unwrap().video_id, "demo");
    }

    #[test]
    fn detects_expiration() {
        let mut session = PlaybackSession::new("demo".into(), metadata());
        session.last_activity = Utc::now() - ChronoDuration::seconds(31);
        assert!(session.is_expired(Duration::from_secs(30), Utc::now()));
    }

    #[test]
    fn generates_confined_cache_path() {
        let root = Path::new("/tmp/localtube");
        let id = Uuid::nil();
        assert_eq!(session_dir(root, id), root.join(id.to_string()));
    }
}
