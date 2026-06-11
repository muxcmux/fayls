use crate::{
    config,
    db::{self, ExistingShareRecord, NewShareRecord},
    error::{AppResult, Error},
    indexing::get_progress,
    web::{
        Access, AuthorizedPath, Order, SharedAccess, Sort, access_scoped_path,
        av::{is_audio_file_extension, is_video_file_extension, should_stream_directly},
        get_sorting,
    },
};
use std::path::{Path, PathBuf};

use base64_turbo::STANDARD;
use maud::{DOCTYPE, Markup, PreEscaped, Render, html};
use multimap::MultiMap;
use salvo::{Request, Scribe, writing::Text};

use crate::db::{ExistingPathRecord, PathRecordKind};

fn access_base_url(path: &str, access: &Access) -> String {
    match access {
        Access::Admin => path.into(),
        Access::Shared(_) => format!("/shared{path}"),
    }
}

fn files_url(path: &str, access: &Access) -> String {
    access_base_url(
        &format!("/files{}", access_scoped_path(path, access)),
        access,
    )
}

fn download_url(path: &str, access: &Access) -> String {
    access_base_url(
        &format!("/download?path={}", access_scoped_path(path, access)),
        access,
    )
}

fn sse_url(path: &str, access: &Access) -> String {
    access_base_url(
        &format!("/sse?path={}", access_scoped_path(path, access)),
        access,
    )
}

fn preview_url(path: &str, access: &Access) -> String {
    access_base_url(
        &format!("/preview?path={}", access_scoped_path(path, access)),
        access,
    )
}

