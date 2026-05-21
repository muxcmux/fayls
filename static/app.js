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

async function preview_docx(el, url) {
  const blob = await fetch(url).then(r => r.blob());
  const opts = {
    useBase64URL: true,
  }
  docx.renderAsync(blob, el, null, opts).then(() => {
    el.removeAttribute("aria-busy");
    el.querySelector('.docx-wrapper').style = "background: var(--pico-muted-border-color)"
    el.querySelectorAll('.docx-wrapper section.docx').forEach(m => m.setAttribute('data-theme', 'light'))
  });
}
