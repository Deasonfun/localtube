"use strict";

const LocalTubeConfig = Object.freeze({
  LOCAL_BASE_URL: "http://127.0.0.1:6969",
  SUPPORTED_HOSTS: Object.freeze([
    "www.youtube.com",
    "youtube.com",
    "m.youtube.com",
  ]),
  VIDEO_ID_PATTERN: /^[A-Za-z0-9_-]{1,128}$/,
  HEALTH_TIMEOUT_MS: 1500,
  DUPLICATE_WINDOW_MS: 3000,
});

if (typeof globalThis !== "undefined") {
  globalThis.LocalTubeConfig = LocalTubeConfig;
}
