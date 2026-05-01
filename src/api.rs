use salvo::{
    conn::tcp::TcpAcceptor,
    http::{HeaderValue, header},
    prelude::*,
    writing::Json,
};
use serde::Serialize;
use sqlx::SqlitePool;

use crate::{
    config,
    fayls::ExistingFayl,
    utils::{bind_vec, expand_vec_placeholder},
};

/// A common error struct for the API which wraps
/// anyhow and sqlx errors and can be converted into
/// a salvo response
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// Use for 401 responses
    #[error("Authentication required")]
    Unauthorized,
    /// Use for 403 responses
    #[error("You are not allowed to perform this action")]
    Forbidden,
    /// Use for 404 responses
    #[error("Resource not found")]
    NotFound,
    /// Use when request is correct, but contains semantic errors,
    /// e.g. fields fail validation criteria
    #[error("{}", .0)]
    UnprocessableEntity(&'static str),
    /// Wrapper around database errors which returns 404s
    /// when rows can't be found and 500s for anything else
    #[error("{}",
        match .0 {
            sqlx::Error::RowNotFound => "Resource not found",
            _ => "An internal server error occurred"
        }
    )]
    Sqlx(#[from] sqlx::Error),
    /// Any other anyhow errors
    /// this is convenient when used with anyhow!("error")
    /// or when we want to return early from a function
    /// with `anyhow::bail!("things crashed")`
    /// for this to work, the fn must return an anyhow result
    /// which is then converted into this enum variant
    #[error("An internal server error occured")]
    Anyhow(#[from] anyhow::Error),
    // #[error("{}", .0)]
    // Json(#[from] serde_json::Error),
    #[error("{}", .0)]
    BadRequest(&'static str),
}

impl Error {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::UnprocessableEntity(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Anyhow(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            // Self::Json(_) => StatusCode::BAD_REQUEST,
            Self::Sqlx(e) => match e {
                sqlx::Error::RowNotFound => StatusCode::NOT_FOUND,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            },
        }
    }
}

impl Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Error::Unauthorized => serializer.serialize_unit_variant("Error", 0, "Unauthorized"),
            Error::Forbidden => serializer.serialize_unit_variant("Error", 1, "Forbidden"),
            Error::NotFound => serializer.serialize_unit_variant("Error", 2, "NotFound"),
            Error::UnprocessableEntity(_) => {
                serializer.serialize_unit_variant("Error", 3, "UnprocessableEntity")
            }
            Error::Sqlx(e) => {
                tracing::error!("sqlx error: {e}");
                serializer.serialize_unit_variant("Error", 4, "Sqlx")
            }
            Error::Anyhow(e) => {
                tracing::error!("anyhow error: {e}");
                serializer.serialize_unit_variant("Error", 5, "Anyhow")
            }
            Error::BadRequest(e) => {
                tracing::warn!("bad request: {e}");
                serializer.serialize_unit_variant("Error", 6, "BadRequest")
            }
        }
    }
}

pub type Result<T = (), E = Error> = std::result::Result<T, E>;

#[async_trait]
impl Writer for Error {
    async fn write(mut self, _req: &mut Request, _depot: &mut Depot, res: &mut Response) {
        res.status_code(self.status_code());
        res.render(Json(self));
    }
}

#[async_trait]
impl Writer for ExistingFayl {
    async fn write(mut self, _req: &mut Request, _depot: &mut Depot, res: &mut Response) {
        res.render(Json(self));
    }
}

async fn list_entries(paths: Vec<Option<String>>, db: &SqlitePool) -> Result<Vec<ExistingFayl>> {
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
    Ok(bind_vec(query, &paths).fetch_all(db).await?)
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
async fn list_files_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) -> Result {
    let path = req.param::<String>("path");

    let db = depot
        .obtain::<SqlitePool>()
        .map_err(|_| anyhow::anyhow!("can't get db"))?;

    res.render(Json(list_entries(vec![path], db).await?));

    Ok(())
}

#[handler]
async fn list_roots_handler(depot: &mut Depot, res: &mut Response) -> Result {
    let db = depot
        .obtain::<SqlitePool>()
        .map_err(|_| anyhow::anyhow!("can't get db"))?;

    let roots = config::get()
        .app
        .canonicalized_sources()
        .iter()
        .map(|s| s.parent().map(|p| p.to_string_lossy().to_string()))
        .collect();

    res.render(Json(list_entries(roots, db).await?));

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

#[handler]
async fn search_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) -> Result {
    let query = req
        .query::<&str>("q")
        .ok_or(Error::BadRequest("no query param"))?;

    let db = depot
        .obtain::<SqlitePool>()
        .map_err(|_| anyhow::anyhow!("can't get db"))?;

    let items = sqlx::query_as::<_, ExistingFayl>(if is_valid_fts(query, db).await {
        FTS_QUERY
    } else {
        FILENAME_QUERY
    })
    .bind(query)
    .fetch_all(db)
    .await?;

    res.render(Json(items));

    Ok(())
}

pub async fn server(db: SqlitePool) -> (Server<TcpAcceptor>, Router) {
    let acceptor = TcpListener::new(config::get().server.addr()).bind().await;

    let router = Router::with_path("api")
        .hoop(affix_state::inject(db))
        .hoop(force_json_format)
        .push(
            Router::with_path("files")
                .get(list_roots_handler)
                .push(Router::new().path("{path}").get(list_files_handler)),
        )
        .push(Router::with_path("search").get(search_handler));

    let server = Server::new(acceptor);
    (server, router)
}
