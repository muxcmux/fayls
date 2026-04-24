use std::path::Path;

use salvo::{
    conn::tcp::TcpAcceptor,
    http::{HeaderValue, header},
    prelude::*,
    writing::Json,
};
use sqlx::{Pool, Sqlite, SqlitePool};
use tokio::sync::mpsc::Sender;

use crate::{app::Event, config::Config, fayls::Fayl};

#[derive(Clone)]
struct AppContext {
    config: Config,
    db: Pool<Sqlite>,
    tx: Sender<Event>,
}

async fn list_entries(path: &Path, db: &SqlitePool) -> Json<Vec<Fayl>> {
    let items = sqlx::query_as::<_, Fayl>(
        r"
        SELECT name, parent, kind, size, checksum, last_modified
        FROM fayls
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

    let ctx = depot.obtain::<AppContext>().map_err(|_| {
        tracing::error!("app state missing from depot");
        StatusError::internal_server_error()
    })?;

    res.render(list_entries(requested_path, &ctx.db).await);
    Ok(())
}

pub async fn server(
    config: Config,
    db: Pool<Sqlite>,
    tx: Sender<Event>,
) -> (Server<TcpAcceptor>, Router) {
    let state = AppContext { config, db, tx };

    let acceptor = TcpListener::new(state.config.server.addr()).bind().await;

    let router = Router::new()
        .hoop(affix_state::inject(state))
        .hoop(force_json_format)
        .get(list_files_handler);

    let server = Server::new(acceptor);
    (server, router)
}
