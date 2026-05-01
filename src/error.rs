use salvo::{Depot, Request, Response, Writer, async_trait, http::StatusCode, writing::Json};
use serde::Serialize;

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
