use std::path::PathBuf;

use maud::{DOCTYPE, Markup, html};
use multimap::MultiMap;

use crate::{
    fayls::{ExistingFayl, FaylKind},
    web::{Order, Sort},
};

const BYTE_UNITS: &[&str] = &["bytes", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
const STEP: f64 = 1024.0;

fn format_size(bytes: i64) -> String {
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

pub fn layout(title: &str, content: &Markup) -> Markup {
    html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/@picocss/pico@2/css/pico.fluid.classless.min.css";
                link rel="stylesheet" href="/static/app.css";
                script defer src="https://cdn.jsdelivr.net/npm/htmx.org@next/dist/htmx.min.js" {}
                script defer src="https://cdn.jsdelivr.net/npm/htmx.org@next/dist/ext/hx-ws.min.js" {}
                script defer src="https://cdn.jsdelivr.net/npm/alpinejs@3.x.x/dist/cdn.min.js" {}
                script src="/static/app.js" {}

                title { (title) }
            }
            body {
                main x-data="{ search_q: new URLSearchParams(location.search).get('q') }" {
                    form hx-get="/search" hx-push-url="true" hx-target="#file-list" hx-swap="outerHTML" {
                        input type="search" x-model="search_q" name="q" placeholder="Search...";
                    }
                    { (content) }
                }
            }
        }
    }
}

fn file_list_header(
    col: &Sort,
    sort: &Sort,
    order: &Order,
    mut queries: MultiMap<String, String>,
) -> Markup {
    let (asc, desc) = if sort == col {
        queries.remove("order");
        queries.insert("order".into(), order.reverse().as_str().into());
        (order == &Order::Asc, order == &Order::Desc)
    } else {
        queries.remove("sort");
        queries.insert("sort".into(), col.as_str().into());
        queries.remove("order");
        queries.insert("order".into(), "asc".into());
        (false, false)
    };

    html! {
        th.(col.as_str()).asc[asc].desc[desc] hx-push-url="true" hx-target="#file-list" hx-swap="outerHTML" hx-get=(queries_to_string(&queries)) {
            svg viewBox="0 0 80 80" fill="none" xmlns="http://www.w3.org/2000/svg" {
                path d="M49.0131 36L30.9126 36C29.0861 36 28.1713 33.7916 29.4629 32.5L38.1067 23.8562C39.1319 22.831 40.7939 22.831 41.819 23.8562L50.4629 32.5C51.7545 33.7916 50.8397 36 49.0131 36Z" fill="currentColor" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="asc" {}
                path d="M49.0131 44L30.9126 44C29.0861 44 28.1713 46.2084 29.4629 47.5L38.1067 56.1438C39.1319 57.169 40.7939 57.169 41.819 56.1438L50.4629 47.5C51.7545 46.2084 50.8397 44 49.0131 44Z" fill="currentColor" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="desc" {}
            }
            (col.humanize())
        }
    }
}

fn file_row_class(fayl: &ExistingFayl) -> String {
    if fayl.kind == FaylKind::Directory {
        "folder".into()
    } else {
        fayl.name
            .split('.')
            .next_back()
            .map_or("file".into(), |e| format!("ext-{e}"))
            .to_lowercase()
    }
}

pub fn file_list(
    files: &[ExistingFayl],
    sort: &Sort,
    order: &Order,
    crumbs: &[PathBuf],
    show_full_paths: bool,
    progress: (i64, i64),
    queries: &MultiMap<String, String>,
) -> Markup {
    let mut queries_without_search_param = queries.clone();
    queries_without_search_param.remove("q");
    let query_string = queries_to_string(&queries_without_search_param);

    let mut total_files = 0;
    let mut total_dirs = 0;
    let mut total_size = 0;

    for f in files {
        total_size += f.size;
        match f.kind {
            FaylKind::Directory => total_dirs += 1,
            FaylKind::File => total_files += 1,
            FaylKind::Symlink => {}
        }
    }

    html! {
        section #file-list {
            @if !crumbs.is_empty() {
                nav {
                    ul {
                        li {
                            a href={ "/" (&query_string) } x-on:click="search_q = ''" hx-get={ "/" (&query_string) } hx-target="#file-list" hx-swap="outerHTML" hx-push-url="true" {
                                svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" fill="currentColor" viewBox="0 0 16 16" {
                                    path d="M8.354 1.146a.5.5 0 0 0-.708 0l-6 6A.5.5 0 0 0 1.5 7.5v7a.5.5 0 0 0 .5.5h4.5a.5.5 0 0 0 .5-.5v-4h2v4a.5.5 0 0 0 .5.5H14a.5.5 0 0 0 .5-.5v-7a.5.5 0 0 0-.146-.354L13 5.793V2.5a.5.5 0 0 0-.5-.5h-1a.5.5 0 0 0-.5.5v1.293zM2.5 14V7.707l5.5-5.5 5.5 5.5V14H10v-4a.5.5 0 0 0-.5-.5h-3a.5.5 0 0 0-.5.5v4z" {}
                                }
                            }
                        }
                        @for path in crumbs {
                            @let link = format!("/files{}{}", path.to_string_lossy(), &query_string);
                            li {
                                a href=(link) hx-get=(link) hx-target="#file-list" hx-swap="outerHTML" hx-push-url="true" {
                                    (path.file_name().map_or(String::new(), |f| f.to_string_lossy().to_string()))
                                }
                            }
                        }
                    }
                }
            }
            table {
                thead {
                    tr {
                        th { }
                        (file_list_header(&Sort::Name, sort, order, queries.clone()))
                        (file_list_header(&Sort::Size, sort, order, queries.clone()))
                        (file_list_header(&Sort::LastModified, sort, order, queries.clone()))
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
                            tr x-on:click="search_q = ''" hx-get=(link) hx-target="#file-list" hx-swap="outerHTML" hx-push-url="true" {
                                td.icon { i.(file_row_class(file)) {} }
                                td.name {
                                    span {
                                        (file.name)
                                        @if show_full_paths {
                                            em { (file.parent.as_deref().unwrap_or("")) }
                                        }
                                    }
                                }
                                td.size { (format_size(file.size)) }
                                td.last_modified {
                                    @let lastmod = file.last_modified.map_or(String::new(), |lm| lm.to_string());
                                    time x-data={ "{ time: timeAgo(" (lastmod) ") }" } x-text="time" datetime=(lastmod) { (lastmod) }
                                }
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
                span #index-progress hx-ws:connect="/ws" {
                    (index_progress(progress))
                }
            }
        }
    }
}

pub fn index_progress((processed, total): (i64, i64)) -> Markup {
    html! {
        progress value=(processed) max=(total) title={"Content indexed for " (processed) " of " (total) " files"} {}
    }
}
