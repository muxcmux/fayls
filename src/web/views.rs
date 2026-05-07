use std::path::PathBuf;

use maud::{DOCTYPE, Markup, html};
use multimap::MultiMap;

use crate::{
    fayls::{ExistingFayl, FaylKind},
    utils,
    web::{Order, Sort},
};

pub fn layout(title: &str, content: &Markup) -> Markup {
    html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/@picocss/pico@2/css/pico.fluid.classless.min.css";
                link rel="stylesheet" href="/static/app.css";
                script defer src="https://cdn.jsdelivr.net/npm/htmx.org@4.0.0-beta2" integrity="sha384-v+EMKtNUAo5enmQxBqgoU/FWvVvvZHvITNzurHSl4kzvCs94wdlgHUci1lliKWKz" crossorigin="anonymous" {}
                script defer src="https://cdn.jsdelivr.net/npm/alpinejs@3.x.x/dist/cdn.min.js" {}
                script src="/static/app.js" {}

                title { (title) }
            }
            body {
                main {
                    form hx-boost="true" action="/search" method="get" {
                        input type="search" name="q" placeholder="Search...";
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
        th.(col.as_str()).asc[asc].desc[desc] hx-push-url="true" hx-target="#file_list" hx-swap="outerHTML" hx-get=(utils::queries_to_string(&queries)) {
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
    queries: &MultiMap<String, String>,
) -> Markup {
    let query_string = utils::queries_to_string(queries);

    html! {
        section #file_list {
            @if !crumbs.is_empty() {
                nav {
                    ul {
                        li {
                            a href={ "/" (&query_string) } hx-get={ "/" (&query_string) } hx-target="#file_list" hx-swap="outerHTML" hx-push-url="true" {
                                "/"
                            }
                        }
                        @for path in crumbs {
                            @let link = format!("/files{}{}", path.to_string_lossy(), &query_string);
                            li {
                                a href=(link) hx-get=(link) hx-target="#file_list" hx-swap="outerHTML" hx-push-url="true" {
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
                    @for file in files {
                        @let link = format!("/files{}/{}{}", file.parent.as_ref().unwrap_or(&String::new()), file.name, &query_string);
                        tr hx-get=(link) hx-target="#file_list" hx-swap="outerHTML" hx-push-url="true" {
                            td.icon { i.(file_row_class(file)) {} }
                            td.name { span { (file.name) } }
                            td.size { (utils::format_size(file.size)) }
                            td.last_modified {
                                @let lastmod = file.last_modified.map_or(String::new(), |lm| lm.to_string());
                                time x-data={ "{ time: timeAgo(" (lastmod) ") }" } x-text="time" datetime=(lastmod) { (lastmod) }
                            }
                        }
                    }
                }
            }
        }
    }
}
