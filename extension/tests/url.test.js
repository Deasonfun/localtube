"use strict";

const assert = require("node:assert/strict");
const { test } = require("node:test");

require("../config.js");
const { extractYouTubeVideoId, buildLocalTubeUrl } = require("../url.js");

const supported = [
  ["https://www.youtube.com/watch?v=abc123", "abc123"],
  ["https://www.youtube.com/watch?v=abc123&list=xyz", "abc123"],
  ["https://youtube.com/watch?v=A-b_C9", "A-b_C9"],
  ["https://m.youtube.com/watch?v=mobile1", "mobile1"],
];

for (const [url, expected] of supported) {
  test(`supports ${url}`, () => {
    assert.equal(extractYouTubeVideoId(url), expected);
  });
}

const ignored = [
  "https://www.youtube.com/",
  "https://www.youtube.com/results?search_query=test",
  "https://www.youtube.com/channel/abc",
  "https://www.youtube.com/playlist?list=xyz",
  "https://www.youtube.com/shorts/abc123",
  "https://www.youtube.com/watch",
  "https://www.youtube.com/watch?v=",
  "https://www.youtube.com/watch?v=bad%2Fid",
  "http://www.youtube.com/watch?v=abc123",
  "https://evil.example/watch?v=abc123",
  "http://127.0.0.1:6969/watch?v=abc123",
  "not a url",
];

for (const url of ignored) {
  test(`ignores ${url}`, () => {
    assert.equal(extractYouTubeVideoId(url), null);
  });
}

test("builds the LocalTube watch URL with URL APIs", () => {
  assert.equal(
    buildLocalTubeUrl("abc123"),
    "http://127.0.0.1:6969/watch?v=abc123",
  );
});

test("does not build a URL for an invalid video ID", () => {
  assert.equal(buildLocalTubeUrl("../secret"), null);
});
