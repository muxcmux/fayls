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
use std::fmt::Write;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use base64_turbo::STANDARD;
use maud::{DOCTYPE, Markup, PreEscaped, Render, html};
use multimap::MultiMap;
use salvo::{Request, Scribe, writing::Text};

use crate::db::{ExistingPathRecord, PathRecordKind};

fn urlencode(query: &[(&str, &str)]) -> String {
    let mut ser = form_urlencoded::Serializer::new(String::new());
    for (k, v) in query {
        ser.append_pair(k, v);
    }
    ser.finish()
}

fn access_base_url(path: &str, access: &Access) -> String {
    match access {
        Access::Admin => path.into(),
        Access::Shared(_) => format!("/shared{path}"),
    }
}

fn access_url_with_query(endpoint: &str, query: &[(&str, &str)], access: &Access) -> String {
    access_base_url(&format!("{endpoint}?{}", urlencode(query)), access)
}

fn files_url(path: &str, access: &Access) -> String {
    access_base_url(
        &format!("/files{}", access_scoped_path(path, access)),
        access,
    )
}

fn download_url<'a>(
    path: &str,
    selected: impl Iterator<Item = &'a i64>,
    access: &Access,
) -> String {
    let path = access_scoped_path(path, access);
    let mut link = access_url_with_query("/download", &[("path", &path)], access);

    for id in selected {
        let _ = write!(link, "&id={id}");
    }

    link
}

fn unlink_url<'a>(path: &str, selected: impl Iterator<Item = &'a i64>) -> String {
    let mut link = format!("/unlink?{}", urlencode(&[("path", path)]));

    for id in selected {
        let _ = write!(link, "&id={id}");
    }

    link
}

fn path_context_menu_url(path: &str, access: &Access) -> String {
    let path = access_scoped_path(path, access);
    access_url_with_query("/path_context_menu", &[("path", &path)], access)
}

fn sse_url(path: &str, access: &Access) -> String {
    let path = access_scoped_path(path, access);
    access_url_with_query("/sse", &[("path", &path)], access)
}

fn preview_url(path: &str, access: &Access) -> String {
    let path = access_scoped_path(path, access);
    access_url_with_query("/preview", &[("path", &path)], access)
}

