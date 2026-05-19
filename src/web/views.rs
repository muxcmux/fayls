use crate::{
    app, config,
    db::{self, NewPathRecord},
    error::{AppResult, Error},
    indexing::get_progress,
    web::{Order, Sort, get_sorting},
};
use std::{
    ffi::OsString,
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
};

use base64_turbo::URL_SAFE_NO_PAD;
use maud::{DOCTYPE, Markup, PreEscaped, html};
use multimap::MultiMap;
use salvo::Request;

use crate::db::{ExistingPathRecord, PathRecordKind};

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

impl<'de> serde::Deserialize<'de> for Page {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;

        if encoded.is_empty() {
            return Ok(Self::search(""));
        }

        let decoded = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(serde::de::Error::custom)?;

        let path = PathBuf::from(OsString::from_vec(decoded));
        Ok(Self::from(path))
    }
}

impl View {
    fn breadcrumbs(&self) -> Vec<PathBuf> {
        match self {
            View::Dir(p) | View::File(p) => {
                let mut parts = vec![];
                for path in p.ancestors() {
                    let is_root = config::get().app.canonicalized_sources().contains(path);
                    parts.push(path);

                    if is_root {
                        break;
                    }
                }
                parts.into_iter().rev().map(PathBuf::from).collect()
            }
            View::Search(_) => vec![PathBuf::from("/Search results")],
            View::Root => vec![PathBuf::from("/")],
        }
    }

    fn as_str(&self) -> &str {
        match self {
            View::Dir(p) | View::File(p) => p.to_str().unwrap_or(""),
            View::Search(_) => "",
            View::Root => "/",
        }
    }

    fn encode(&self) -> String {
        format!("/{}", URL_SAFE_NO_PAD.encode(self.as_str()))
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

pub(crate) fn layout(title: &str, restore_from_history: bool, view: &Markup) -> Markup {
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
                script defer src="https://cdn.jsdelivr.net/npm/alpinejs@3.x.x/dist/cdn.min.js" {}
                script src="/static/app.js" {}

                title { (title) }
            }
            body {
                main.container-fluid x-data="{ search_q: new URLSearchParams(location.search).get('q') }" {
                    form hx-get="/search" hx-push-url="true" hx-target="#view" {
                        input type="search" x-model="search_q" name="q" placeholder="Search...";
                    }
                    section #view {
                        { (view) }
                        @if restore_from_history {
                            script { (PreEscaped("setTimeout(() => { htmx.process(document.body) }, 10)")) }
                        }
                    }
                }
            }
        }
    }
}

pub(crate) async fn page(page: Page, req: &Request) -> AppResult<Markup> {
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

            file_list(&page.view, &items, get_progress().await?, req)
        }
        View::Dir(path_buf) => {
            let (sort, order) = get_sorting(req);
            let items = db::list_paths(&[&path_buf.to_string_lossy()], &sort, &order).await?;
            file_list(&page.view, &items, get_progress().await?, req)
        }
        View::File(path_buf) => file_view(&page.view, path_buf, req).await?,
        View::Search(term) => {
            let items = db::search(term).await?;

            file_list(&page.view, &items, get_progress().await?, req)
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
        th.(col.as_str()).asc[asc].desc[desc] hx-push-url="true" hx-target="#view" hx-get=(queries_to_string(&queries)) {
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

fn breadcrumbs(view: &View, query_string: &str, after_list: Markup) -> Markup {
    let crumbs = view.breadcrumbs();

    html! {
        @if !crumbs.is_empty() {
            nav {
                ul #breadcrumbs {
                    li {
                        a href={ "/" (query_string) } x-on:click="search_q = ''" hx-get={ "/" (query_string) } hx-target="#view" hx-push-url="true" {
                            svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" fill="currentColor" viewBox="0 0 16 16" {
                                path d="M8.354 1.146a.5.5 0 0 0-.708 0l-6 6A.5.5 0 0 0 1.5 7.5v7a.5.5 0 0 0 .5.5h4.5a.5.5 0 0 0 .5-.5v-4h2v4a.5.5 0 0 0 .5.5H14a.5.5 0 0 0 .5-.5v-7a.5.5 0 0 0-.146-.354L13 5.793V2.5a.5.5 0 0 0-.5-.5h-1a.5.5 0 0 0-.5.5v1.293zM2.5 14V7.707l5.5-5.5 5.5 5.5V14H10v-4a.5.5 0 0 0-.5-.5h-3a.5.5 0 0 0-.5.5v4z" {}
                            }
                        }
                    }
                    @for path in crumbs {
                        @let link = format!("/files{}{}", path.to_string_lossy(), query_string);
                        li {
                            a href=(link) hx-get=(link) hx-target="#view" hx-push-url="true" {
                                (path.file_name().map_or(String::new(), |f| f.to_string_lossy().to_string()))
                            }
                        }
                    }
                }
                (after_list)
            }
        }
    }
}

