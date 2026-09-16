"use strict";

(() => {
  const api = globalThis.browser ?? globalThis.chrome;
  if (!api?.runtime?.sendMessage) return;

  let lastUrl = location.href;

  function reportNavigation() {
    if (location.href === lastUrl) return;
    lastUrl = location.href;
    api.runtime.sendMessage({
      type: "localtube:navigation",
      url: lastUrl,
    }).catch?.(() => {});
  }

  window.addEventListener("yt-navigate-finish", reportNavigation);
  window.addEventListener("popstate", reportNavigation);
})();
