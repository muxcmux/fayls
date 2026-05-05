mod views;

use crate::{
    app,
    error::{Error, Result},
};
use salvo::{conn::tcp::TcpAcceptor, fs::NamedFile, prelude::*};
use serde::Deserialize;

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

#[handler]
async fn list_files_handler(req: &mut Request, res: &mut Response) -> Result {
    let path = req.param::<&str>("path").map(|p| format!("/{p}"));
    let (sort, order) = get_sorting(req);

    let items = list_entries(vec![path], &sort, &order).await?;

    res.render(Text::Html(
        if is_hx(req) {
            views::file_list(&items, &sort, &order, req.queries())
        } else {
            views::layout(
                "Fayls",
                &views::file_list(&items, &sort, &order, req.queries()),
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
            views::file_list(&items, &sort, &order, req.queries())
        } else {
            views::layout(
                "Fayls",
                &views::file_list(&items, &sort, &order, req.queries()),
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
    res.render(Text::Html(
        views::layout(
            &format!("Searching fo {query}"),
            &views::file_list(&items, &sort, &order, req.queries()),
        )
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
        // always needs to be last
        .push(Router::with_path("{*file}").get(serve_static_file));

    let server = Server::new(acceptor);
    (server, router)
}
