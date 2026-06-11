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
  } else if (media.canPlayType("application/vnd.apple.mpegurl")) {
    el.src = src;
  }
}
