use maud::{DOCTYPE, Markup, html};

use crate::fayls::{ExistingFayl, FaylKind};

pub fn layout(title: &str, content: &Markup) -> Markup {
    html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/@picocss/pico@2/css/pico.fluid.classless.min.css";
                link rel="stylesheet" href="/static/app.css";
                script src="https://cdn.jsdelivr.net/npm/htmx.org@4.0.0-beta2" integrity="sha384-v+EMKtNUAo5enmQxBqgoU/FWvVvvZHvITNzurHSl4kzvCs94wdlgHUci1lliKWKz" crossorigin="anonymous" {}
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

pub fn file_list(files: &Vec<ExistingFayl>) -> Markup {
    html! {
        table.file_list {
            thead {
                tr {
                    th { "Name" }
                    th { "Size" }
                    th { "Last Modified" }
                }
            }
            tbody {
                @for file in files {
                    tr {
                        td {
                            @if file.kind == FaylKind::Directory {
                                @let link = format!("/files{}/{}", file.parent.as_ref().unwrap_or(&String::new()), file.name);
                                a hx-boost="true" href=(link) { (file.name) }
                            } @else {
                                (file.name)
                            }
                        }
                        td { (file.size) }
                        td {
                            @if let Some(last_modified) = file.last_modified {
                                time datetime=(last_modified) { (last_modified) }
                            }
                        }
                    }
                }
            }
        }
    }
}
