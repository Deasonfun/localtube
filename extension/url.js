"use strict";

function extractYouTubeVideoId(value, config = globalThis.LocalTubeConfig) {
  let url;
  try {
    url = new URL(value);
  } catch {
    return null;
  }

  if (url.protocol !== "https:") return null;
  if (!config.SUPPORTED_HOSTS.includes(url.hostname.toLowerCase())) return null;
  if (url.pathname !== "/watch") return null;

  const videoId = url.searchParams.get("v");
  if (!videoId || !config.VIDEO_ID_PATTERN.test(videoId)) return null;
  return videoId;
}

function buildLocalTubeUrl(videoId, config = globalThis.LocalTubeConfig) {
  if (!config.VIDEO_ID_PATTERN.test(videoId)) return null;
  const target = new URL("/watch", config.LOCAL_BASE_URL);
  target.searchParams.set("v", videoId);
  return target.href;
}

if (typeof globalThis !== "undefined") {
  globalThis.LocalTubeUrl = Object.freeze({
    extractYouTubeVideoId,
    buildLocalTubeUrl,
  });
}

if (typeof module !== "undefined" && module.exports) {
  module.exports = { extractYouTubeVideoId, buildLocalTubeUrl };
}
