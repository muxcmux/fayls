mod views;
use futures_util::{FutureExt, StreamExt};
use tokio_stream::wrappers::UnboundedReceiverStream;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        LazyLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use crate::{
    app, content_indexing,
    error::{Error, Result},
    fayls::NewFayl,
};
use salvo::{
    conn::tcp::TcpAcceptor,
    fs::NamedFile,
    prelude::*,
    websocket::{Message, WebSocket},
};
use serde::Deserialize;
use tokio::sync::{
    RwLock,
    mpsc::{self, UnboundedSender},
};

use crate::{
    config,
    fayls::ExistingFayl,
    utils::{bind_vec, expand_vec_placeholder},
};

fn is_hx(req: &Request) -> bool {
    req.header::<bool>("hx-request").is_some_and(|v| v)
}

#[derive(Default, Deserialize, PartialEq)]
enum Order {
    #[default]
    #[serde(rename = "desc")]
    Desc,
    #[serde(rename = "asc")]
    Asc,
}

#[derive(Default, Deserialize, PartialEq)]
enum Sort {
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

async fn list_entries(
    paths: Vec<Option<String>>,
    sort: &Sort,
    order: &Order,
) -> Result<Vec<ExistingFayl>> {
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

    Ok(bind_vec(query, &paths).fetch_all(app::db()).await?)
}

fn get_sorting(req: &Request) -> (Sort, Order) {
    let sort = req.query::<Sort>("sort").unwrap_or_default();
    let order = req.query::<Order>("order").unwrap_or_default();
    (sort, order)
}

fn breadcrumbs(path: Option<&String>) -> Vec<PathBuf> {
    match path {
        None => vec![],
        Some(p) => {
            let mut path_buf = Some(PathBuf::from(p));
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
    }
}

#[handler]
async fn list_files_handler(req: &mut Request, res: &mut Response) -> Result {
    let path = req.param::<&str>("path").map(|p| format!("/{p}"));
    let (sort, order) = get_sorting(req);

    let breadcrumbs = breadcrumbs(path.as_ref());

    let items = list_entries(vec![path], &sort, &order).await?;

    res.render(Text::Html(
        if is_hx(req) {
            views::file_list(
                &items,
                &sort,
                &order,
                &breadcrumbs,
                false,
                content_indexing::get_progress().await?,
                req.queries(),
            )
        } else {
            views::layout(
                "Fayls",
                &views::file_list(
                    &items,
                    &sort,
                    &order,
                    &breadcrumbs,
                    false,
                    content_indexing::get_progress().await?,
                    req.queries(),
                ),
            )
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
        .map(|s| s.parent().map(|p| p.to_string_lossy().to_string()))
        .collect();

    let (sort, order) = get_sorting(req);
    let items = list_entries(roots, &sort, &order).await?;

    res.render(Text::Html(
        if is_hx(req) {
            views::file_list(
                &items,
                &sort,
                &order,
                &[],
                false,
                content_indexing::get_progress().await?,
                req.queries(),
            )
        } else {
            views::layout(
                "Fayls",
                &views::file_list(
                    &items,
                    &sort,
                    &order,
                    &[],
                    false,
                    content_indexing::get_progress().await?,
                    req.queries(),
                ),
            )
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

    let (sort, order) = get_sorting(req);
    let breadcrumbs = vec![PathBuf::from("/Search results")];
    res.render(Text::Html(
        if is_hx(req) {
            views::file_list(
                &items,
                &sort,
                &order,
                &breadcrumbs,
                true,
                content_indexing::get_progress().await?,
                req.queries(),
            )
        } else {
            views::layout(
                &format!("Results for {query}"),
                &views::file_list(
                    &items,
                    &sort,
                    &order,
                    &breadcrumbs,
                    true,
                    content_indexing::get_progress().await?,
                    req.queries(),
                ),
            )
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
async fn upgrade_to_websocket(req: &mut Request, res: &mut Response) -> Result {
    WebSocketUpgrade::new()
        .upgrade(req, res, handle_socket)
        .await
        .map_err(|e| anyhow::anyhow!("websocket error: {e}"))?;
    Ok(())
}

type WebsocketConnections = RwLock<HashMap<usize, UnboundedSender<Result<Message, salvo::Error>>>>;
static NEXT_WS_CONN_ID: AtomicUsize = AtomicUsize::new(1);
static WS_CONNECTIONS: LazyLock<WebsocketConnections> =
    LazyLock::new(WebsocketConnections::default);

pub enum Event {
    Add(Vec<NewFayl>),
    Update(Vec<ExistingFayl>),
    // (indexed, total)
    Progress((i64, i64)),
}

impl Event {
    fn into_html(self) -> String {
        match self {
            Event::Progress(progress) => maud::html! {
                hx-partial hx-target="#index-progress" {
                    (views::index_progress(progress))
                }
            }
            .into_string(),
            _ => String::new(),
        }
    }
}

pub fn broadcast(event: Event) {
    tokio::spawn(async move {
        let msg = Message::text(event.into_html());
        for (id, tx) in WS_CONNECTIONS.read().await.iter() {
            if let Err(e) = tx.send(Ok(msg.clone())) {
                tracing::error!(error = ?e, "sending to tx {id} failed");
            }
        }
    });
}

async fn handle_socket(ws: WebSocket) {
    let (ws_tx, mut ws_rx) = ws.split();
    let (tx, rx) = mpsc::unbounded_channel();
    let rx = UnboundedReceiverStream::new(rx);

    tokio::spawn(rx.forward(ws_tx).map(|result| {
        if let Err(e) = result {
            tracing::error!(error = ?e, "forwarding websocket send error");
        }
    }));

    tokio::spawn(async move {
        let next_id = NEXT_WS_CONN_ID.fetch_add(1, Ordering::Relaxed);
        WS_CONNECTIONS.write().await.insert(next_id, tx);
        tracing::info!("web socket id {next_id} connected.");
        while let Some(result) = ws_rx.next().await {
            match result {
                Ok(msg) => {
                    tracing::info!("web socket msg: {:?}", msg);
                }
                Err(e) => {
                    tracing::info!("web socket err: {e}");
                    break;
                }
            }
        }

        tracing::info!("web socket id {next_id} disconnected.");
        WS_CONNECTIONS.write().await.remove(&next_id);
    });
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
        .push(Router::with_path("ws").goal(upgrade_to_websocket))
        // always needs to be last
        .push(Router::with_path("{*file}").get(serve_static_file));

    let server = Server::new(acceptor);
    (server, router)
}
