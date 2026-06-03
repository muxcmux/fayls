pub(crate) mod views;

use futures_util::StreamExt;
use serde::Deserialize;
use tokio_stream::wrappers::UnboundedReceiverStream;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

use crate::{
    app, config,
    db::NewPathRecord,
    error::{AppResult, Error},
    web::views::Page,
};
use salvo::{
    conn::tcp::TcpAcceptor,
    fs::NamedFile,
    prelude::*,
    session::{CookieStore, Session, SessionDepotExt},
};
use subtle::ConstantTimeEq;
use tokio::sync::mpsc::{self, UnboundedSender};

fn is_hx(req: &Request) -> bool {
    req.header::<bool>("hx-request").is_some_and(|v| v)
}

fn history_restore_requested(req: &Request) -> bool {
    req.header::<bool>("hx-history-restore-request")
        .is_some_and(|v| v)
}

#[derive(Default, Deserialize, PartialEq)]
pub(crate) enum Order {
    #[default]
    #[serde(rename = "desc")]
    Desc,
    #[serde(rename = "asc")]
    Asc,
}

#[derive(Default, Deserialize, PartialEq)]
pub(crate) enum Sort {
    #[default]
    #[serde(rename = "last_modified")]
    LastModified,
    #[serde(rename = "name")]
    Name,
    #[serde(rename = "size")]
    Size,
}

impl Order {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }

    pub(crate) fn reverse(&self) -> Self {
        match self {
            Self::Desc => Self::Asc,
            Self::Asc => Self::Desc,
        }
    }
}

impl Sort {
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::LastModified => "last_modified",
            Self::Name => "name",
            Self::Size => "size",
        }
    }

    pub(crate) fn humanize(&self) -> &str {
        match self {
            Self::LastModified => "Last Modified",
            Self::Name => "Name",
            Self::Size => "Size",
        }
    }
}

pub(crate) fn get_sorting(req: &Request) -> (Sort, Order) {
    let sort = req.query::<Sort>("sort").unwrap_or_default();
    let order = req.query::<Order>("order").unwrap_or_default();
    (sort, order)
}

#[handler]
async fn healthcheck() -> &'static str {
    "Ok"
}

#[handler]
async fn path_handler(req: &mut Request, res: &mut Response) -> AppResult {
    // unwrapping here is safe because of the routes guard where
    // we handle the /files path by serving root dirs
    let path = req
        .param::<&str>("path")
        .map(|p| format!("/{p}"))
        .expect("this can't happen");

    let path = PathBuf::from(path);

    let page = views::page(Page::from(&path), req).await?;

    res.render(Text::Html(
        if is_hx(req) {
            page
        } else {
            views::layout("Fayls", history_restore_requested(req), &page)
        }
        .into_string(),
    ));

    Ok(())
}

#[handler]
async fn list_roots_handler(req: &Request, res: &mut Response) -> AppResult {
    let page = views::page(Page::root(), req).await?;

    res.render(Text::Html(
        if is_hx(req) {
            page
        } else {
            views::layout("Fayls", history_restore_requested(req), &page)
        }
        .into_string(),
    ));

    Ok(())
}

#[handler]
async fn search_handler(req: &mut Request, res: &mut Response) -> AppResult {
    let term = req
        .query::<&str>("q")
        .ok_or(Error::BadRequest("no query param"))?;

    let page = views::page(Page::search(term), req).await?;

    res.render(Text::Html(
        if is_hx(req) {
            page
        } else {
            views::layout("Fayls", history_restore_requested(req), &page)
        }
        .into_string(),
    ));

    Ok(())
}

#[handler]
async fn preview_handler(req: &mut Request, res: &mut Response) -> AppResult {
    let path = req
        .query::<PathBuf>("path")
        .ok_or(Error::BadRequest("no path param"))?;

    _ = NewPathRecord::from(&path)
        .find_existing(app::db())
        .await?
        .ok_or(Error::NotFound)?;

    if req.query::<&str>("force_inline").is_some() {
        serve_inline_file(&path, req, res).await;
        return Ok(());
    }

    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => match ext.to_ascii_lowercase().as_ref() {
            "docx" => preview_docx_file(&path, res).await?,
            _ => serve_inline_file(&path, req, res).await,
        },
        None => serve_inline_file(&path, req, res).await,
    }

    Ok(())
}

async fn serve_inline_file(path: &Path, req: &mut Request, res: &mut Response) {
    NamedFile::builder(path)
        .disposition_type("inline")
        .send(req.headers(), res)
        .await;
}

async fn preview_docx_file(path: &Path, res: &mut Response) -> AppResult {
    res.render(Text::Html(views::docx_frame(path).await?.into_string()));
    Ok(())
}

#[handler]
async fn download_handler(req: &mut Request, res: &mut Response) -> AppResult {
    let path = req
        .query::<&str>("path")
        .ok_or(Error::BadRequest("no path param"))?;

    _ = NewPathRecord::from(path)
        .find_existing(app::db())
        .await?
        .ok_or(Error::NotFound)?;

    NamedFile::builder(path)
        .disposition_type("attachment")
        .send(req.headers(), res)
        .await;

    Ok(())
}

#[handler]
async fn ensure_authenticated(depot: &Depot, ctrl: &mut FlowCtrl, res: &mut Response) -> AppResult {
    if depot
        .session()
        .is_some_and(|session| session.get_raw("username").is_some())
    {
        Ok(())
    } else {
        ctrl.skip_rest();
        res.render(Redirect::other("/login"));
        Ok(())
    }
}