fn preview_hls_url(path: &str, hls: &str, access: &Access) -> String {
    access_base_url(
        &format!(
            "/preview?path={}&hls={}",
            access_scoped_path(path, access),
            hls
        ),
        access,
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum View {
    Root,
    Dir(PathBuf),
    File(PathBuf),
    Search(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Page {
    view: View,
}

impl Page {
    pub(crate) fn search(term: &str) -> Self {
        Self {
            view: View::Search(term.to_string()),
        }
    }

    pub(crate) fn root() -> Self {
        Self { view: View::Root }
    }
}

pub(crate) enum Message<'a> {
    Error(&'a str),
    Success(&'a str),
}

impl Message<'_> {
    fn to_html(&self) -> Markup {
        match self {
            Self::Error(msg) => html! {
                div.alert { (msg) }
            },
            Self::Success(msg) => html! {
                div.alert.success { (msg) }
            },
        }
    }
}

impl Render for Message<'_> {
    fn render(&self) -> Markup {
        self.to_html()
    }
}

impl Scribe for Message<'_> {
    fn render(self, res: &mut salvo::Response) {
        res.render(Text::Html(self.to_html().into_string()));
    }
}

impl<T: AsRef<Path>> From<T> for Page {
    fn from(value: T) -> Self {
        let path = value.as_ref();

        if path.as_os_str().is_empty() {
            return Self::search("");
        }

        if path == "/" {
            return Self { view: View::Root };
        }

        if path.is_dir() {
            Self {
                view: View::Dir(path.to_path_buf()),
            }
        } else {
            // just treating everything else as a file
            Self {
                view: View::File(path.to_path_buf()),
            }
        }
    }
}

struct Breadcrumb {
    text: Markup,
    unscoped_path: Option<String>,
}

impl Breadcrumb {
    fn admin_root() -> Self {
        Self {
            text: html! {
                svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" fill="currentColor" viewBox="0 0 16 16" {
                    path d="M8.354 1.146a.5.5 0 0 0-.708 0l-6 6A.5.5 0 0 0 1.5 7.5v7a.5.5 0 0 0 .5.5h4.5a.5.5 0 0 0 .5-.5v-4h2v4a.5.5 0 0 0 .5.5H14a.5.5 0 0 0 .5-.5v-7a.5.5 0 0 0-.146-.354L13 5.793V2.5a.5.5 0 0 0-.5-.5h-1a.5.5 0 0 0-.5.5v1.293zM2.5 14V7.707l5.5-5.5 5.5 5.5V14H10v-4a.5.5 0 0 0-.5-.5h-3a.5.5 0 0 0-.5.5v4z" {}
                }
            },
            unscoped_path: Some("/".into()),
        }
    }
}

impl View {
    fn breadcrumbs(&self, access: &Access) -> Vec<Breadcrumb> {
        match self {
            View::Dir(p) | View::File(p) => {
                let mut parts = vec![];
                for path in p.ancestors() {
                    if !access.is_allowed(path) {
                        break;
                    }

                    let is_root = config::get().app.canonicalized_sources().contains(path);

                    parts.push(Breadcrumb {
                        text: html! {
                            (path.file_name()
                                .map(|f| f.to_string_lossy())
                                .unwrap_or(path.to_string_lossy()))
                        },
                        unscoped_path: Some(path.to_string_lossy().into()),
                    });

                    if is_root {
                        break;
                    }
                }

                if matches!(access, Access::Admin) {
                    parts.push(Breadcrumb::admin_root());
                }

                parts.into_iter().rev().collect()
            }
            View::Search(_) => {
                let mut parts = Vec::with_capacity(2);
                parts.push(match access {
                    Access::Admin => Breadcrumb::admin_root(),
                    Access::Shared(SharedAccess { path_buf, .. }) => Breadcrumb {
                        unscoped_path: Some(path_buf.to_string_lossy().into()),
                        text: html! {
                            (path_buf
                             .file_name()
                             .map(|f| f.to_string_lossy())
                             .unwrap_or(path_buf.to_string_lossy())
                            )
                        },
                    },
                });

                parts.push(Breadcrumb {
                    text: html! { "Search results" },
                    unscoped_path: None,
                });

                parts
            }
            View::Root => {
                vec![Breadcrumb::admin_root()]
            }
        }
    }

    fn path(&self) -> &str {
        match self {
            View::Dir(p) | View::File(p) => p.to_str().unwrap_or(""),
            View::Search(_) => "",
            View::Root => "/",
        }
    }
}

const BYTE_UNITS: &[&str] = &["bytes", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
const STEP: f64 = 1024.0;

fn format_size(bytes: i64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let mut value = bytes as f64;
    let mut i = 0;

    while value >= STEP && i < BYTE_UNITS.len() - 1 {
        value /= STEP;
        i += 1;
    }

    let value = format!("{value:.1}");
    let value = value.trim_end_matches(".0");
    [value, BYTE_UNITS[i]].join(" ")
}

fn queries_to_string(queries: &MultiMap<String, String>) -> String {
    if queries.is_empty() {
        return String::new();
    }

    let mut ser = form_urlencoded::Serializer::new(String::new());
    for (k, values) in queries.iter_all() {
        for v in values {
            ser.append_pair(k, v);
        }
    }
    format!("?{}", ser.finish())
}

pub(crate) fn layout(
    title: &str,
    restore_from_history: bool,
    view: &Markup,
    access: &Access,
) -> Markup {
    html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                link rel="stylesheet" href={ "https://cdn.jsdelivr.net/npm/@picocss/pico@2/css/" (config::get().app.theme) ".min.css" };
                link rel="stylesheet" href="/static/app.css";
                script src="https://cdn.jsdelivr.net/npm/htmx.org@next/dist/htmx.min.js" {}
                script src="https://cdn.jsdelivr.net/npm/htmx.org@next/dist/ext/hx-sse.min.js" {}
                script src="https://cdn.jsdelivr.net/npm/hls.js@1/dist/hls.min.js" {}
                script defer src="https://cdn.jsdelivr.net/npm/alpinejs@3.x.x/dist/cdn.min.js" {}
                script src="/static/app.js" {}

                title { (title) }
            }
            body {
                main #container.container-fluid x-data="{ search_q: new URLSearchParams(location.search).get('q') }" {
                    form #search hx-get=(access_base_url("/search", access)) hx-push-url="true" hx-target="#view" hx-swap="innerHTML show:top showTarget:#container" {
                        input type="search" x-model="search_q" name="q" placeholder="Search...";
                        @if matches!(access, Access::Admin) {
                            a href="/logout" hx-delete="/logout" hx-target="#container" {
                                svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" fill="currentColor" viewBox="0 0 16 16" {
                                    path d="M7.5 1v7h1V1z" {}
                                    path d="M3 8.812a5 5 0 0 1 2.578-4.375l-.485-.874A6 6 0 1 0 11 3.616l-.501.865A5 5 0 1 1 3 8.812" {}
                                }
                            }
                        }
                    }
                    section #view {
                        { (view) }
                        @if restore_from_history {
                            script { (PreEscaped("setTimeout(() => { htmx.process(document.body) }, 10)")) }
                        }
                    }
                }
                div #modal {}
            }
        }
    }
}

