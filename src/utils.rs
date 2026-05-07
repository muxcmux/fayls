use multimap::MultiMap;
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

pub(crate) fn expand_vec_placeholder(q: &str, len: usize) -> String {
    let mut r = String::from("(");
    for _ in 1..len {
        r.push_str("?, ");
    }
    r.push_str("?)");
    q.replace("(?)", &r)
}

const BYTE_UNITS: &[&str] = &["bytes", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
const STEP: f64 = 1024.0;

pub fn format_size(bytes: i64) -> String {
    let mut value = bytes as f64;
    let mut i = 0;

    while value >= STEP && i < BYTE_UNITS.len() - 1 {
        value /= STEP;
        i += 1;
    }

    let value = format!("{value:.1}");
    let value = value.trim_end_matches(".0");
    [value, BYTE_UNITS[i]].join(" ")
}

pub fn queries_to_string(queries: &MultiMap<String, String>) -> String {
    if queries.is_empty() {
        return String::new();
    }

    let mut ser = form_urlencoded::Serializer::new(String::new());
    for (k, values) in queries.iter_all() {
        for v in values {
            ser.append_pair(k, v);
        }
    }
    format!("?{}", ser.finish())
}
