use maud::{DOCTYPE, Markup, html};

use crate::{fayls::ExistingFayl, utils};

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

pub fn file_list(files: &[ExistingFayl]) -> Markup {
    html! {
        section #file_list {
            table {
                thead {
                    tr {
                        th.icon { "" }
                        th.name { "Name" }
                        th.size { "Size" }
                        th.date { "Last Modified" }
                    }
                }
                tbody {
                    @for file in files {
                        @let link = format!("/files{}/{}", file.parent.as_ref().unwrap_or(&String::new()), file.name);
                        tr hx-get=(link) hx-target="#file_list" hx-push-url="true" {
                            td.icon { (utils::fayl_icon(file)) }
                            td.name { span { (file.name) } }
                            td.size { (utils::format_size(file.size)) }
                            td.date {
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