fn preview_hls_url(path: &str, hls: &str, access: &Access) -> String {
    let path = access_scoped_path(path, access);
    access_url_with_query("/preview", &[("path", &path), ("hls", hls)], access)
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
    Notification(&'a str),
    CloseModal,
}

impl Message<'_> {
    fn to_html(&self) -> Markup {
        match self {
            Self::CloseModal => html! {
                hx-partial hx-target="#modal" {}
            },
            Self::Error(msg) => html! {
                div.alert { (msg) }
            },
            Self::Success(msg) => html! {
                div.alert.success { (msg) }
            },
            Self::Notification(msg) => html! {
                hx-partial hx-target="#notification" hx-swap="outerHTML" {
                    div #notification x-ref="notification"
                        class="alert success" role="status"
                        x-data="{}"
                        x-init="setTimeout(() => { $refs.notification.hidden = true }, 5000)" {
                        span #notification-text { (msg) }
                        button type="button"
                        "@click"="$refs.notification.hidden = true"
                        aria-label="Dismiss notification" { "×" }
                    }
                }
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
                svg xmlns="http://www.w3.org/2000/svg" fill="currentColor" viewBox="0 0 16 16" {
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
                vec![]
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
                script type="importmap" {
                    (PreEscaped(r#"{
                        "imports": {
                            "@silurus/ooxml/docx": "https://cdn.jsdelivr.net/npm/@silurus/ooxml@0.89.7/dist/docx.mjs",
                            "@silurus/ooxml/pptx": "https://cdn.jsdelivr.net/npm/@silurus/ooxml@0.89.7/dist/pptx.mjs",
                            "@silurus/ooxml/internal/companion": "https://cdn.jsdelivr.net/npm/@silurus/ooxml@0.89.7/dist/internal-companion.mjs",
                            "@silurus/ooxml-legacy-office/doc": "https://cdn.jsdelivr.net/npm/@silurus/ooxml-legacy-office@0.89.7/dist/doc.mjs",
                            "@silurus/ooxml-legacy-office/ppt": "https://cdn.jsdelivr.net/npm/@silurus/ooxml-legacy-office@0.89.7/dist/ppt.mjs"
                        }
                    }"#))
                }
                script src="https://cdn.jsdelivr.net/npm/htmx.org@4.x.x/dist/htmx.min.js" {}
                script src="https://cdn.jsdelivr.net/npm/htmx.org@4.x.x/dist/ext/hx-sse.min.js" {}
                script src="https://cdn.jsdelivr.net/npm/hls.js@1/dist/hls.min.js" {}
                script src="https://cdn.jsdelivr.net/npm/dropzone@5.9.3/dist/min/dropzone.min.js" {}
                script defer src="https://cdn.jsdelivr.net/npm/alpinejs@3.x.x/dist/cdn.min.js" {}
                script src="/static/app.js" {}

                title { (title) }
            }
            body {
                div #request-progress aria-hidden="true" {
                    div.bar {}
                }
                div #request-error class="alert" role="alert" hidden {
                    span #request-error-text {}
                    button type="button" aria-label="Dismiss error" { "×" }
                }
                div #notification {}
                main #container.container-fluid x-data="{ search_q: new URLSearchParams(location.search).get('q'), multiselecting: false }" {
                    form #search hx-get=(access_base_url("/search", access)) hx-push-url="true" hx-target="#view" hx-swap="innerHTML show:top showTarget:#container" {
                        input type="search" x-model="search_q" name="q" placeholder="Search...";
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

pub(crate) fn path_context_menu(
    target: &AuthorizedPath,
    search: bool,
    empty: bool,
    access: &Access,
    selected: &HashMap<i64, AuthorizedPath>,
) -> Markup {
    let has_selected = !selected.is_empty();
    let path_str = target.as_ref().to_string_lossy();
    html! {
        div.open #context-menu "@click"="$refs.menu.classList.remove('open'); contextMenuClose(event)" x-ref="menu" {
            ul.ui-menu {
                @if !empty {
                    @if has_selected {
                        li {
                            a.deselect-multiple href="#"
                                "@click.prevent"="multiselecting = !multiselecting; checked.clear(); lastToggledCheckbox = null" {
                                i {}
                                span {
                                    "Cancel selection ("
                                    (selected.len())
                                    ")"
                                }
                            }
                        }
                    } @else {
                        li {
                            a ":class"="multiselecting ? 'deselect-multiple' : 'select-multiple'" href="#"
                                "@click.prevent"="multiselecting = !multiselecting; checked.clear(); lastToggledCheckbox = null" {
                                i {}
                                span x-text="multiselecting ? 'Cancel selection' : 'Select multiple'" {}
                            }
                        }
                    }
                    li {
                        @let link = download_url(path_str.as_ref(), selected.keys(), access);
                        a.download href=(link) {
                            i {}
                            "Download"
                        }
                    }
                }
                @if matches!(access, Access::Admin) {
                    @if !has_selected && !search && !empty {
                        li {
                            @let link = format!("/rename?{}", urlencode(&[("path", path_str.as_ref())]));
                            a.rename href=(link) hx-get=(link) hx-target="#modal" {
                                i {}
                                "Rename"
                            }
                        }
                    }
                    @if !has_selected {
                        li {
                            @let link = format!("/share?{}", urlencode(&[("path", path_str.as_ref())]));
                            a.share href=(link) hx-get=(link) hx-target="#modal" {
                                i {}
                                "Share"
                            }
                        }
                        @let parent = if empty {
                            target.as_ref()
                        } else {
                            target.as_ref().parent().unwrap_or(target.as_ref())
                        };
                        @if !search {
                            li.menu-divider role="separator" {}
                            li {
                                @let link = format!("/new?{}", urlencode(&[("parent", &parent.to_string_lossy())]));
                                a.new-folder href=(link) hx-get=(link) hx-target="#modal" {
                                    i {}
                                    "New folder"
                                }
                            }
                            li {
                                @let link = format!("/upload?{}", urlencode(&[("parent", &parent.to_string_lossy())]));
                                a.upload href=(link) hx-get=(link) hx-target="#modal" {
                                    i {}
                                    "Upload files"
                                }
                            }
                        }
                    }
                    @if !empty {
                        li.menu-divider role="separator" {}
                        li {
                            @let link = unlink_url(path_str.as_ref(), selected.keys());
                            a.delete href=(link) hx-get=(link) hx-target="#modal" {
                                i {}
                                "Delete"
                            }
                        }
                    }
                }
            }
        }
    }
}

fn page_actions(view: &View, access: &Access) -> Markup {
    match view {
        View::Root | View::Search(_) => html! {},
        View::Dir(path) | View::File(path) => html! {
            details.dropdown #page-actions x-ref="dropdown" {
                summary {
                    svg xmlns="http://www.w3.org/2000/svg" fill="currentColor" viewBox="0 0 16 16" {
                        path d="M3 9.5a1.5 1.5 0 1 1 0-3 1.5 1.5 0 0 1 0 3m5 0a1.5 1.5 0 1 1 0-3 1.5 1.5 0 0 1 0 3m5 0a1.5 1.5 0 1 1 0-3 1.5 1.5 0 0 1 0 3" {}
                    }
                }
                ul.ui-menu x-on:click="$refs.dropdown.open = false" {
                    @if matches!(access, Access::Admin) {
                        @if matches!(view, View::Dir(_)) {
                            li {
                                @let link = format!("/new?{}", urlencode(&[("parent", &path.to_string_lossy())]));
                                a.new-folder href=(link) hx-get=(link) hx-target="#modal" {
                                    i {}
                                    "New folder"
                                }
                            }
                            li {
                                @let link = format!("/upload?{}", urlencode(&[("parent", &path.to_string_lossy())]));
                                a.upload href=(link) hx-get=(link) hx-target="#modal" {
                                    i {}
                                    "Upload files"
                                }
                            }
                        }
                        li {
                            @let link = format!("/share?{}", urlencode(&[("path", view.path())]));
                            a.share href=(link) hx-get=(link) hx-target="#modal" {
                                i {}
                                "Share"
                            }
                        }
                    }
                    li {
                        a.download href=(download_url(view.path(), std::iter::empty(), access)) {
                            i {}
                            "Download"
                            @if matches!(view, View::File(_)) {
                                " file"
                            } @else if matches!(view, View::Dir(_)) {
                                " folder"
                            }
                        }
                    }
                    @if matches!(access, Access::Admin) {
                        li.menu-divider role="separator" {}
                        li {
                            a.logout href="/logout" hx-delete="/logout" hx-target="#container" {
                                i {}
                                "Logout"
                            }
                        }
                    }
                }
            }
        },
    }
}

pub(crate) fn new_folder_modal(parent: &Path) -> Markup {
    html! {
        dialog open x-ref="modal" x-data="{ folderName: '' }" {
            article "@click.outside"="$refs.modal.close()" {
                header {
                    span { "New folder" }
                    button aria-label="Close" rel="prev" x-on:click="$refs.modal.close()" {}
                }
                form #new-folder-form action="/new" hx-post="/new" hx-target="#modal" {
                    input type="hidden" name="parent" x-ref="parent" value=(parent.to_string_lossy());
                    label for="new-folder-name" { "Folder name or path" }
                    input #new-folder-name type="text" name="name" x-model="folderName" placeholder="path/to/new/folder" autofocus aria-describedby="new-folder-path new-folder-error";
                    small x-text="($refs.parent.value.endsWith('/') ? $refs.parent.value : $refs.parent.value + '/') + folderName" { (parent.to_string_lossy()) "/" }
                    div #new-folder-error role="alert" {}
                }
                footer {
                    button form="new-folder-form" { "Create folder" }
                }
            }
        }
    }
}

pub(crate) fn upload_modal(parent: &Path) -> Markup {
    html! {
        dialog #upload-modal open x-ref="modal" x-data="uploadFiles()" {
            article "@click.outside"="$refs.modal.close()" {
                header {
                    span { "Upload files" }
                    button aria-label="Close" rel="prev" x-on:click="$refs.modal.close()" {}
                }
                input type="hidden" x-ref="parent" value=(parent.to_string_lossy());
                div.outline.secondary #upload-dropzone x-ref="dropzone" role="button" tabindex="0" aria-label="Choose files to upload"
                    data-max-size=(config::get().app.max_upload_file_size)
                    data-chunk-size=(crate::upload::CHUNK_SIZE)
                    "@keydown.enter.prevent"="$refs.dropzone.click()"
                    "@keydown.space.prevent"="$refs.dropzone.click()" {
                    svg xmlns="http://www.w3.org/2000/svg" width="28" height="28" fill="currentColor" viewBox="0 0 16 16" {
                        path d="M.5 9.9a.5.5 0 0 1 .5.5v2.5a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-2.5a.5.5 0 0 1 1 0v2.5a2 2 0 0 1-2 2H2a2 2 0 0 1-2-2v-2.5a.5.5 0 0 1 .5-.5" {}
                        path d="M7.646 11.854a.5.5 0 0 0 .708 0l3-3a.5.5 0 0 0-.708-.708L8.5 10.293V1.5a.5.5 0 0 0-1 0v8.793L5.354 8.146a.5.5 0 1 0-.708.708z" {}
                    }
                    "Drop files here or click to choose files"
                }
                small { "Destination: " (parent.to_string_lossy()) }
                div #upload-list aria-live="polite" {
                    template x-for="item in uploads" ":key"="item.id" {
                        div .upload-item {
                            div .upload-item-label {
                                span x-text="item.name" {}
                                span x-text="item.status === 'Uploading' ? Math.round(item.progress) + '%' : item.status" {}
                                a href="#" .outline .secondary x-show="item.status === 'Failed' && item.retryable" x-cloak data-tooltip="Retry"
                                    "@click.prevent"="retry(item.id)" ":aria-label"="'Retry uploading ' + item.name" { i.retry {} }
                            }
                            progress max="100" ":value"="item.progress" ":aria-label"="item.name + ' upload progress'" {}
                            small x-show="item.message" x-text="item.message" role="alert" {}
                        }
                    }
                }
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

fn office_preview(path: &Path, access: &Access) -> Markup {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let presentation = extension == "ppt" || extension == "pptx";
    let legacy = extension == "doc" || extension == "ppt";
    let source = format!(
        "{}&force_inline=true",
        preview_url(&path.to_string_lossy(), access)
    );

    html! {
        div #office-preview
            data-src=(source)
            data-kind=(if presentation { "pptx" } else { "docx" })
            data-legacy=(legacy)
            x-init="load()"
            x-data=r"(() => {
                let engine, viewer, scrollViewer, observer, media, onMediaChange, office, kind, disposed = false;
                return {
                    pages: [], current: 0, error: '', mobile: matchMedia('(max-width: 640px)').matches,
                    async load() {
                        try {
                            kind = this.$el.dataset.kind;
                            office = await import(kind === 'docx' ? '@silurus/ooxml/docx' : '@silurus/ooxml/pptx');
                            if (disposed) return;
                            const options = {};
                            if (this.$el.dataset.legacy === 'true') {
                                const reader = kind === 'docx' ? 'doc' : 'ppt';
                                const legacy = await import('@silurus/ooxml-legacy-office/' + reader);
                                options[kind === 'docx' ? 'legacyDoc' : 'legacyPpt'] = legacy[kind === 'docx' ? 'legacyDoc' : 'legacyPpt'];
                            }
                            if (disposed) return;
                            const document = kind === 'docx'
                                ? await office.DocxDocument.load(this.$el.dataset.src, options)
                                : await office.PptxPresentation.load(this.$el.dataset.src, options);
                            if (disposed) { document.destroy(); return; }
                            engine = document;
                            media = matchMedia('(max-width: 640px)');
                            this.mobile = media.matches;
                            this.pages = Array.from({ length: kind === 'docx' ? document.pageCount : document.slideCount }, (_, i) => i);
                            onMediaChange = () => {
                                this.mobile = media.matches;
                                this.$nextTick(() => this.switchView());
                            };
                            media.addEventListener('change', onMediaChange);
                            await this.$nextTick();
                            if (disposed) return;
                            await this.switchView();
                            observer = new ResizeObserver(() => viewer?.fitWidth());
                            observer.observe(this.$refs.stage);
                        } catch (error) {
                            if (disposed) return;
                            this.error = error.code === 'unsupported-content'
                                ? 'This older Office file is not supported. Convert it to DOCX or PPTX to preview it.'
                                : error.message || 'Preview failed.';
                        }
                    },
                    async switchView() {
                        if (disposed || !engine) return;
                        if (this.mobile) {
                            if (scrollViewer) return;
                            viewer?.destroy();
                            viewer = null;
                            scrollViewer = kind === 'docx'
                                ? office.DocxScrollViewer.fromDocument(this.$refs.scroll, engine, { enableTextSelection: true })
                                : office.PptxScrollViewer.fromPresentation(this.$refs.scroll, engine, { enableTextSelection: true });
                            scrollViewer.fitWidth();
                            if (kind === 'docx') scrollViewer.scrollToPage(this.current);
                            else scrollViewer.scrollToSlide(this.current);
                        } else {
                            if (viewer) return;
                            if (scrollViewer) {
                                this.current = kind === 'docx' ? scrollViewer.topVisiblePage : scrollViewer.topVisibleSlide;
                                scrollViewer.destroy();
                                scrollViewer = null;
                            }
                            viewer = kind === 'docx'
                                ? office.DocxViewer.fromDocument(this.$refs.page, engine, { enableTextSelection: true })
                                : office.PptxViewer.fromPresentation(this.$refs.page, engine, { enableTextSelection: true });
                            await this.show(this.current);
                        }
                    },
                    async show(index) {
                        if (!viewer) return;
                        if (kind === 'docx') await viewer.goToPage(index);
                        else await viewer.goToSlide(index);
                        this.current = index;
                        await viewer.fitWidth();
                    },
                    async thumbnail(canvas, index) {
                        if (kind === 'docx') await engine.renderPage(canvas, index, { width: 160 });
                        else await engine.renderSlide(canvas, index, { width: 160 });
                    },
                    destroy() {
                        if (disposed) return;
                        disposed = true;
                        media?.removeEventListener('change', onMediaChange);
                        observer?.disconnect();
                        viewer?.destroy();
                        scrollViewer?.destroy();
                        engine?.destroy();
                    }
                }})()" {
            p #office-status x-show="!pages.length && !error" role="status" { "Loading preview…" }
            p #office-error x-show="error" x-text="error" x-cloak role="alert" {}
            nav #office-thumbnails x-show="pages.length" x-cloak aria-label=(if presentation { "Slides" } else { "Pages" }) {
                template x-for="page in mobile ? [] : pages" ":key"="page" {
                    button type="button" "@click"="show(page)" ":aria-current"="current === page ? 'page' : null"
                        ":aria-label"={ (if presentation { "'Slide '" } else { "'Page '" }) " + (page + 1)" } {
                        canvas x-init="thumbnail($el, page)" aria-hidden="true" {}
                        span x-text="page + 1" {}
                    }
                }
            }
            section #office-detail x-show="pages.length && !mobile" x-cloak aria-label=(if presentation { "Current slide" } else { "Current page" }) {
                div #office-stage x-ref="stage" {
                    canvas x-ref="page" {}
                }
            }
            section #office-scroll x-ref="scroll" x-show="pages.length && mobile" x-cloak aria-label=(if presentation { "Slides" } else { "Pages" }) {}
        }
    }
}

