use sqlx::{Database, Encode, FromRow, IntoArguments, Type, query::QueryAs};

pub fn bind_vec<'q, DB, O, B>(
    mut q: QueryAs<'q, DB, O, <DB as Database>::Arguments<'q>>,
    binds: &'q [B],
) -> QueryAs<'q, DB, O, <DB as Database>::Arguments<'q>>
where
    DB: Database,
    B: 'q + Encode<'q, DB> + Type<DB>,
    O: for<'r> FromRow<'r, DB::Row> + Send,
    <DB as Database>::Arguments<'q>: IntoArguments<'q, DB>,
{
    for b in binds {
        q = q.bind(b);
    }

    q
}

#[must_use]
pub fn expand_vec_placeholder(q: &str, len: usize) -> String {
    let mut r = String::from("(");
    for _ in 1..len {
        r.push_str("?, ");
    }
    r.push_str("?)");
    q.replace("(?)", &r)
}
