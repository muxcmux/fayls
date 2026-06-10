pub(crate) mod views;

use argon2::{Argon2, PasswordHash, PasswordVerifier};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::UnboundedReceiverStream;

use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    sync::{LazyLock, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::web::views::Message;
use crate::{
    config,
    db::{ExistingPathRecord, ExistingShareRecord, NewShareRecord},
    error::{AppResult, Error},
    web::views::Page,
};
use salvo::{
    conn::tcp::TcpAcceptor,
    fs::NamedFile,
    prelude::*,
    session::{CookieStore, Session, SessionDepotExt},
};
use tokio::sync::mpsc::{self, UnboundedSender};

pub(crate) fn access_scoped_path(path: &str, access: &Access) -> String {
    match access {
        Access::Admin => path.into(),
        Access::Shared(SharedAccess { path_buf, .. }) => match path_buf.parent() {
            Some(parent) => match path.strip_prefix(parent.to_string_lossy().as_ref()) {
                Some(p) => p.into(),
                None => path.into(),
            },
            None => path.into(),
        },
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct SharedAccess {
    pub(crate) expires_at: Option<i64>,
    pub(crate) path_buf: PathBuf,
}
#[derive(Serialize, Deserialize)]
pub(crate) enum Access {
    Admin,
    Shared(SharedAccess),
}

impl Access {
    pub(crate) fn is_allowed(&self, path: impl AsRef<Path>) -> bool {
        match self {
            Self::Admin => true,
            Self::Shared(SharedAccess {
                expires_at,
                path_buf,
            }) => {
                if ensure_shared_access_hasnt_expired(*expires_at).is_err() {
                    return false;
                }

                path.as_ref().starts_with(path_buf)
            }
        }
    }
}

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
async fn path_handler(depot: &Depot, req: &mut Request, res: &mut Response) -> AppResult {
    // unwrapping here is safe because of the routes guard where
    // we handle the /files path by serving root dirs
    let path = req
        .param::<&str>("path")
        .map(|p| format!("/{p}"))
        .expect("this can't happen");

    let mut path = PathBuf::from(path);

    ensure_full_accessible_path(&mut path, depot)?;

    let access = access(depot)?;
    let page = views::page(Page::from(&path), req, &access).await?;

    res.render(Text::Html(
        if is_hx(req) {
            page
        } else {
            views::layout("Fayls", history_restore_requested(req), &page, &access)
        }
        .into_string(),
    ));

    Ok(())
}

#[handler]
async fn list_roots_handler(depot: &Depot, req: &Request, res: &mut Response) -> AppResult {
    let access = access(depot)?;
    let page = views::page(Page::root(), req, &access).await?;

    res.render(Text::Html(
        if is_hx(req) {
            page
        } else {
            views::layout("Fayls", history_restore_requested(req), &page, &access)
        }
        .into_string(),
    ));

    Ok(())
}

#[handler]
async fn search_handler(depot: &Depot, req: &mut Request, res: &mut Response) -> AppResult {
    let term = req
        .query::<&str>("q")
        .ok_or(Error::BadRequest("no query param"))?;

    let access = access(depot)?;
    let page = views::page(Page::search(term), req, &access).await?;

    res.render(Text::Html(
        if is_hx(req) {
            page
        } else {
            views::layout("Fayls", history_restore_requested(req), &page, &access)
        }
        .into_string(),
    ));

    Ok(())
}

#[handler]
async fn preview_handler(depot: &Depot, req: &mut Request, res: &mut Response) -> AppResult {
    let mut path = req
        .query::<PathBuf>("path")
        .ok_or(Error::BadRequest("no path param"))?;

    ensure_full_accessible_path(&mut path, depot)?;

    _ = ExistingPathRecord::find_by_path(&path)
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
async fn download_handler(depot: &Depot, req: &mut Request, res: &mut Response) -> AppResult {
    let mut path = req
        .query::<PathBuf>("path")
        .ok_or(Error::BadRequest("no path param"))?;

    ensure_full_accessible_path(&mut path, depot)?;

    _ = ExistingPathRecord::find_by_path(&path)
        .await?
        .ok_or(Error::NotFound)?;

    NamedFile::builder(path)
        .disposition_type("attachment")
        .send(req.headers(), res)
        .await;

    Ok(())
}

fn ensure_shared_access_hasnt_expired(expires_at: Option<i64>) -> AppResult {
    if let Some(expires_at) = expires_at {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| anyhow::anyhow!(e))?
            .as_secs()
            .cast_signed();

        if expires_at < now {
            return Err(Error::Forbidden);
        }
    }

    Ok(())
}

fn access(depot: &Depot) -> AppResult<Access> {
    let session = depot.session().ok_or(Error::Forbidden)?;
    session.get::<Access>("access").ok_or(Error::Forbidden)
}

fn ensure_full_accessible_path(requested_path: &mut PathBuf, depot: &Depot) -> AppResult {
    let access = access(depot)?;

    if let Access::Shared(SharedAccess { path_buf, .. }) = &access {
        let real_path = requested_path
            .components()
            .filter(|c| !matches!(c, Component::RootDir | Component::Prefix(_)))
            .collect::<PathBuf>();
        *requested_path = path_buf.parent().ok_or(Error::Forbidden)?.join(real_path);
    }

    if !access.is_allowed(&*requested_path) {
        return Err(Error::Forbidden);
    }

    Ok(())
}

#[handler]
async fn ensure_some_access(depot: &Depot, ctrl: &mut FlowCtrl, res: &mut Response) -> AppResult {
    if depot
        .session()
        .is_some_and(|session| session.get::<Access>("access").is_some())
    {
        Ok(())
    } else {
        ctrl.skip_rest();
        res.render(Redirect::other("/login"));
        Ok(())
    }
}

#[handler]
async fn ensure_admin_access(depot: &Depot, ctrl: &mut FlowCtrl, res: &mut Response) -> AppResult {
    if depot.session().is_some_and(|session| {
        session
            .get::<Access>("access")
            .is_some_and(|access| matches!(access, Access::Admin))
    }) {
        Ok(())
    } else {
        ctrl.skip_rest();
        res.render(Redirect::other("/login"));
        Ok(())
    }
}

#[handler]
async fn login(req: &mut Request, depot: &mut Depot, res: &mut Response) -> AppResult {
    if req.method() == salvo::http::Method::POST {
        let user = req.form::<String>("username").await;
        let pass = req.form::<String>("password").await;

        if user.is_none() || pass.is_none() {
            res.status_code(StatusCode::UNAUTHORIZED);
            res.render(Text::Html(
                views::login(Some(Message::Error("Bad credentials")), None).into_string(),
            ));
            return Ok(());
        }

        let auth = format!("{}:{}", user.as_ref().unwrap(), pass.unwrap());

        let hash = PasswordHash::new(config::admin_auth()).map_err(|_| Error::Forbidden)?;

        if Argon2::default()
            .verify_password(auth.as_bytes(), &hash)
            .is_err()
        {
            res.status_code(StatusCode::UNAUTHORIZED);
            res.render(Text::Html(
                views::login(Some(Message::Error("Bad credentials")), user.as_ref()).into_string(),
            ));
            return Ok(());
        }

        let mut session = Session::new();
        session
            .insert("access", Access::Admin)
            .map_err(|e| anyhow::anyhow!(e))?;
        depot.set_session(session);
        res.render(Redirect::other("/"));
    } else {
        res.render(Text::Html(views::login(None, None).into_string()));
    }

    Ok(())
}

#[handler]
async fn logout(depot: &mut Depot, res: &mut Response) {
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
async fn open_share_modal(req: &mut Request, res: &mut Response) -> AppResult {
    let path = req.query::<PathBuf>("path").ok_or(Error::NotFound)?;
    let add = req.query::<bool>("add").is_some_and(|v| v);

    let path_record = ExistingPathRecord::find_by_path(&path)
        .await?
        .ok_or(Error::NotFound)?;

    let shares = path_record.shares().await?;
    if add || shares.is_empty() {
        let share_record = NewShareRecord::new(path_record.id).await?;

        res.render(Text::Html(
            views::share_modal(&path_record, &share_record).into_string(),
        ));
    } else {
        res.render(Text::Html(
            views::shares(&path_record, &shares, None).into_string(),
        ));
    }

    Ok(())
}

#[handler]
async fn create_share_handler(req: &mut Request, res: &mut Response) -> AppResult {
    match req.parse_form::<NewShareRecord>().await {
        Ok(new_record) => match new_record.save().await {
            Ok(record) => {
                let path = record.path().await?;
                let shares = path.shares().await?;
                let msg = Message::Success("Share link created successfully");
                res.status_code(StatusCode::CREATED);
                res.render(Text::Html(
                    views::shares(&path, &shares, Some(msg)).into_string(),
                ));
            }
            Err(e) => {
                res.status_code(StatusCode::UNPROCESSABLE_ENTITY);
                _ = res.add_header("HX-Retarget", "#share-message", true);
                res.render(Message::Error(e.to_string().as_ref()));
            }
        },
        Err(e) => {
            res.status_code(StatusCode::UNPROCESSABLE_ENTITY);
            _ = res.add_header("HX-Retarget", "#share-message", true);
            res.render(Message::Error(e.to_string().as_ref()));
        }
    }

    Ok(())
}

#[handler]
async fn shares_handler(req: &Request, res: &mut Response) -> AppResult {
    let path = req.query::<PathBuf>("path").ok_or(Error::NotFound)?;

    let path_record = ExistingPathRecord::find_by_path(&path)
        .await?
        .ok_or(Error::NotFound)?;

    let shares = path_record.shares().await?;
    res.render(Text::Html(
        views::shares(&path_record, &shares, None).into_string(),
    ));

    Ok(())
}

#[handler]
async fn delete_share_handler(req: &Request, res: &mut Response) -> AppResult {
    let url = req.param::<&str>("url").ok_or(Error::NotFound)?;

    let share_record = ExistingShareRecord::find_by_url(url)
        .await?
        .ok_or(Error::NotFound)?;

    let path_record = share_record.path().await?;
    share_record.destroy().await?;

    let shares = path_record.shares().await?;

    res.render(Text::Html(
        views::shares(
            &path_record,
            &shares,
            Some(Message::Success("Share link deleted")),
        )
        .into_string(),
    ));

    Ok(())
}

#[handler]
async fn shared_link_handler(
    depot: &mut Depot,
    req: &mut Request,
    res: &mut Response,
) -> AppResult {
    async fn grant_access(
        record: &mut ExistingShareRecord,
        res: &mut Response,
        depot: &mut Depot,
    ) -> AppResult {
        let path = record.path().await?;

        let mut session = Session::new();
        let access = Access::Shared(SharedAccess {
            expires_at: record.expires_at,
            path_buf: path.path_buf(),
        });

        let path_buf = path.path_buf();
        let redirect_path = access_scoped_path(path_buf.to_str().unwrap_or(""), &access);

        session
            .insert("access", access)
            .map_err(|e| anyhow::anyhow!(e))?;
        depot.set_session(session);

        record.access().await?;

        res.render(Redirect::other(format!("/shared/files{redirect_path}")));

        Ok(())
    }

    let url = req.param::<String>("url").ok_or(Error::NotFound)?;
    let mut share_record = ExistingShareRecord::find_by_url(&url)
        .await?
        .ok_or(Error::NotFound)?;

    ensure_shared_access_hasnt_expired(share_record.expires_at).map_err(|_| Error::NotFound)?;

    if let Some(ref share_password) = share_record.password {
        let mut msg = None;

        if req.method() == salvo::http::Method::POST {
            let password = req
                .form::<String>("password")
                .await
                .ok_or(Error::Forbidden)?;

            let hash = PasswordHash::new(share_password).map_err(|_| Error::Forbidden)?;

            if Argon2::default()
                .verify_password(password.as_bytes(), &hash)
                .is_ok()
            {
                grant_access(&mut share_record, res, depot).await?;
                return Ok(());
            }

            msg = Some(Message::Error("Invalid password"));
        }

        res.render(Text::Html(
            views::shared_link_password(&share_record, msg).into_string(),
        ));

        return Ok(());
    }

    grant_access(&mut share_record, res, depot).await?;

    Ok(())
}

#[handler]
async fn sse_connected(depot: &Depot, req: &Request, res: &mut Response) -> AppResult {
    let path = req.query::<&str>("path").unwrap_or("");
    let page = if path.is_empty() {
        Page::search("")
    } else {
        let mut path_buf = PathBuf::from(path);
        ensure_full_accessible_path(&mut path_buf, depot)?;
        Page::from(path_buf)
    };

    let (tx, rx) = mpsc::unbounded_channel::<Event>();
    let rx = UnboundedReceiverStream::new(rx);

    {
        let mut conns = SSE_CONNECTIONS
            .lock()
            .map_err(|e| anyhow::anyhow!("mutex acquiring error: {e}"))?;

        if let Some(existing) = conns.get_mut(&page) {
            existing.push(tx);
        } else {
            conns.insert(page, vec![tx]);
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
        Ok(mut all_connections) => match event {
            Event::Ping | Event::Progress(_) => {
                all_connections.retain(|_, conns| {
                    conns.retain(|tx| tx.send(event.clone()).is_ok());
                    !conns.is_empty()
                });
            }
            Event::Reload(page) => {
                if let Some(conns) = all_connections.get_mut(page) {
                    conns.retain(|tx| tx.send(event.clone()).is_ok());
                }
            }
        },
        Err(e) => {
            tracing::error!(error = ?e, "broadcasting event failed");
        }
    }
}

fn admin_routes() -> Router {
    Router::new()
        .hoop(ensure_admin_access)
        .get(list_roots_handler)
        .push(Router::with_path("files").get(list_roots_handler))
        .push(Router::with_path("logout").delete(logout))
        .push(
            Router::with_path("share")
                .get(open_share_modal)
                .post(create_share_handler)
                .push(Router::with_path("{url}").delete(delete_share_handler)),
        )
        .push(Router::with_path("shares").get(shares_handler))
}

fn protected_routes() -> Router {
    Router::new()
        .hoop(ensure_some_access)
        .push(Router::with_path("files").push(Router::with_path("{*path}").get(path_handler)))
        .push(Router::with_path("preview").get(preview_handler))
        .push(Router::with_path("download").get(download_handler))
        .push(Router::with_path("search").get(search_handler))
        .push(Router::with_path("sse").get(sse_connected))
}

pub(crate) async fn server() -> (Server<TcpAcceptor>, Router) {
    let acceptor = TcpListener::new(config::get().server.addr()).bind().await;

    let session = SessionHandler::builder(CookieStore::new(), config::secret())
        .build()
        .expect("failed to build session store");

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
        .push(
            Router::with_path("shared")
                .push(
                    Router::with_path("link/{url}")
                        .get(shared_link_handler)
                        .post(shared_link_handler),
                )
                .push(protected_routes()),
        )
        .push(protected_routes())
        .push(admin_routes());

    let server = Server::new(acceptor);
    (server, router)
}
