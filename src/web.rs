mod views;
use futures_util::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{LazyLock, Mutex},
};

use crate::{
    app, content_indexing,
    error::{Error, Result},
    web::views::View,
};
use salvo::{conn::tcp::TcpAcceptor, fs::NamedFile, prelude::*};
use serde::Deserialize;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::{
    config,
    fayls::ExistingFayl,
    utils::{bind_vec, expand_vec_placeholder},
};

fn is_hx(req: &Request) -> bool {
    req.header::<bool>("hx-request").is_some_and(|v| v)
}

#[derive(Default, Deserialize, PartialEq)]
pub enum Order {
    #[default]
    #[serde(rename = "desc")]
    Desc,
    #[serde(rename = "asc")]
    Asc,
}

#[derive(Default, Deserialize, PartialEq)]
pub enum Sort {
    #[default]
    #[serde(rename = "last_modified")]
    LastModified,
    #[serde(rename = "name")]
    Name,
    #[serde(rename = "size")]
    Size,
}

impl Order {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }

    fn reverse(&self) -> Self {
        match self {
            Self::Desc => Self::Asc,
            Self::Asc => Self::Desc,
        }
    }
}

impl Sort {
    fn as_str(&self) -> &'static str {
        match self {
            Self::LastModified => "last_modified",
            Self::Name => "name",
            Self::Size => "size",
        }
    }

    fn humanize(&self) -> &'static str {
        match self {
            Self::LastModified => "Last Modified",
            Self::Name => "Name",
            Self::Size => "Size",
        }
    }
}

async fn list_entries(paths: &[&str], sort: &Sort, order: &Order) -> Result<Vec<ExistingFayl>> {
    let sql = expand_vec_placeholder(
        &format!(
            r"
            SELECT * FROM fayls
            WHERE parent IN (?)
            ORDER BY
                CASE WHEN kind = 'directory' THEN 0 ELSE 1 END,
                {} {}
            ",
            sort.as_str(),
            order.as_str()
        ),
        paths.len(),
    );
    let query = sqlx::query_as::<_, ExistingFayl>(&sql);

    Ok(bind_vec(query, paths).fetch_all(app::db()).await?)
}

pub(crate) fn get_sorting(req: &Request) -> (Sort, Order) {
    let sort = req.query::<Sort>("sort").unwrap_or_default();
    let order = req.query::<Order>("order").unwrap_or_default();
    (sort, order)
}

#[handler]
async fn list_files_handler(req: &mut Request, res: &mut Response) -> Result {
    // unwrapping here is safe because of the routes guard where
    // we handle the /files path by serving root dirs
    let path = req
        .param::<&str>("path")
        .map(|p| format!("/{p}"))
        .expect("this can't happen");

    let (sort, order) = get_sorting(req);
    let items = list_entries(&[path.as_ref()], &sort, &order).await?;

    let file_list = views::file_list(
        &View::Path(PathBuf::from(path)),
        &items,
        content_indexing::get_progress().await?,
        req,
    );

    res.render(Text::Html(
        if is_hx(req) {
            file_list
        } else {
            views::layout("Fayls", &file_list)
        }
        .into_string(),
    ));

    Ok(())
}

#[handler]
async fn list_roots_handler(req: &Request, res: &mut Response) -> Result {
    let roots = config::get()
        .app
        .canonicalized_sources()
        .iter()
        .filter_map(|s| s.parent().and_then(|p| p.to_str()))
        .collect::<Vec<&str>>();

    let (sort, order) = get_sorting(req);

    let items = list_entries(&roots, &sort, &order).await?;

    let file_list = views::file_list(
        &View::Root,
        &items,
        content_indexing::get_progress().await?,
        req,
    );

    res.render(Text::Html(
        if is_hx(req) {
            file_list
        } else {
            views::layout("Fayls", &file_list)
        }
        .into_string(),
    ));

    Ok(())
}

const FILENAME_QUERY: &str = "SELECT * FROM fayls WHERE name LIKE '%' || ? || '%' LIMIT 20";