pub(crate) async fn spreadsheet_frame(path: &AuthorizedPath) -> AppResult<Markup> {
    let contents = tokio::fs::read(path)
        .await
        .map_err(|e| anyhow::anyhow!("can't read file {e}"))?;

    Ok(html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                link rel="stylesheet" href="/static/vendor/sheetjs/preview.css";
                script defer src="/static/vendor/sheetjs/xlsx.full.min.js" {}
                script defer src="/static/vendor/sheetjs/preview.js" {}
                title { (path.as_ref().to_str().unwrap_or_default()) }
            }
            body {
                template #workbook-data { (STANDARD.encode(contents)) }
                p #status role="status" { "Loading spreadsheet…" }
                nav #sheet-tabs aria-label="Worksheets" hidden {}
                main #sheet-content hidden {}
            }
        }
    })
}

fn font_preview(path: &Path, access: &Access) -> Markup {
    let source = preview_url(&path.to_string_lossy(), access);
    let sample = "The quick brown fox jumps over the lazy dog.";

    html! {
        article #font-preview data-src=(source) x-init="load($el)"
            x-data=r"{
                sample: 'The quick brown fox jumps over the lazy dog.',
                ready: false,
                error: '',
                face: null,
                controller: new AbortController(),
                async load(element) {
                    try {
                        this.face = await loadFontPreview(element.dataset.src, this.controller.signal);
                        element.style.setProperty('--font-preview-family', this.face.family);
                        this.ready = true;
                    } catch (error) {
                        if (error.name !== 'AbortError') this.error = error.message || 'unknown error';
                    }
                },
                destroy() {
                    this.controller.abort();
                    if (this.face) document.fonts.delete(this.face);
                }
            }" {
            p x-show="!ready && !error" role="status" { "Loading font…" }
            p x-show="error" x-text="'Could not preview this font: ' + error" x-cloak role="alert" {}
            header {
                select x-model="sample" x-bind:disabled="!ready" disabled {
                    option value=(sample) { (sample) }
                    option value="Pack my box with five dozen liquor jugs." { "Pack my box with five dozen liquor jugs." }
                    option value="Sphinx of black quartz, judge my vow." { "Sphinx of black quartz, judge my vow." }
                    option value="How vexingly quick daft zebras jump!" { "How vexingly quick daft zebras jump!" }
                }
            }
            main x-show="ready" x-cloak {
                h1 class="sample-text" x-text="sample" { (sample) }
                h2 class="sample-text" x-text="sample" { (sample) }
                h3 class="sample-text" x-text="sample" { (sample) }
                h4 class="sample-text" x-text="sample" { (sample) }
                h5 class="sample-text" x-text="sample" { (sample) }
                h6 class="sample-text" x-text="sample" { (sample) }
                p class="sample-text" x-text="sample" { (sample) }
            }
        }
    }
}

