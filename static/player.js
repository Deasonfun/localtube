const sessionId = document.body.dataset.sessionId;
const player = document.querySelector('#player');
const statusNode = document.querySelector('#status');
const preparing = document.querySelector('#preparing');
const preparingLabel = document.querySelector('#preparing-label');
const downloadProgress = document.querySelector('#download-progress');
const downloadPercent = document.querySelector('#download-percent');
const downloadSize = document.querySelector('#download-size');
const downloadSpeed = document.querySelector('#download-speed');
const downloadEta = document.querySelector('#download-eta');
const errorNode = document.querySelector('#error');
const description = document.querySelector('.stat__description');
let timer;
let pendingSeek = null;

// Turns URLs and timestamps in the plain-text description into clickable
// elements. Everything is built with createElement so no HTML can be injected.
function linkifyDescription() {
  if (!description || !description.textContent.trim()) return;
  const text = description.textContent;
  const pattern = /(https?:\/\/[^\s<>"']+)|\b(?:(\d{1,2}):)?(\d{1,2}):(\d{2})\b/g;
  const fragment = document.createDocumentFragment();
  let lastIndex = 0;
  let match;
  while ((match = pattern.exec(text)) !== null) {
    fragment.append(document.createTextNode(text.slice(lastIndex, match.index)));
    if (match[1]) {
      const url = match[1].replace(/[.,;:!?)\]]+$/, '');
      const link = document.createElement('a');
      link.href = url;
      link.target = '_blank';
      link.rel = 'noopener noreferrer';
      link.textContent = url;
      fragment.append(link);
      fragment.append(document.createTextNode(match[1].slice(url.length)));
    } else {
      const hours = match[2] ? parseInt(match[2], 10) : 0;
      const seconds = hours * 3600 + parseInt(match[3], 10) * 60 + parseInt(match[4], 10);
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'timestamp';
      button.dataset.seconds = seconds;
      button.textContent = match[0];
      fragment.append(button);
    }
    lastIndex = match.index + match[0].length;
  }
  fragment.append(document.createTextNode(text.slice(lastIndex)));
  description.replaceChildren(fragment);
}

linkifyDescription();

description?.addEventListener('click', (event) => {
  const target = event.target.closest('.timestamp');
  if (!target) return;
  const seconds = Number(target.dataset.seconds);
  if (!Number.isFinite(seconds)) return;
  if (player.readyState >= 1) {
    player.currentTime = seconds;
  } else {
    // Media not loaded yet; seek once metadata is available.
    pendingSeek = seconds;
  }
});

player.addEventListener('loadedmetadata', () => {
  if (pendingSeek !== null) {
    player.currentTime = pendingSeek;
    pendingSeek = null;
  }
});

function formatBytes(value) {
  if (!Number.isFinite(value) || value < 0) return '';
  const units = ['B', 'KiB', 'MiB', 'GiB'];
  let amount = value;
  let unit = 0;
  while (amount >= 1024 && unit < units.length - 1) {
    amount /= 1024;
    unit += 1;
  }
  return `${amount.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
}

function formatEta(seconds) {
  if (!Number.isFinite(seconds) || seconds < 0) return '';
  const rounded = Math.round(seconds);
  const minutes = Math.floor(rounded / 60);
  return `${minutes}:${String(rounded % 60).padStart(2, '0')}`;
}

function clearProgressDetails() {
  downloadPercent.textContent = '';
  downloadSize.textContent = '';
  downloadSpeed.textContent = '';
  downloadEta.textContent = '';
}

async function updateStatus() {
  try {
    const response = await fetch(`/api/sessions/${sessionId}`, { cache: 'no-store' });
    if (!response.ok) throw new Error('Session is unavailable.');
    const session = await response.json();
    statusNode.textContent = session.status_label;
    const progress = session.progress;
    if (progress?.stage === 'downloading') {
      preparingLabel.textContent = 'Downloading video…';
      if (Number.isFinite(progress.percent)) {
        const percent = Math.max(0, Math.min(100, progress.percent));
        downloadProgress.value = percent;
        downloadPercent.textContent = `${percent.toFixed(1)}%`;
      } else {
        downloadProgress.removeAttribute('value');
        downloadPercent.textContent = '';
      }

      const downloaded = formatBytes(progress.downloaded_bytes);
      const total = formatBytes(progress.total_bytes);
      downloadSize.textContent = downloaded
        ? `Downloaded: ${downloaded}${total ? ` / ${total}` : ''}`
        : '';
      const speed = formatBytes(progress.speed_bytes_per_second);
      downloadSpeed.textContent = speed ? `Speed: ${speed}/s` : '';
      const eta = formatEta(progress.eta_seconds);
      downloadEta.textContent = eta ? `ETA: ${eta}` : '';
    } else {
      const processing = progress?.stage === 'processing' || session.status === 'processing';
      preparingLabel.textContent = processing ? 'Processing video…' : 'Preparing video…';
      downloadProgress.removeAttribute('value');
      clearProgressDetails();
    }
    if (session.status === 'ready' || session.status === 'playing') {
      clearInterval(timer);
      player.src = `/media/${sessionId}`;
      player.style.display = 'block';
      preparing.style.display = 'none';
    } else if (session.status === 'failed') {
      clearInterval(timer);
      preparing.style.display = 'none';
      errorNode.textContent = session.error || 'The media could not be acquired.';
    }
  } catch (error) {
    clearInterval(timer);
    errorNode.textContent = error.message;
  }
}

async function event(name) {
  try { await fetch(`/api/sessions/${sessionId}/${name}`, { method: 'POST', keepalive: true }); } catch (_) {}
}

player.addEventListener('play', () => event('playing'));
player.addEventListener('ended', () => event('completed'));
timer = setInterval(updateStatus, 1000);
updateStatus();