const FTS_QUERY: &str = r"
    WITH
    name_hits AS (
        SELECT
            f.*,
            1 AS result_group,
            NULL AS rank
        FROM fayls AS f
        WHERE f.name LIKE '%' || ?1 || '%'
    ),

    fts_hits AS (
        SELECT
            f.*,
            2 AS result_group,
            bm25(content_index, 10.0, 5.0) AS rank
        FROM content_index
        JOIN fayls AS f
          ON f.id = content_index.rowid
        WHERE content_index MATCH ?1
          AND f.id NOT IN (SELECT id FROM name_hits)
    )

    SELECT *
    FROM (
        SELECT * FROM name_hits
        UNION ALL
        SELECT * FROM fts_hits
    )
    ORDER BY
        result_group ASC,
        rank ASC
    LIMIT 20
";

async fn is_valid_fts(query: &str) -> bool {
    match sqlx::query("SELECT content FROM fts_query_validator WHERE content MATCH ?")
        .bind(query)
        .fetch_optional(app::db())
        .await
    {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("bad fts query: {e}");
            false
        }
    }
}

#[handler]
async fn search_handler(req: &mut Request, res: &mut Response) -> Result {
    let query = req
        .query::<&str>("q")
        .ok_or(Error::BadRequest("no query param"))?;

    let items = sqlx::query_as::<_, ExistingFayl>(if is_valid_fts(query).await {
        FTS_QUERY
    } else {
        FILENAME_QUERY
    })
    .bind(query)
    .fetch_all(app::db())
    .await?;

    let file_list = views::file_list(
        &View::Search,
        &items,
        content_indexing::get_progress().await?,
        req,
    );

    res.render(Text::Html(
        if is_hx(req) {
            file_list
        } else {
            views::layout(&format!("Results for {query}"), &file_list)
        }
        .into_string(),
    ));

    Ok(())
}

#[handler]
async fn serve_static_file(req: &mut Request, res: &mut Response) -> Result {
    let file = req.param::<&str>("file").ok_or(Error::NotFound)?;

    NamedFile::builder(file).send(req.headers(), res).await;

    Ok(())
}

#[handler]
async fn sse_connected(req: &Request, res: &mut Response) -> Result {
    let view = req.param::<View>("view").unwrap_or(View::Search);

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

type SseConnections = Mutex<HashMap<View, Vec<UnboundedSender<Event>>>>;
static SSE_CONNECTIONS: LazyLock<SseConnections> = LazyLock::new(SseConnections::default);

#[derive(Clone)]
pub enum Event {
    Ping,
    Refresh(View),
    // (indexed, total)
    Progress((i64, i64)),
}

impl Event {
    fn into_sse_event(self) -> SseEvent {
        match self {
            Self::Ping => SseEvent::default().name("ping"),
            Self::Refresh(_) => SseEvent::default()
                .name("refresh")
                .text("pls refresh yourself"),
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

pub fn broadcast(event: &Event) {
    match SSE_CONNECTIONS.lock() {
        Ok(mut connections) => connections.retain(|_, conns| {
            conns.retain(|tx| tx.send(event.clone()).is_ok());
            !conns.is_empty()
        }),
        Err(e) => {
            tracing::error!(error = ?e, "broadcasting event failed");
        }
    }
}

pub fn broadcast_to_view(event: &Event, view: &View) {
    match SSE_CONNECTIONS.lock() {
        Ok(mut connections) => {
            if let Some(conns) = connections.get_mut(view) {
                conns.retain(|tx| tx.send(event.clone()).is_ok());
            }
        }
        Err(e) => {
            tracing::error!(error = ?e, "broadcasting event failed");
        }
    }
}

pub async fn server() -> (Server<TcpAcceptor>, Router) {
    let acceptor = TcpListener::new(config::get().server.addr()).bind().await;

    let router = Router::new()
        .hoop(
            Compression::new()
                .enable_brotli(CompressionLevel::Default)
                .enable_gzip(CompressionLevel::Default),
        )
        .hoop(CachingHeaders::new())
        .get(list_roots_handler)
        .push(
            Router::with_path("files")
                .get(list_roots_handler)
                .push(Router::with_path("{*path}").get(list_files_handler)),
        )
        .push(Router::with_path("search").get(search_handler))
        .push(
            Router::with_path("sse")
                .get(sse_connected)
                .push(Router::with_path("{*view}").get(sse_connected)),
        )
        // always needs to be last
        .push(Router::with_path("{*file}").get(serve_static_file));

    let server = Server::new(acceptor);
    (server, router)
}