pub(crate) fn model_frame(path: &AuthorizedPath, access: &Access) -> Markup {
    let source = format!(
        "{}&force_inline=true",
        preview_url(&path.as_ref().to_string_lossy(), access)
    );
    let format = path
        .as_ref()
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (path.as_ref().to_string_lossy()) }
                link rel="stylesheet" href="/static/vendor/model-preview.css";
                script type="importmap" {
                    (PreEscaped(r#"{
                        "imports": {
                            "three": "https://cdn.jsdelivr.net/npm/three@0.187.0/build/three.module.js",
                            "three/addons/": "https://cdn.jsdelivr.net/npm/three@0.187.0/examples/jsm/"
                        }
                    }"#))
                }
                script type="module" src="https://ajax.googleapis.com/ajax/libs/model-viewer/4.4.0/model-viewer.min.js" {}
                script type="module" src="/static/vendor/model-preview.js" {}
            }
            body {
                model-viewer #model data-src=(source) data-format=(format)
                    alt="3D model preview" camera-controls loading="eager" shadow-intensity="1" {}
                p #status role="status" { "Loading 3D model…" }
            }
        }
    }
}

pub(crate) async fn page(page: &Page, req: &Request, access: &Access) -> AppResult<Markup> {
    Ok(match &page.view {
        View::Root => {
            let mut sources = config::get()
                .app
                .canonicalized_sources()
                .iter()
                .collect::<Vec<_>>();
            sources.sort();
            root_view(&sources)
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

fn root_view(sources: &[&PathBuf]) -> Markup {
    html! {
        div.root-page {
            section aria-labelledby="sources-heading" {
                details.dropdown.menu x-ref="dropdown" {
                    summary {
                        svg xmlns="http://www.w3.org/2000/svg" fill="currentColor"  viewBox="0 0 16 16" {
                            path fill-rule="evenodd" d="M2.5 12a.5.5 0 0 1 .5-.5h10a.5.5 0 0 1 0 1H3a.5.5 0 0 1-.5-.5m0-4a.5.5 0 0 1 .5-.5h10a.5.5 0 0 1 0 1H3a.5.5 0 0 1-.5-.5m0-4a.5.5 0 0 1 .5-.5h10a.5.5 0 0 1 0 1H3a.5.5 0 0 1-.5-.5" {}
                        }
                        "Menu"
                    }
                    ul.ui-menu x-on:click="$refs.dropdown.open = false" {
                        li {
                            a.share  href="/shares?all=true" hx-get="/shares?all=true" hx-target="#modal" {
                                i {}
                                "View shared links"
                            }
                        }
                        li.menu-divider role="separator" {}
                        li {
                            a.logout href="/logout" hx-delete="/logout" hx-target="#container" {
                                i {}
                                "Logout"
                            }
                        }
                    }
                }
                h2 #sources-heading { "Sources" }
                @if sources.is_empty() {
                    p { "No sources configured." }
                } @else {
                    div.source-list {
                        @for source in sources {
                            @let path = source.to_string_lossy();
                            @let link = files_url(&path, &Access::Admin);
                            @let name = source.file_name().map_or_else(|| path.clone(), |name| name.to_string_lossy());
                            a href=(link) hx-get=(link) hx-target="#view" hx-swap="innerHTML show:top showTarget:#container" hx-push-url="true" x-on:click="search_q = ''" title=(path) {
                                svg aria-hidden="true" viewBox="0 0 16 16" fill="currentColor" {
                                    path d="M9.828 3h3.982a2 2 0 0 1 1.992 2.181l-.637 7A2 2 0 0 1 13.174 14H2.825a2 2 0 0 1-1.991-1.819l-.637-7a2 2 0 0 1 .342-1.31L.5 3a2 2 0 0 1 2-2h3.672a2 2 0 0 1 1.414.586l.828.828A2 2 0 0 0 9.828 3m-8.322.12q.322-.119.684-.12h5.396l-.707-.707A1 1 0 0 0 6.172 2H2.5a1 1 0 0 0-1 .981z" {}
                                }
                                span { (name) }
                            }
                        }
                    }
                }
            }
            small.build-info.text-muted {
                "fayls v." (env!("CARGO_PKG_VERSION")) " (" (env!("FAYLS_COMMIT")) ", " (env!("FAYLS_PROFILE")) " profile)"
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
        nav {
            (breadcrumbs(view, &query_string, access))
            (page_actions(view, access))
        }
        div.paths x-data="{
            checked: new Set(),
            lastToggledCheckbox: null,
            setRange(current, on) {
                const checkboxes = Array.from($refs.tbody.querySelectorAll('tr .check input[type=checkbox]'))
                const previous = checkboxes.indexOf(this.lastToggledCheckbox)
                const next = checkboxes.indexOf(current)
                if (previous === -1 || next === -1) return

                checkboxes.slice(Math.min(previous, next), Math.max(previous, next) + 1)
                    .filter(checkbox => checkbox !== current && checkbox.checked !== on)
                    .forEach(checkbox => checkbox.click())
            }
        }" {
            div #context-menu {}
            table.striped hx-get={ (files_url(view.path(), access)) (&query_string) }
                ":class"="multiselecting ? 'multiselecting' : ''"
                hx-trigger="reload-view" hx-target="#view" {
                thead hx-sse:connect={ (sse_url(view.path(), access)) } hx-trigger="load delay:1s"
                    hx-config="ws.pauseOnBackground: false" {
                        @if !files.is_empty() {
                            tr x-data="{
                                toggle(on) {
                                    lastToggledCheckbox = null
                                    if (on) {
                                        $refs.tbody.querySelectorAll('input[type=checkbox]').forEach(c => {
                                            c.checked = true
                                            c.dispatchEvent(new Event('change', { bubbles: true }))
                                        })
                                    } else {
                                        checked.clear()
                                    }
                                }
                            }" {
                                th.check {
                                    input type="checkbox" "@click.stop"="" "@change"="toggle(event.currentTarget.checked)" {}
                                }
                                @let (sort, order) = get_sorting(req);
                                (file_list_header(&Sort::Name, &sort, &order, req))
                                    (file_list_header(&Sort::Size, &sort, &order, req))
                                    (file_list_header(&Sort::LastModified, &sort, &order, req))
                            }
                        }
                    }
                tbody x-ref="tbody" {
                    @if files.is_empty() {
                        tr.empty {
                            @match view {
                                View::Dir(path_buf) => {
                                    td colspan="4" hx-get={(path_context_menu_url(path_buf.to_string_lossy().as_ref(), access)) "&empty=true"}
                                        hx-trigger="contextmenu"
                                        hx-target="#context-menu"
                                        hx-swap="outerHTML"
                                        "@contextmenu"="rowContextMenu(event)"
                                        hx-on::after:swap="repositionContextMenu(event)" {
                                            "Empty"
                                    }
                                },
                                _ => td colspan="4" { "Nothing found" },
                            }
                        }
                    } @else {
                        @for file in files {
                            @let link = format!("{}{}", files_url(&file.path_buf().to_string_lossy().clone(), access), &query_string);
                            (row(view, file, &link, show_full_paths, access))
                        }
                    }
                }
                @if !files.is_empty() {
                    tfoot {
                        tr {
                            td colspan="10" {
                                span.wrap {
                                    span.stats {
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
                                    }

                                    @if matches!(access, Access::Admin) {
                                        span #index-progress {
                                            (index_progress(progress))
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn row_cell(
    record: &ExistingPathRecord,
    contextmenu_url: &str,
    class: &str,
    contents: Markup,
) -> Markup {
    html! {
        td .(class)
            hx-get=(contextmenu_url)
            hx-trigger="contextmenu"
            hx-target="#context-menu"
            hx-swap="outerHTML"
            hx-include={"#r" (record.id) " .selected-path"}
            "@contextmenu"="rowContextMenu(event)"
            "@click"="if (multiselecting) {
                event.stopPropagation();
                const checkbox = $el.closest('tr').querySelector('.check input[type=checkbox]');
                if (event.shiftKey) setRange(checkbox, !checkbox.checked);
                checkbox.click();
            }"
            hx-on::after:swap="repositionContextMenu(event)" {
                (contents)
            }
    }
}

fn row(
    view: &View,
    record: &ExistingPathRecord,
    link: &str,
    show_full_paths: bool,
    access: &Access,
) -> Markup {
    let mut contextmenu_url =
        path_context_menu_url(record.path_buf().to_string_lossy().as_ref(), access);
    if matches!(view, View::Search(_)) {
        contextmenu_url = format!("{contextmenu_url}&search=true");
    }
    html! {
        tr.(file_row_class(record)) id={ "r"(record.id) } x-on:click="search_q = ''"
            hx-get=(link)
            hx-target="#view"
            hx-swap="innerHTML show:top showTarget:#container"
            hx-push-url="true"
            x-data={
                "{
                    id: " (record.id) ",
                    toggle(on) {
                        on ? checked.add(this.id) : checked.delete(this.id)
                    }
                }"
            }
            x-bind:class="checked.has(id) ? 'checked' : ''" {
            template x-for="path in checked" {
                input.selected-path name="id" type="hidden" x-model="path";
            }

            (row_cell(record, &contextmenu_url, "check", html! {
                input type="checkbox" "@click.stop"="if (event.shiftKey) setRange($el, $el.checked); lastToggledCheckbox = $el"
                    ":checked"="checked.has(id)" "@change"="toggle($el.checked)" {}
            }))

            (row_cell(record, &contextmenu_url, "name", html! {
                span.wrapper {
                    i.icon {}
                    span {
                        (record.name)
                        @if show_full_paths {
                            em { (access_scoped_path(record.parent.as_deref().unwrap_or("/"), access)) }
                        }
                    }
                }
            }))

            (row_cell(record, &contextmenu_url, "size", html! {
                (format_size(record.size))
            }))

            (row_cell(record, &contextmenu_url, "last_modified", html! {
                @let lastmod = record.last_modified.map_or(String::new(), |lm| lm.to_string());
                time x-data={ "{ time: timeAgo(" (lastmod) ") }" } x-text="time" datetime=(lastmod) { (lastmod) }
            }))
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
        nav {
            (breadcrumbs(view, &query_string, access))
            (page_actions(view, access))
        }
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
        "otf" | "ttf" | "woff" => font_preview(file_path, access),
        "doc" | "docx" | "ppt" | "pptx" => office_preview(file_path, access),
        "pdf" | "xlsx" | "xls" | "ods" | "numbers" | "gltf" | "glb" | "ply" | "stl" => {
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
                a role="button" href={ (download_url(f, std::iter::empty(), access)) } { "Download" }
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

pub(crate) fn shares_for_path(
    path_record: &ExistingPathRecord,
    shares: &[ExistingShareRecord],
    message: Option<Message>,
) -> Markup {
    let link = urlencode(&[
        ("add", "true"),
        ("path", path_record.path_buf().to_string_lossy().as_ref()),
    ]);
    share_list_modal(
        &html! { "Shared links for " code { (path_record.name) } },
        shares,
        message,
        &html! {
            footer {
                button.outline hx-get={"/share?" (link)} hx-target="#modal" {
                    "+ Add shared link "
                }
            }
        },
        "",
        false,
    )
}

pub(crate) fn all_shares(shares: &[ExistingShareRecord], message: Option<Message>) -> Markup {
    share_list_modal(
        &html! { "Shared links" },
        shares,
        message,
        &html! {},
        "?all=true",
        true,
    )
}

fn share_list_modal(
    title: &Markup,
    shares: &[ExistingShareRecord],
    message: Option<Message>,
    footer: &Markup,
    delete_suffix: &str,
    show_path_label: bool,
) -> Markup {
    let base_url = config::get().app.share_url.as_deref().unwrap_or("");
    html! {
        dialog open x-ref="modal" x-data="{}" {
            article "@click.outside"="$refs.modal.close()" {
                header {
                    span { (title) }
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
                                    span.share-link-name {
                                        @if show_path_label {
                                            @if let Some(path_label) = &share.path_label {
                                                @let link = files_url(path_label, &Access::Admin);
                                                a href=(link) hx-get=(link) hx-target="#view" hx-swap="innerHTML show:top showTarget:#container" hx-push-url="true" x-on:click="$refs.modal.close()" {
                                                    (path_label)
                                                }
                                            }
                                        } @else {
                                            span x-text="base_url" { (base_url) }
                                            span x-text="url" { (share.url) }
                                        }
                                    }

                                    small.text-muted.share-link-labels {
                                        code {
                                            "created "
                                                time x-data={ "{ time: timeAgo(" (share.created_at) ") }" } x-text="time" datetime=(share.created_at) { (share.created_at) }
                                        }
                                        " "
                                        code {
                                            "accessed " (share.accessed) " times"
                                        }
                                        @if share.password.is_some() {
                                            " "
                                                code { "protected" }
                                        }
                                        @if let Some(time) = share.expires_at {
                                            " "
                                                code x-data={ "{ expired: " (time) " < Math.floor(Date.now() / 1000) }" } {
                                                    span x-text="expired ? 'expired ' : 'expires '" { "expires " }
                                                    time x-text={ "expired ? timeAgo(" (time) ") : time(" (time) ")" } datetime=(time) { (time) }
                                                }
                                        }
                                    }
                                }
                                td.actions {
                                    a.copy x-ref="copy" data-tooltip="Copy link" href="#" "@click.prevent"=(copy) { i {}}
                                    a.delete data-tooltip="Delete" href="#" hx-confirm="Are you sure?" hx-delete={ "/share/" (share.url) (delete_suffix) } hx-target="#modal" { i {}}
                                }
                            }
                        }
                    }
                }
                (footer)
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

pub(crate) fn unlink_modal(paths: &HashMap<i64, AuthorizedPath>) -> Markup {
    html! {
        dialog open x-ref="modal" x-data="{}" {
            article "@click.outside"="$refs.modal.close()" {
                header {
                    span {
                        "Delete " (paths.len())
                        @if paths.len() == 1 {
                            " path"
                        } @else {
                            " paths"
                        }
                    }
                    button aria-label="Close" rel="prev" x-on:click="$refs.modal.close()" {}
                }
                div.alert {
                    form action="/unlink" hx-delete="/unlink" hx-target="#modal" id="unlink-form" {
                        p { "Warning! The following will be permanently deleted:" }
                        ul {
                            @for (id, path) in paths.iter() {
                                input type="hidden" name="id" value=(id);
                                li { (path.as_ref().to_string_lossy()) }
                            }
                        }
                        p { "Proceed?" }
                    }
                }
                footer {
                    button form="unlink-form" { "Delete" }
                }
            }
        }
    }
}

pub(crate) fn rename_modal(path: &AuthorizedPath) -> Markup {
    let name = path
        .as_ref()
        .file_name()
        .map(|f| f.to_string_lossy())
        .unwrap_or_default();
    let parent = path
        .as_ref()
        .parent()
        .map(|f| f.to_string_lossy())
        .unwrap_or_default();
    let path = path.as_ref().to_string_lossy();
    html! {
        dialog open x-ref="modal" x-data={"{ name: '" (name) "', parent: '" (parent) "' }"} {
            article "@click.outside"="$refs.modal.close()" {
                header {
                    span {
                        "Rename "
                        code { (name) }
                    }
                    button aria-label="Close" rel="prev" x-on:click="$refs.modal.close()" {}
                }
                form #rename-form action="/rename" hx-patch="/rename" hx-target="#modal" {
                    input type="hidden" name="path" x-ref="path" value=(path);
                    label for="new-name" { "New name" }
                    input #new-name type="text" name="name" x-model="name" autofocus autocomplete="off";
                    small x-text="(parent.endsWith('/') ? parent : parent + '/') + name" {}
                    div #rename-error role="alert" {}
                }
                footer {
                    button form="rename-form" { "Rename" }
                }
            }
        }
    }
}
