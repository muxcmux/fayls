use crate::{
    app,
    error::{Error, Result},
};
use salvo::{
    conn::tcp::TcpAcceptor,
    http::{HeaderValue, header},
    prelude::*,
    writing::Json,
};

use crate::{
    config,
    fayls::ExistingFayl,
    utils::{bind_vec, expand_vec_placeholder},
};

async fn list_entries(paths: Vec<Option<String>>) -> Result<Vec<ExistingFayl>> {
    let sql = expand_vec_placeholder(
        r"
        SELECT * FROM fayls
        WHERE parent IN (?)
        ORDER BY
            CASE WHEN kind = 'directory' THEN 0 ELSE 1 END,
            last_modified
        ",
        paths.len(),
    );
    let query = sqlx::query_as::<_, ExistingFayl>(&sql);
    Ok(bind_vec(query, &paths).fetch_all(app::db()).await?)
}

#[handler]
async fn force_json_format(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
    ctrl: &mut FlowCtrl,
) {
    req.headers_mut()
        .insert(header::ACCEPT, HeaderValue::from_static("application/json"));

    ctrl.call_next(req, depot, res).await;

    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
}

#[handler]
async fn list_files_handler(req: &mut Request, res: &mut Response) -> Result {
    let path = req.param::<String>("path");

    res.render(Json(list_entries(vec![path]).await?));

    Ok(())
}

#[handler]
async fn list_roots_handler(res: &mut Response) -> Result {
    let roots = config::get()
        .app
        .canonicalized_sources()
        .iter()
        .map(|s| s.parent().map(|p| p.to_string_lossy().to_string()))
        .collect();

    res.render(Json(list_entries(roots).await?));

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

    res.render(Json(items));

    Ok(())
}

pub async fn server() -> (Server<TcpAcceptor>, Router) {
    let acceptor = TcpListener::new(config::get().server.addr()).bind().await;

    let router = Router::with_path("api")
        .hoop(force_json_format)
        .hoop(
            Compression::new()
                .enable_brotli(CompressionLevel::Default)
                .enable_gzip(CompressionLevel::Default)
                .enable_deflate(CompressionLevel::Default)
                .enable_zstd(CompressionLevel::Default),
        )
        .hoop(CachingHeaders::new())
        .push(
            Router::with_path("files")
                .get(list_roots_handler)
                .push(Router::new().path("{path}").get(list_files_handler)),
        )
        .push(Router::with_path("search").get(search_handler));

    let server = Server::new(acceptor);
    (server, router)
}
