"use strict";

if (typeof importScripts === "function" && !globalThis.LocalTubeConfig) {
  importScripts("config.js", "url.js");
}

const extensionApi = globalThis.browser ?? globalThis.chrome;
const recentRedirects = new Map();

function log(message, ...details) {
  console.info(`[LocalTube] ${message}`, ...details);
}

function logError(message, ...details) {
  console.error(`[LocalTube] ${message}`, ...details);
}

function isDuplicate(tabId, videoId) {
  const previous = recentRedirects.get(tabId);
  return previous?.videoId === videoId
    && Date.now() - previous.timestamp < LocalTubeConfig.DUPLICATE_WINDOW_MS;
}

async function backendAvailable() {
  const controller = new AbortController();
  const timeout = setTimeout(
    () => controller.abort(),
    LocalTubeConfig.HEALTH_TIMEOUT_MS,
  );

  try {
    log("Checking backend");
    const response = await fetch(`${LocalTubeConfig.LOCAL_BASE_URL}/health`, {
      cache: "no-store",
      signal: controller.signal,
    });
    if (!response.ok) return false;
    const health = await response.json();
    return health?.status === "ok";
  } catch (error) {
    logError("LocalTube backend unavailable", error);
    return false;
  } finally {
    clearTimeout(timeout);
  }
}

async function handleNavigation(tabId, rawUrl) {
  const videoId = LocalTubeUrl.extractYouTubeVideoId(rawUrl);
  if (!videoId || isDuplicate(tabId, videoId)) return;

  recentRedirects.set(tabId, { videoId, timestamp: Date.now() });
  log("Detected YouTube video");
  log("Video ID:", videoId);

  if (!await backendAvailable()) {
    recentRedirects.delete(tabId);
    logError("LocalTube isn't running. Start the backend and try again.");
    return;
  }

  const target = LocalTubeUrl.buildLocalTubeUrl(videoId);
  if (!target) return;

  log("Backend available");
  log("Redirecting to LocalTube");
  try {
    await extensionApi.tabs.update(tabId, { url: target });
  } catch (error) {
    logError("Unable to redirect the tab", error);
  }
}

extensionApi.webNavigation.onCommitted.addListener((details) => {
  if (details.frameId === 0) void handleNavigation(details.tabId, details.url);
}, {
  url: [{
    schemes: ["https"],
    hostEquals: "www.youtube.com",
    pathEquals: "/watch",
  }, {
    schemes: ["https"],
    hostEquals: "youtube.com",
    pathEquals: "/watch",
  }, {
    schemes: ["https"],
    hostEquals: "m.youtube.com",
    pathEquals: "/watch",
  }],
});

extensionApi.runtime.onMessage.addListener((message, sender) => {
  if (message?.type !== "localtube:navigation") return;
  if (sender.tab?.id === undefined || sender.frameId !== 0) return;
  void handleNavigation(sender.tab.id, message.url);
});

extensionApi.tabs.onRemoved.addListener((tabId) => {
  recentRedirects.delete(tabId);
});
