use std::path::Path;

use salvo::{
    conn::tcp::TcpAcceptor,
    http::{HeaderValue, header},
    prelude::*,
    writing::Json,
};
use sqlx::SqlitePool;

use crate::{config, fayls::ExistingFayl};

async fn list_entries(path: &Path, db: &SqlitePool) -> Json<Vec<ExistingFayl>> {
    let items = sqlx::query_as::<_, ExistingFayl>(
        r"
        SELECT * FROM fayls
        WHERE parent = ?
        ORDER BY
            CASE WHEN kind = 'directory' THEN 0 ELSE 1 END,
            name
    ",
    )
    .bind(path.to_string_lossy().as_ref())
    .fetch_all(db)
    .await
    .unwrap();

    Json(items)
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
async fn list_files_handler(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
) -> Result<(), StatusError> {
    let path = req
        .query::<String>("path")
        .ok_or_else(StatusError::bad_request)?;

    let requested_path = Path::new(&path);
    if !requested_path.exists() {
        return Err(StatusError::not_found());
    }

    let db = depot.obtain::<SqlitePool>().map_err(|_| {
        tracing::error!("can' get db");
        StatusError::internal_server_error()
    })?;

    res.render(list_entries(requested_path, db).await);
    Ok(())
}

async fn search(query: &str, db: &SqlitePool) -> Json<Vec<ExistingFayl>> {
    let items = sqlx::query_as::<_, ExistingFayl>(
        r"
        -- :q_like   -> pattern for file names (e.g. '%report%')
        -- :q_match  -> FTS5 query (e.g. 'sqlite NEAR/5 search')

        WITH
        -- 1) Name matches from fayls
        name_hits AS (
            SELECT
                f.id,
                f.name,
                f.parent,
                f.kind,
                f.size,
                f.checksum,
                f.last_modified,
                f.processed,
                1 AS result_group,          -- ensures these come first
                NULL AS rank
            FROM fayls AS f
            WHERE f.name LIKE '%' || ?1 || '%'
        ),

        -- 2) Full-text matches from content_index
        fts_hits AS (
            SELECT
                f.id,
                f.name,
                f.parent,
                f.kind,
                f.size,
                f.checksum,
                f.last_modified,
                f.processed,
                2 AS result_group,          -- after LIKE results
                bm25(content_index, 10.0, 5.0) AS rank
            FROM content_index
            JOIN fayls AS f
              ON f.id = content_index.rowid   -- or content_index.id if stored explicitly
            WHERE content_index MATCH ?1
        )

        -- 3) Combine both sets
        SELECT *
        FROM (
            SELECT * FROM name_hits
            UNION ALL
            SELECT * FROM fts_hits
        )
        ORDER BY
            result_group ASC,                 -- LIKE first, MATCH second
            rank ASC;                         -- bm25: lower is better
    ",
    )
    .bind(query)
    .fetch_all(db)
    .await
    .unwrap();

    Json(items)
}

#[handler]
async fn search_handler(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
) -> Result<(), StatusError> {
    let query = req
        .query::<&str>("q")
        .ok_or_else(StatusError::bad_request)?;

    let db = depot.obtain::<SqlitePool>().map_err(|_| {
        tracing::error!("can't get db");
        StatusError::internal_server_error()
    })?;

    res.render(search(query, db).await);

    Ok(())
}

pub async fn server(db: SqlitePool) -> (Server<TcpAcceptor>, Router) {
    let acceptor = TcpListener::new(config::get().server.addr()).bind().await;

    let router = Router::new()
        .hoop(affix_state::inject(db))
        .hoop(force_json_format)
        .get(list_files_handler)
        .push(Router::with_path("search").get(search_handler));

    let server = Server::new(acceptor);
    (server, router)
}
