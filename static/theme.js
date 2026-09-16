(function () {
  const STORAGE_KEY = 'localtube-theme';
  const root = document.documentElement;
  const media = window.matchMedia('(prefers-color-scheme: dark)');

  function systemTheme() {
    return media.matches ? 'dark' : 'light';
  }

  function apply(theme) {
    root.dataset.theme = theme;
    const toggle = document.querySelector('#theme-toggle');
    if (toggle) {
      const dark = theme === 'dark';
      const label = dark ? 'Switch to light mode' : 'Switch to dark mode';
      toggle.setAttribute('aria-pressed', String(dark));
      toggle.setAttribute('aria-label', label);
      toggle.title = label;
    }
  }

  apply(localStorage.getItem(STORAGE_KEY) || systemTheme());

  // The toggle button is parsed after this script runs, so update its
  // accessible state once the DOM exists.
  document.addEventListener('DOMContentLoaded', () => apply(root.dataset.theme));

  // Delegated listener: attached before the button exists, still catches clicks.
  document.addEventListener('click', (event) => {
    if (!event.target.closest('#theme-toggle')) return;
    const next = root.dataset.theme === 'dark' ? 'light' : 'dark';
    localStorage.setItem(STORAGE_KEY, next);
    apply(next);
  });

  // Follow OS theme changes only while the user has no explicit choice.
  media.addEventListener('change', (event) => {
    if (localStorage.getItem(STORAGE_KEY)) return;
    apply(event.matches ? 'dark' : 'light');
  });
})();
