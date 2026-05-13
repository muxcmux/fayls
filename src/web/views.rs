use crate::config;
use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};

use base64_turbo::URL_SAFE_NO_PAD;
use maud::{DOCTYPE, Markup, html};
use multimap::MultiMap;
use salvo::Request;

use crate::{
    path_indexing::{ExistingPathRecord, PathRecordKind},
    web::{self, Order, Sort},
};

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

pub fn layout(title: &str, file_list: &Markup) -> Markup {
    html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/@picocss/pico@2/css/pico.fluid.classless.min.css";
                link rel="stylesheet" href="/static/app.css";
                script src="https://cdn.jsdelivr.net/npm/htmx.org@next/dist/htmx.min.js" {}
                script src="https://cdn.jsdelivr.net/npm/htmx.org@next/dist/ext/hx-sse.min.js" {}
                script defer src="https://cdn.jsdelivr.net/npm/alpinejs@3.x.x/dist/cdn.min.js" {}
                script src="/static/app.js" {}

                title { (title) }
            }
            body {
                main x-data="{ search_q: new URLSearchParams(location.search).get('q') }" {
                    form hx-get="/search" hx-push-url="true" hx-target="#file-list" {
                        input type="search" x-model="search_q" name="q" placeholder="Search...";
                    }
                    section #file-list {
                        { (file_list) }
                    }
                }
            }
        }
    }
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
        th.(col.as_str()).asc[asc].desc[desc] hx-push-url="true" hx-target="#file-list" hx-get=(queries_to_string(&queries)) {
            svg viewBox="0 0 80 80" fill="none" xmlns="http://www.w3.org/2000/svg" {
                path d="M49.0131 36L30.9126 36C29.0861 36 28.1713 33.7916 29.4629 32.5L38.1067 23.8562C39.1319 22.831 40.7939 22.831 41.819 23.8562L50.4629 32.5C51.7545 33.7916 50.8397 36 49.0131 36Z" fill="currentColor" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="asc" {}
                path d="M49.0131 44L30.9126 44C29.0861 44 28.1713 46.2084 29.4629 47.5L38.1067 56.1438C39.1319 57.169 40.7939 57.169 41.819 56.1438L50.4629 47.5C51.7545 46.2084 50.8397 44 49.0131 44Z" fill="currentColor" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="desc" {}
            }
            (col.humanize())
        }
    }
}

fn file_row_class(record: &ExistingPathRecord) -> String {
    if record.kind == PathRecordKind::Directory {
        "folder".into()
    } else {
        record
            .name
            .split('.')
            .next_back()
            .map_or("file".into(), |e| format!("ext-{e}"))
            .to_lowercase()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum View {
    Root,
    Path(PathBuf),
    Search,
}

impl<'de> serde::Deserialize<'de> for View {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;

        if encoded.is_empty() {
            return Ok(View::Search);
        }

        let decoded = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(serde::de::Error::custom)?;

        let path = PathBuf::from(OsString::from_vec(decoded));

        if path == *"/" {
            Ok(View::Root)
        } else {
            Ok(View::Path(path))
        }
    }
}

impl View {
    fn breadcrumbs(&self) -> Vec<PathBuf> {
        match self {
            View::Path(p) => {
                let mut path_buf = Some(p.clone());
                let mut parts = vec![];
                while let Some(path) = path_buf {
                    let is_root = config::get().app.canonicalized_sources().contains(&path);
                    path_buf = path.parent().map(std::path::Path::to_path_buf);
                    parts.push(path);

                    if is_root {
                        break;
                    }
                }
                parts.into_iter().rev().collect()
            }
            View::Search => vec![PathBuf::from("/Search results")],
            View::Root => vec![],
        }
    }

    fn encode(&self) -> String {
        match self {
            View::Path(p) => format!("/{}", URL_SAFE_NO_PAD.encode(p.to_string_lossy().as_ref())),
            View::Search => String::new(),
            // "/"
            View::Root => "/Lw".into(),
        }
    }
}

pub fn file_list(
    folder: &View,
    files: &[ExistingPathRecord],
    progress: (i64, i64),
    req: &Request,
) -> Markup {
    let show_full_paths = folder == &View::Search;
    let crumbs = folder.breadcrumbs();

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
        @if !crumbs.is_empty() {
            nav {
                ul {
                    li {
                        a href={ "/" (query_string) } x-on:click="search_q = ''" hx-get={ "/" (&query_string) } hx-target="#file-list" hx-push-url="true" {
                            svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" fill="currentColor" viewBox="0 0 16 16" {
                                path d="M8.354 1.146a.5.5 0 0 0-.708 0l-6 6A.5.5 0 0 0 1.5 7.5v7a.5.5 0 0 0 .5.5h4.5a.5.5 0 0 0 .5-.5v-4h2v4a.5.5 0 0 0 .5.5H14a.5.5 0 0 0 .5-.5v-7a.5.5 0 0 0-.146-.354L13 5.793V2.5a.5.5 0 0 0-.5-.5h-1a.5.5 0 0 0-.5.5v1.293zM2.5 14V7.707l5.5-5.5 5.5 5.5V14H10v-4a.5.5 0 0 0-.5-.5h-3a.5.5 0 0 0-.5.5v4z" {}
                            }
                        }
                    }
                    @for path in crumbs {
                        @let link = format!("/files{}{}", path.to_string_lossy(), &query_string);
                        li {
                            a href=(link) hx-get=(link) hx-target="#file-list" hx-push-url="true" {
                                (path.file_name().map_or(String::new(), |f| f.to_string_lossy().to_string()))
                            }
                        }
                    }
                }
            }
        }
        table hx-sse:connect={ "/sse" (folder.encode()) } hx-trigger="load delay:1s" hx-config="ws.pauseOnBackground: false" {
            thead {
                tr {
                    th { }
                    @let (sort, order) = web::get_sorting(req);
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
                        tr x-on:click="search_q = ''" hx-get=(link) hx-target="#file-list" hx-push-url="true" {
                            (row(file, show_full_paths))
                        }
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

pub fn row(record: &ExistingPathRecord, show_full_paths: bool) -> Markup {
    html! {
        td.icon { i.(file_row_class(record)) {} }
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

pub fn index_progress((processed, total): (i64, i64)) -> Markup {
    if processed == total {
        return Markup::default();
    }

    html! {
        progress value=(processed) max=(total) title={"Content indexed for " (processed) " of " (total) " files"} {}
    }
}
