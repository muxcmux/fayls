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
            last_modified
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

const FILENAME_QUERY: &str = "SELECT * FROM fayls WHERE name LIKE '%' || ? || '%'";

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
";

async fn is_valid_fts(query: &str, db: &SqlitePool) -> bool {
    match sqlx::query("SELECT content FROM fts_query_validator WHERE content MATCH ?")
        .bind(query)
        .fetch_optional(db)
        .await
    {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("bad fts query: {e}");
            false
        }
    }
}

async fn search(query: &str, db: &SqlitePool) -> Json<Vec<ExistingFayl>> {
    let items = sqlx::query_as::<_, ExistingFayl>(if is_valid_fts(query, db).await {
        FTS_QUERY
    } else {
        FILENAME_QUERY
    })
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