pub(crate) fn login(message: Option<Message>, username: Option<&String>) -> Markup {
    html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                link rel="stylesheet" href={ "https://cdn.jsdelivr.net/npm/@picocss/pico@2/css/" (config::get().app.theme) ".min.css" };
                link rel="stylesheet" href="/static/app.css";
                title { "Login to Fayls" }
            }
            body {
                dialog open {
                    article {
                        header {
                            strong { "Login" }
                        }
                        @if message.is_some() {
                            (message.unwrap())
                        }
                        form action="/login" method="POST" {
                            input type="text" autofocus required name="username" value=[username] placeholder="user";
                            input type="password" required name="password" placeholder="password";
                            input type="submit" value="Login";
                        }
                    }
                }
            }
        }
    }
}

pub(crate) async fn docx_frame(path: &AuthorizedPath) -> AppResult<Markup> {
    let contents = tokio::fs::read(path)
        .await
        .map_err(|e| anyhow::anyhow!("can't read file {e}"))?;

    Ok(html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                script src="https://unpkg.com/jszip/dist/jszip.min.js" {}
                script src="https://unpkg.com/docx-preview/dist/docx-preview.min.js" {}
                title { (path.as_ref().to_str().unwrap_or_default()) }
                script type="text/javascript" {
                    (PreEscaped("
                        document.addEventListener('DOMContentLoaded', (_) => {
                            const template = document.body.querySelector('template');
                            const contents = template.content.textContent.trim();
                            const blob = Uint8Array.fromBase64(contents);
                            const opts = { useBase64URL: true };
                            docx.renderAsync(blob, main, null, opts).then(() => {
                                main.style = 'display: unset';
                                main.querySelector('.docx-wrapper').style = 'background: var(--bg)'
                            });
                        });
                    "))
                }
                style {
                    (PreEscaped("
                        :root {
                          --bg: #e7eaf0;
                          @media (prefers-color-scheme: dark) {
                            --bg: #202632;
                          }
                        }
                        main { display: none }
                        body {
                            background: var(--bg);
                            margin: 0;
                        }
                    "))
                }
            }
            body {
                template {
                    (STANDARD.encode(contents))
                }
                main #main {}
            }
        }
    })
}

pub(crate) async fn page(page: Page, req: &Request, access: &Access) -> AppResult<Markup> {
    Ok(match &page.view {
        View::Root => {
            let (sort, order) = get_sorting(req);

            let roots = config::get()
                .app
                .canonicalized_sources()
                .iter()
                .filter_map(|s| s.parent().and_then(|p| p.to_str()))
                .collect::<Vec<&str>>();

            let items = db::list_paths(&roots, &sort, &order).await?;

            file_list(&page.view, &items, get_progress().await?, req, access)
        }
        View::Dir(path_buf) => {
            let (sort, order) = get_sorting(req);
            let items = db::list_paths(&[&path_buf.to_string_lossy()], &sort, &order).await?;
            file_list(&page.view, &items, get_progress().await?, req, access)
        }
        View::File(path_buf) => file_view(&page.view, path_buf, req, access).await?,
        View::Search(term) => {
            let items = db::search(term, access).await?;

            file_list(&page.view, &items, get_progress().await?, req, access)
        }
    })
}

fn file_list_header(col: &Sort, sort: &Sort, order: &Order, req: &Request) -> Markup {
    let mut queries = req.queries().clone();
    let (asc, desc) = if sort == col {
        queries.remove("order");
        queries.insert("order".into(), order.reverse().as_str().into());
        (*order == Order::Asc, *order == Order::Desc)
    } else {
        queries.remove("sort");
        queries.insert("sort".into(), col.as_str().into());
        queries.remove("order");
        queries.insert("order".into(), "asc".into());
        (false, false)
    };

    html! {
        th.(col.as_str()).asc[asc].desc[desc] hx-push-url="true" hx-target="#view" hx-swap="innerHTML show:top showTarget:#container" hx-get=(queries_to_string(&queries)) {
            svg viewBox="0 0 80 80" fill="none" xmlns="http://www.w3.org/2000/svg" {
                path d="M49.0131 36L30.9126 36C29.0861 36 28.1713 33.7916 29.4629 32.5L38.1067 23.8562C39.1319 22.831 40.7939 22.831 41.819 23.8562L50.4629 32.5C51.7545 33.7916 50.8397 36 49.0131 36Z" fill="currentColor" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="asc" {}
                path d="M49.0131 44L30.9126 44C29.0861 44 28.1713 46.2084 29.4629 47.5L38.1067 56.1438C39.1319 57.169 40.7939 57.169 41.819 56.1438L50.4629 47.5C51.7545 46.2084 50.8397 44 49.0131 44Z" fill="currentColor" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="desc" {}
            }
            (col.humanize())
        }
    }
}

fn file_row_class(record: &ExistingPathRecord) -> String {
    match record.kind {
        PathRecordKind::Directory => "folder".into(),
        PathRecordKind::Symlink => "symlink".into(),
        PathRecordKind::File => record
            .name
            .split('.')
            .next_back()
            .map_or("file".into(), |e| format!("ext-{e}"))
            .to_lowercase(),
    }
}

fn breadcrumbs(view: &View, query_string: &str, access: &Access) -> Markup {
    let crumbs = view.breadcrumbs(access);

    html! {
        @if !crumbs.is_empty() {
            nav {
                ul #breadcrumbs {
                    @for crumb in crumbs {
                        @let link = files_url(&format!("{}{}", crumb.unscoped_path.unwrap_or_default(), query_string), access);
                        li {
                            a href=(link) hx-get=(link) x-on:click="search_q = ''" hx-target="#view" hx-swap="innerHTML show:top showTarget:#container" hx-push-url="true" {
                                (crumb.text)
                            }
                        }
                    }
                }
                @if matches!(view, View::File(_) | View::Dir(_)) {
                    details.dropdown x-ref="dropdown" {
                        summary {
                            i {}
                        }
                        ul {
                            @if matches!(access, Access::Admin) {
                                li {
                                    @let link = format!("/share?path={}", view.path());
                                    a.share x-on:click="$refs.dropdown.open = false" href=(link) hx-get=(link) hx-target="#modal" {
                                        i {}
                                        "Share"
                                    }
                                }
                            }
                            li {
                                a.download x-on:click="$refs.dropdown.open = false" href=(download_url(view.path(), access)) {
                                    i {}
                                    "Download"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn file_list(
    view: &View,
    files: &[ExistingPathRecord],
    progress: (i64, i64),
    req: &Request,
    access: &Access,
) -> Markup {
    let show_full_paths = matches!(view, &View::Search(_));

    let mut queries_without_search_param = req.queries().clone();
    queries_without_search_param.remove("q");
    let query_string = queries_to_string(&queries_without_search_param);

    let mut total_files = 0;
    let mut total_dirs = 0;
    let mut total_size = 0;

    for f in files {
        total_size += f.size;
        match f.kind {
            PathRecordKind::Directory => total_dirs += 1,
            PathRecordKind::File => total_files += 1,
            PathRecordKind::Symlink => {}
        }
    }

    html! {
        (breadcrumbs(view, &query_string, access))

        table.striped hx-get={ (files_url(view.path(), access)) (&query_string) } hx-trigger="reload-view" hx-target="#view" {
            thead hx-sse:connect={ (sse_url(view.path(), access)) } hx-trigger="load delay:1s" hx-config="ws.pauseOnBackground: false" {
                tr {
                    th { }
                    @let (sort, order) = get_sorting(req);
                    (file_list_header(&Sort::Name, &sort, &order, req))
                    (file_list_header(&Sort::Size, &sort, &order, req))
                    (file_list_header(&Sort::LastModified, &sort, &order, req))
                }
            }
            tbody {
                @if files.is_empty() {
                    tr.empty {
                        td {}
                        td colspan="3" { "Empty" }
                    }
                } @else {
                    @for file in files {
                        @let link = format!("{}{}", files_url(&file.path_buf().to_string_lossy().clone(), access), &query_string);
                        (row(file, &link, show_full_paths, access))
                    }
                }
            }
        }
        footer {
            (total_dirs)
                @if total_dirs == 1 {
                    " folder, "
                } @else {
                    " folders, "
                }
            (total_files)
                @if total_files == 1 {
                    " file, "
                } @else {
                    " files, "
                }
            (format_size(total_size)) " total "

            @if matches!(access, Access::Admin) {
                span #index-progress {
                    (index_progress(progress))
                }
            }
        }
    }
}

fn row(record: &ExistingPathRecord, link: &str, show_full_paths: bool, access: &Access) -> Markup {
    html! {
        tr.(file_row_class(record)) x-on:click="search_q = ''" hx-get=(link) hx-target="#view" hx-swap="innerHTML show:top showTarget:#container" hx-push-url="true" {
            td.icon { i {} }
            td.name {
                span {
                    (record.name)
                    @if show_full_paths {
                        em { (access_scoped_path(record.parent.as_deref().unwrap_or("/"), access)) }
                    }
                }
            }
            td.size { (format_size(record.size)) }
            td.last_modified {
                @let lastmod = record.last_modified.map_or(String::new(), |lm| lm.to_string());
                time x-data={ "{ time: timeAgo(" (lastmod) ") }" } x-text="time" datetime=(lastmod) { (lastmod) }
            }
        }
    }
}

pub(crate) fn index_progress((processed, total): (i64, i64)) -> Markup {
    if processed == total {
        return Markup::default();
    }

    html! {
        progress value=(processed) max=(total) title={"Content indexed for " (processed) " of " (total) " files"} {}
    }
}

async fn file_view(
    view: &View,
    file_path: &Path,
    req: &Request,
    access: &Access,
) -> AppResult<Markup> {
    let mut queries_without_search_param = req.queries().clone();
    queries_without_search_param.remove("q");
    let query_string = queries_to_string(&queries_without_search_param);

    ExistingPathRecord::find_by_path(file_path)
        .await?
        .ok_or(Error::NotFound)?;

    Ok(html! {
        (breadcrumbs(view, &query_string, access))
        section #file-contents { (preview_file(file_path, access).await? ) }
    })
}

async fn preview_file(file_path: &Path, access: &Access) -> AppResult<Markup> {
    let f = file_path.to_string_lossy();
    let ext = file_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("attempt to read as utf8");

    let markup = match ext.to_ascii_lowercase().as_ref() {
        "png" | "jpeg" | "jpg" | "gif" | "webp" | "svg" | "bmp" => {
            html! {
                img src={ (preview_url(f.as_ref(), access)) };
            }
        }
        "html" => {
            html! {
                iframe sandbox width="100%" height="100%" src={ (preview_url(f.as_ref(), access)) } {}
            }
        }
        "pdf" | "docx" => {
            html! {
                iframe width="100%" height="100%" src={ (preview_url(f.as_ref(), access)) } {}
            }
        }
        "epub" => {
            html! {
                iframe width="100%" height="100%" src={ "/static/vendor/epub/index.html?book=" (preview_url(f.as_ref(), access)) "&force_inline=true" } {}
            }
        }
        ext if is_video_file_extension(ext) => {
            if should_stream_directly(file_path).await.unwrap_or(false) {
                html! {
                    video controls preload="metadata" src={ (preview_url(f.as_ref(), access)) } {}
                }
            } else {
                html! {
                    video controls preload="metadata"
                        x-ref="video"
                        x-init="hls($refs.video, src)"
                        x-data={"{ src: '" (preview_hls_url(f.as_ref(), "master", access)) "'}"} {}
                }
            }
        }
        ext if is_audio_file_extension(ext) => {
            if should_stream_directly(file_path).await.unwrap_or(false) {
                html! {
                    audio controls preload="metadata" src={ (preview_url(f.as_ref(), access)) } {}
                }
            } else {
                html! {
                    audio controls preload="metadata"
                        x-ref="audio"
                        x-init="hls($refs.audio, src)"
                        x-data={"{ src: '" (preview_hls_url(f.as_ref(), "audio", access)) "'}"} {}
                }
            }
        }
        _ => {
            let filesize = file_path.metadata().map_or(0, |m| m.len());
            if filesize > config::get().preview.max_unknown_file_size {
                return Ok(no_preview(f.as_ref(), access));
            }

            if let Ok(contents) = tokio::fs::read_to_string(file_path).await {
                html! {
                    pre {
                        code { (contents) }
                    }
                }
            } else {
                no_preview(f.as_ref(), access)
            }
        }
    };

    Ok(markup)
}

fn no_preview(f: &str, access: &Access) -> Markup {
    html! {
        div.no-preview {
            h4 { "No preview is available for this file." }
            p {
                button href={ (download_url(f, access)) } { "Download" }
            }
        }
    }
}

pub(crate) fn share_modal(
    path_record: &ExistingPathRecord,
    share_record: &NewShareRecord,
) -> Markup {
    let base_url = config::get().app.share_url.as_deref().unwrap_or("");
    let data = format!(
        r"
        {{
            get base_url() {{ return '{}' || window.location.origin }},
            expiry: '',
            url: '{}',
            password: '',
            protected: false,
            get expires_at() {{
                if (this.expiry === '') {{
                    return null
                }}
                const epoch_ms = new Date(this.expiry + 'T23:59:59.000Z').getTime();
                if (!isNaN(epoch_ms)) {{
                    return Math.floor(epoch_ms / 1000);
                }}
                return null;
            }}
        }}
    ",
        base_url, &share_record.url,
    );
    html! {
        dialog open x-ref="modal" x-data=(data) {
            article "@click.outside"="$refs.modal.close()" {
                header {
                    span {
                        "Share "
                        code { (path_record.name) }
                    }
                    button aria-label="Close" rel="prev" x-on:click="$refs.modal.close()" {}
                }
                div #share-message {}
                form action="/share" hx-post="/share" hx-target="#modal" id="share-form" {
                    input type="hidden" name="path_id" value=(share_record.path_id);
                    label {
                        "URL"
                        input name="url" required type="text" x-model="url";
                        small {
                            span x-text="base_url" { (base_url) }
                            "/shared/link/"
                            span x-text="url";
                        }
                    }
                    label {
                        "Expire after"
                        template x-if="expires_at !== null" {
                            input name="expires_at" type="hidden" x-model="expires_at";
                        }
                        input type="date" x-model="expiry";
                    }
                    label {
                        input x-on:change="protected = !protected" type="checkbox" role="switch";
                        "Require password"
                    }
                    template x-if="protected" {
                        label {
                            input name="password" required type="password" placeholder="Access password";
                        }
                    }
                }
                footer {
                    button form="share-form" { "Create link" }
                }
            }
        }
    }
}

pub(crate) fn shares(
    path_record: &ExistingPathRecord,
    shares: &[ExistingShareRecord],
    message: Option<Message>,
) -> Markup {
    let base_url = config::get().app.share_url.as_deref().unwrap_or("");
    html! {
        dialog open x-ref="modal" x-data="{}" {
            article "@click.outside"="$refs.modal.close()" {
                header {
                    span {
                        "Shared links for "
                        code { (path_record.name) }
                    }
                    button aria-label="Close" rel="prev" x-on:click="$refs.modal.close()" {}
                }
                div #share-message {
                    @if let Some(m) = message {
                        (m)
                    }
                }
                @if shares.is_empty() {
                    em.text-muted { "No share links found." }
                } @else {
                    table.share-links x-data={"{get base_url() { return '" (base_url) "' || window.location.origin }}"} {
                        @let copy = r"
                            (async () => {
                                navigator.clipboard.writeText(base_url + url);
                                $refs.copy.dataset.tooltip = 'Copied!';
                                setTimeout(() => {
                                    $refs.copy.dataset.tooltip = 'Copy link';
                                }, 500)
                            })()
                        ";
                        @for share in shares {
                            tr x-data={"{url: '/shared/link/" (share.url) "' }"} {
                                td {
                                    span x-text="base_url" { (base_url) }
                                    span x-text="url" { (share.url)}
                                    br;

                                    small.text-muted {
                                        code {
                                            "accessed " (share.accessed) " times"
                                        }
                                        @if share.password.is_some() {
                                            " "
                                                code { "protected" }
                                        }
                                        @if let Some(time) = share.expires_at {
                                            " "
                                                code {
                                                    "expires "
                                                        time x-data={ "{ time: time(" (time) ") }" } x-text="time" datetime=(time) { (time) }
                                                }
                                        }
                                    }
                                }
                                td.actions {
                                    a.copy x-ref="copy" data-tooltip="Copy link" href="#" "@click.prevent"=(copy) { i {}}
                                    a.delete data-tooltip="Delete" href="#" hx-confirm="Are you sure?" hx-delete={ "/share/" (share.url) } hx-target="#modal" { i {}}
                                }
                            }
                        }
                    }
                }
                footer {
                    button.outline hx-get={"/share?add=true&path=" (path_record.path_buf().display())} hx-target="#modal" {
                        "+ Add shared link "
                    }
                }
            }
        }
    }
}

pub(crate) fn shared_link_password(
    record: &ExistingShareRecord,
    message: Option<Message>,
) -> Markup {
    html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                link rel="stylesheet" href={ "https://cdn.jsdelivr.net/npm/@picocss/pico@2/css/" (config::get().app.theme) ".min.css" };
                link rel="stylesheet" href="/static/app.css";
                title { "Password required" }
            }
            body {
                dialog open {
                    article {
                        header {
                            strong { "Password required" }
                        }
                        @if message.is_some() {
                            (message.unwrap())
                        }
                        form action={"/shared/link/" (record.url)} method="POST" {
                            input type="password" autofocus name="password" required placeholder="password";
                            input type="submit" value="Continue";
                        }
                    }
                }
            }
        }
    }
}