#[handler]
pub async fn login(req: &mut Request, depot: &mut Depot, res: &mut Response) -> AppResult {
    if req.method() == salvo::http::Method::POST {
        let user = req.form::<String>("username").await;
        let pass = req.form::<String>("password").await;

        if user.is_none() || pass.is_none() {
            res.status_code(StatusCode::UNAUTHORIZED);
            res.render(Text::Html(
                views::login(Some("Bad credentials"), None).into_string(),
            ));
            return Ok(());
        }

        let a = format!("{}:{}", config::get().auth.user, config::get().auth.pass);
        let b = format!("{}:{}", user.as_ref().unwrap(), pass.unwrap());

        if (!a.as_bytes().ct_eq(b.as_bytes())).into() {
            res.status_code(StatusCode::UNAUTHORIZED);
            res.render(Text::Html(
                views::login(Some("Bad credentials"), user.as_ref()).into_string(),
            ));
            return Ok(());
        }

        let mut session = Session::new();
        session
            .insert("username", user)
            .map_err(|e| anyhow::anyhow!(e))?;
        depot.set_session(session);
        res.render(Redirect::other("/"));
    } else {
        res.render(Text::Html(views::login(None, None).into_string()));
    }

    Ok(())
}

#[handler]
pub async fn logout(depot: &mut Depot, res: &mut Response) {
    if let Some(session) = depot.session_mut() {
        session.remove("username");
    }
    res.render(Redirect::other("/login"));
}

#[handler]
async fn serve_static_file(req: &mut Request, res: &mut Response) -> AppResult {
    let file = req.param::<&str>("file").ok_or(Error::NotFound)?;

    NamedFile::builder(PathBuf::from("static").join(file))
        .send(req.headers(), res)
        .await;

    Ok(())
}

#[handler]
async fn sse_connected(req: &Request, res: &mut Response) -> AppResult {
    let view = req.param::<Page>("page").unwrap_or(Page::search(""));

    let (tx, rx) = mpsc::unbounded_channel::<Event>();
    let rx = UnboundedReceiverStream::new(rx);

    {
        let mut conns = SSE_CONNECTIONS
            .lock()
            .map_err(|e| anyhow::anyhow!("mutex acquiring error: {e}"))?;

        if let Some(existing) = conns.get_mut(&view) {
            existing.push(tx);
        } else {
            conns.insert(view, vec![tx]);
        }
    }

    broadcast(&Event::Ping);

    let stream = rx.map(|event| Ok::<_, salvo::Error>(event.into_sse_event()));

    SseKeepAlive::new(stream).stream(res);

    Ok(())
}

type SseConnections = Mutex<HashMap<Page, Vec<UnboundedSender<Event>>>>;
static SSE_CONNECTIONS: LazyLock<SseConnections> = LazyLock::new(SseConnections::default);

#[derive(Clone)]
enum Event {
    Ping,
    Reload(Page),
    // (indexed, total)
    Progress((i64, i64)),
}

impl Event {
    fn into_sse_event(self) -> SseEvent {
        match self {
            Self::Ping => SseEvent::default().name("ping").text(""),
            Self::Reload(_) => SseEvent::default().name("reload-view").text(""),
            Self::Progress(progress) => SseEvent::default().text(
                maud::html! {
                    hx-partial hx-target="#index-progress" {
                        (views::index_progress(progress))
                    }
                }
                .into_string(),
            ),
        }
    }
}

pub(crate) fn reload(page: Page) {
    broadcast(&Event::Reload(page));
}

pub(crate) fn report_indexing_progress(progress: (i64, i64)) {
    broadcast(&Event::Progress(progress));
}

fn broadcast(event: &Event) {
    match SSE_CONNECTIONS.lock() {
        Ok(mut connections) => match event {
            Event::Ping | Event::Progress(_) => {
                connections.retain(|_, conns| {
                    conns.retain(|tx| tx.send(event.clone()).is_ok());
                    !conns.is_empty()
                });
            }
            Event::Reload(view) => {
                if let Some(conns) = connections.get_mut(view) {
                    conns.retain(|tx| tx.send(event.clone()).is_ok());
                }
            }
        },
        Err(e) => {
            tracing::error!(error = ?e, "broadcasting event failed");
        }
    }
}

pub async fn server() -> (Server<TcpAcceptor>, Router) {
    let acceptor = TcpListener::new(config::get().server.addr()).bind().await;

    let session = SessionHandler::builder(CookieStore::new(), config::secret())
        .build()
        .expect("failed to build session store");

    let protected_routes = Router::new()
        .hoop(ensure_authenticated)
        .push(Router::with_path("logout").delete(logout))
        .get(list_roots_handler)
        .push(
            Router::with_path("files")
                .get(list_roots_handler)
                .push(Router::with_path("{*path}").get(path_handler)),
        )
        .push(Router::with_path("search").get(search_handler))
        .push(Router::with_path("preview").get(preview_handler))
        .push(Router::with_path("download").get(download_handler))
        .push(
            Router::with_path("sse")
                .get(sse_connected)
                .push(Router::with_path("{*page}").get(sse_connected)),
        );

    let router = Router::new()
        .hoop(session)
        .hoop(
            Compression::new()
                .enable_brotli(CompressionLevel::Default)
                .enable_gzip(CompressionLevel::Default),
        )
        .hoop(CachingHeaders::new())
        .push(Router::with_path("health").get(healthcheck))
        .push(Router::with_path("static/{*file}").get(serve_static_file))
        .push(Router::with_path("login").goal(login))
        .push(protected_routes);

    let server = Server::new(acceptor);
    (server, router)
}
