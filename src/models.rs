use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VideoMetadata {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub thumbnail_url: Option<String>,
    pub duration_seconds: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcquisitionStage {
    Downloading,
    Processing,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AcquisitionProgress {
    pub stage: AcquisitionStage,
    pub percent: Option<f64>,
    pub downloaded_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub speed_bytes_per_second: Option<f64>,
    pub eta_seconds: Option<u64>,
}

impl AcquisitionProgress {
    pub fn downloading(downloaded_bytes: u64, total_bytes: Option<u64>) -> Self {
        let percent = total_bytes
            .filter(|total| *total > 0)
            .map(|total| downloaded_bytes as f64 * 100.0 / total as f64);
        Self {
            stage: AcquisitionStage::Downloading,
            percent,
            downloaded_bytes: Some(downloaded_bytes),
            total_bytes,
            speed_bytes_per_second: None,
            eta_seconds: None,
        }
    }

    pub fn processing() -> Self {
        Self {
            stage: AcquisitionStage::Processing,
            percent: None,
            downloaded_bytes: None,
            total_bytes: None,
            speed_bytes_per_second: None,
            eta_seconds: None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct WatchQuery {
    pub v: String,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
}

pub fn valid_video_id(value: &str) -> bool {
    let length_ok = (1..=128).contains(&value.len());
    length_ok
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::valid_video_id;

    #[test]
    fn validates_safe_identifiers() {
        assert!(valid_video_id("demo-video_01"));
        assert!(!valid_video_id(""));
        assert!(!valid_video_id("../secret"));
        assert!(!valid_video_id("https://example.com/video"));
        assert!(!valid_video_id(&"a".repeat(129)));
    }
}
