function timeAgo(unixTimestamp) {
  const now = new Date();
  const date = new Date(unixTimestamp * 1000);

  const secondsDiff = Math.floor((now - date) / 1000);

  const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });

  if (secondsDiff < 60) {
    return rtf.format(0, "second");
  }

  const intervals = [
    { label: "year", seconds: 31536000 },
    { label: "month", seconds: 2592000 },
    { label: "week", seconds: 604800 },
    { label: "day", seconds: 86400 },
    { label: "hour", seconds: 3600 },
    { label: "minute", seconds: 60 }
  ];

  for (const interval of intervals) {
    const count = Math.floor(secondsDiff / interval.seconds);
    if (count >= 1) {
      return rtf.format(-count, interval.label);
    }
  }
}

function time(unixTimestamp) {
  const date = new Date(unixTimestamp * 1000);
  return date.toUTCString();
}

const activeRequests = new Set();
let progressShowTimer;
let progressHideTimer;

// Keep server errors from replacing the current view with an error response.
htmx.config.noSwap.push('5xx');

function finishRequest(ctx) {
  if (!activeRequests.delete(ctx) || activeRequests.size) return;

  clearTimeout(progressShowTimer);
  const progress = document.getElementById('request-progress');
  if (!progress.classList.contains('is-visible')) return;

  progress.classList.add('is-complete');
  progressHideTimer = setTimeout(() => {
    progress.classList.remove('is-visible');
    progressHideTimer = setTimeout(() => {
      progress.classList.remove('is-complete');
    }, 200);
  }, 200);
}

function showRequestError(message) {
  document.getElementById('request-error-text').textContent = message;
  document.getElementById('request-error').hidden = false;
}

htmx.on('htmx:before:request', (event) => {
  const path = new URL(event.detail.ctx.request.action, document.baseURI).pathname;
  if (path === '/sse' || path === '/shared/sse') return;

  const wasIdle = activeRequests.size === 0;
  activeRequests.add(event.detail.ctx);
  if (!wasIdle) return;

  clearTimeout(progressHideTimer);
  const progress = document.getElementById('request-progress');
  progress.classList.remove('is-visible', 'is-complete');
  clearTimeout(progressShowTimer);
  progressShowTimer = setTimeout(() => {
    if (activeRequests.size) progress.classList.add('is-visible');
  }, 150);
});

htmx.on('htmx:finally:request', (event) => {
  finishRequest(event.detail.ctx);
});

htmx.on('htmx:after:request', (event) => {
  if (event.detail.ctx.response.status < 400) {
    document.getElementById('request-error').hidden = true;
  }
});

htmx.on('htmx:response:error', (event) => {
  if (event.detail.ctx.response.status >= 500) {
    showRequestError('The server returned an error. Please try again.');
  }
});

htmx.on('htmx:error', (event) => {
  if (!event.detail.ctx) return;
  showRequestError('The request failed. Check your connection and try again.');
});

document.addEventListener('click', (event) => {
  if (event.target.closest('#request-error button')) {
    document.getElementById('request-error').hidden = true;
  }
});

htmx.on('htmx:before:history:update', () => {
  sessionStorage.setItem('scrollY', window.scrollY);
});

document.addEventListener('htmx:before:history:restore', () => {
  sessionStorage.setItem('willRestoreHistory', true);
});

htmx.on('htmx:after:settle', () => {
  if (sessionStorage.getItem('willRestoreHistory')) {
    const y = parseInt(sessionStorage.getItem('scrollY') || '0', 10);
    window.scrollTo(0, y);
  }

  sessionStorage.removeItem('willRestoreHistory');
})

function hls(el, src) {
  if (window.Hls && Hls.isSupported()) {
    const hls = new Hls();
    hls.loadSource(src);
    hls.attachMedia(el);
    el.hls = hls;
  } else if (el.canPlayType("application/vnd.apple.mpegurl")) {
    el.src = src;
  }
}