fn file_list(
    view: &View,
    files: &[ExistingPathRecord],
    progress: (i64, i64),
    req: &Request,
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
        (breadcrumbs(view, &query_string, html!{}))

        table.striped hx-get={ "/files" (view.as_str()) (&query_string) } hx-trigger="reload-view" hx-target="#view" {
            thead hx-sse:connect={ "/sse" (view.encode()) } hx-trigger="load delay:1s" hx-config="ws.pauseOnBackground: false" {
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
                        @let link = format!("/files{}/{}{}", file.parent.as_ref().unwrap_or(&String::new()), file.name, &query_string);
                        (row(file, &link, show_full_paths))
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
            span #index-progress {
                (index_progress(progress))
            }
        }
    }
}

fn row(record: &ExistingPathRecord, link: &str, show_full_paths: bool) -> Markup {
    html! {
        tr.(file_row_class(record)) x-on:click="search_q = ''" hx-get=(link) hx-target="#view" hx-push-url="true" {
            td.icon { i {} }
            td.name {
                span {
                    (record.name)
                    @if show_full_paths {
                        em { (record.parent.as_deref().unwrap_or("")) }
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

fn file_dropdown_menu(file_path: &Path) -> Markup {
    html! {
        details.dropdown {
            summary {
                i {}
            }
            ul {
                li {
                    a.download href={ "/download?path=" (file_path.to_string_lossy()) } {
                        i {}
                        "Download"
                    }
                }
            }
        }
    }
}

async fn file_view(view: &View, file_path: &Path, req: &Request) -> AppResult<Markup> {
    let mut queries_without_search_param = req.queries().clone();
    queries_without_search_param.remove("q");
    let query_string = queries_to_string(&queries_without_search_param);

    NewPathRecord::from(file_path)
        .find_existing(app::db())
        .await?
        .ok_or_else(|| Error::NotFound)?;

    Ok(html! {
        (breadcrumbs(view, &query_string, file_dropdown_menu(file_path)))
        section #file-contents { (read_file(file_path).await? ) }
    })
}

async fn read_file(file_path: &Path) -> AppResult<Markup> {
    let f = file_path.to_string_lossy();
    let ext = file_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("attempt to read as utf8");

    let markup = match ext {
        "png" | "jpeg" | "jpg" | "gif" | "webp" | "svg" | "bmp" => {
            html! {
                img src={ "/read?path=" (f) };
            }
        }
        "html" => {
            html! {
                iframe sandbox width="100%" height="100%" src={ "/read?path=" (f) } {}
            }
        }
        "pdf" => {
            html! {
                iframe width="100%" height="100%" src={ "/read?path=" (f) } {}
            }
        }
        _ => {
            if let Ok(contents) = tokio::fs::read_to_string(file_path).await {
                html! {
                    pre {
                        code { (contents) }
                    }
                }
            } else {
                html! {
                    div {
                        h4 { "This file can't be viewed." }
                        p {
                            button href={ "/download?path=" (f) } { "Download" }
                        }
                    }
                }
            }
        }
    };

    Ok(markup)
}
