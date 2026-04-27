use std::path::Path;

use salvo::{
    conn::tcp::TcpAcceptor,
    http::{HeaderValue, header},
    prelude::*,
    writing::Json,
};
use sqlx::SqlitePool;

use crate::{config, fayls::Fayl};

async fn list_entries(path: &Path, db: &SqlitePool) -> Json<Vec<Fayl>> {
    let items = sqlx::query_as::<_, Fayl>(
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

    res.render(list_entries(requested_path, &db).await);
    Ok(())
}

pub async fn server(db: SqlitePool) -> (Server<TcpAcceptor>, Router) {
    let acceptor = TcpListener::new(config::get().server.addr()).bind().await;

    let router = Router::new()
        .hoop(affix_state::inject(db))
        .hoop(force_json_format)
        .get(list_files_handler);

    let server = Server::new(acceptor);
    (server, router)
}
