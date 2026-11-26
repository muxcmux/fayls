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
  if (event.detail.ctx.sourceEvent?.type === 'reload-view') return;

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

Dropzone.autoDiscover = false;

function uploadFiles() {
  return {
    uploads: [],
    nextId: 0,
    init() {
      const target = this.$refs.dropzone;
      this.dropzone = new Dropzone(target, {
        url: '/upload',
        method: 'post',
        maxFilesize: Number(target.dataset.maxSize) / (1024 * 1024),
        chunkSize: Number(target.dataset.chunkSize),
        chunking: true,
        forceChunking: false,
        parallelChunkUploads: false,
        retryChunks: true,
        retryChunksLimit: 5,
        parallelUploads: 2,
        createImageThumbnails: false,
        disablePreviews: true,
        hiddenInputContainer: target,
      });

      const itemFor = (file) => this.uploads.find((item) => item.id === file.uploadItemId);
      this.dropzone.on('sending', (_file, _xhr, formData) => {
        formData.append('parent', this.$refs.parent.value);
      });
      this.dropzone.on('addedfile', (file) => {
        file.uploadItemId = ++this.nextId;
        this.uploads.push({ id: file.uploadItemId, name: file.name, progress: 0, status: 'Queued', message: '', retryable: false });
      });
      this.dropzone.on('uploadprogress', (file, progress) => {
        const item = itemFor(file);
        if (item) {
          item.progress = progress;
          item.status = 'Uploading';
        }
      });
      this.dropzone.on('success', (file) => {
        const item = itemFor(file);
        if (item) {
          item.progress = 100;
          item.status = 'Complete';
        }
      });
      this.dropzone.on('error', (file, message) => {
        const item = itemFor(file);
        if (item) {
          item.status = 'Failed';
          item.message = typeof message === 'string' ? message : 'Upload failed';
          item.retryable = file.accepted === true;
        }
      });
    },
    retry(id) {
      const file = this.dropzone.files.find((candidate) => candidate.uploadItemId === id);
      if (!file || file.status !== Dropzone.ERROR || !file.accepted) return;
      const chunk = file.upload.chunked
        ? file.upload.chunks.find((candidate) => candidate?.dataBlock && candidate.status !== Dropzone.SUCCESS)
        : null;
      if (file.upload.chunked && !chunk) return;

      const item = this.uploads.find((candidate) => candidate.id === id);
      item.status = 'Uploading';
      item.message = '';
      item.retryable = false;

      if (file.upload.chunked) {
        chunk.retries = 0;
        chunk.progress = 0;
        chunk.bytesSent = 0;
        item.progress = 100 * file.upload.chunks.filter((candidate) => candidate?.status === Dropzone.SUCCESS).length / file.upload.totalChunkCount;
        file.status = Dropzone.UPLOADING;
        // Dropzone 5.9.3 retains successful chunks; resend only the failed data block.
        this.dropzone._uploadData([file], [chunk.dataBlock]);
      } else {
        item.progress = 0;
        this.dropzone.processFile(file);
      }
    },
    destroy() {
      this.dropzone?.destroy();
    },
  };
}

let fontPreviewSequence = 0;

async function loadFontPreview(url, signal) {
  const response = await fetch(url, { signal });
  if (!response.ok) throw new Error(`file request failed (${response.status})`);

  const face = new FontFace(`FaylsFontPreview${++fontPreviewSequence}`, await response.arrayBuffer());
  await face.load();
  if (signal.aborted) throw new DOMException('Aborted', 'AbortError');
  document.fonts.add(face);
  return face;
}

// context menu shite
let x, y;
function rowContextMenu(event) {
  event.preventDefault()
  x = event.x
  y = event.y
  event.currentTarget.parentElement.classList.add('highlight')
}
function repositionContextMenu(event) {
  const wrapper = document.getElementById('context-menu');
  const menu = wrapper.querySelector('ul');
  const w = menu.clientWidth;
  const h = menu.clientHeight;
  const sw = window.innerWidth;
  const sh = window.innerHeight;
  const mx = (x + w > sw) ? x - w : x;
  const my = (y + h > sh) ? y - h : y;
  menu.style = `transform: translate(${mx}px, ${my}px)`;
}
function contextMenuClose(_) {
  const rows = document.getElementById('view').querySelectorAll('.paths table tr');
  rows.forEach((r => r.classList.remove('highlight')))
}

// drag and drop files in lists
let draggedIds = new Set();

function dragRowStart(event, multiselecting, checked, id) {
  const row = event.currentTarget;

  if (multiselecting) {
    checked.add(id)
  }

  draggedIds = new Set(checked.size == 0 ? [id] : checked);

  const filename = row.querySelector('td.name').textContent;

  const ghost = document.createElement('div');
  ghost.classList = row.classList;
  ghost.classList.add('drag-ghost');

  if (checked.size > 1) {
    ghost.classList.add('multiple')
  }

  const content = document.createElement('div');
  content.classList.add('content');

  const icon = document.createElement('i');
  icon.className = checked.size == 0 ? 'icon' : 'select-multiple'
  content.appendChild(icon)

  const text = document.createElement('span');
  text.textContent = checked.size > 1 ? `${filename} and ${checked.size - 1} other${checked.size == 2 ? '' : 's'}` : filename;
  content.appendChild(text)

  ghost.appendChild(content);
  document.body.appendChild(ghost);

  event.dataTransfer.setDragImage(ghost, 10, 5);

  requestAnimationFrame(() => ghost.remove());
}

const dragDepth = new WeakMap();

function dragRowEnter(event, id) {
  const row = event.currentTarget;
  const depth = (dragDepth.get(row) ?? 0) + 1;
  dragDepth.set(row, depth);

  if (depth === 1 && !draggedIds.has(id)) {
    row.classList.add('drop');
  }
}

function dragRowLeave(event) {
  const row = event.currentTarget;
  const depth = dragDepth.get(row);
  if (depth === undefined) return;

  if (depth > 1) {
    dragDepth.set(row, depth - 1);
  } else {
    dragDepth.delete(row);
    row.classList.remove('drop');
  }
}

function dragRowDrop(event, id) {
  event.preventDefault();
  dragRowLeave(event, id)

  if (!draggedIds.has(id)) {
    event.currentTarget.dispatchEvent(new CustomEvent("move", {
      bubbles: true,
      detail: { ids: [...draggedIds] }
    }))
  }
}

function confirmMoveModal(event, moveUrl) {
  const url = new URL(moveUrl, location.origin);
  for (const id of event.detail.ids) {
    url.searchParams.append("id", String(id));
  }
  return htmx.ajax("GET", url.pathname + url.search, "#modal");
}
