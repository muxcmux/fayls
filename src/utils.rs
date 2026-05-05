use std::{collections::HashMap, sync::LazyLock};

use maud::{Markup, html};
use sqlx::{Database, Encode, FromRow, IntoArguments, Type, query::QueryAs};

use crate::fayls::{ExistingFayl, FaylKind};

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

const FOLDER_SVG: &str =
    "M10,4H4C2.89,4 2,4.89 2,6V18A2,2 0 0,0 4,20H20A2,2 0 0,0 22,18V8C22,6.89 21.1,6 20,6H12L10,4Z";
const FILE_SVG: &str =
    "M14,2H6A2,2 0 0,0 4,4V20A2,2 0 0,0 6,22H18A2,2 0 0,0 20,20V8L14,2M18,20H6V4H13V9H18V20Z";
const DOCUMENT_SVG: &str = "M6,2A2,2 0 0,0 4,4V20A2,2 0 0,0 6,22H18A2,2 0 0,0 20,20V8L14,2H6M6,4H13V9H18V20H6V4M8,12V14H16V12H8M8,16V18H13V16H8Z";
const IMAGE_FILE_SVG: &str = "M14,2L20,8V20A2,2 0 0,1 18,22H6A2,2 0 0,1 4,20V4A2,2 0 0,1 6,2H14M18,20V9H13V4H6V20H18M17,13V19H7L12,14L14,16M10,10.5A1.5,1.5 0 0,1 8.5,12A1.5,1.5 0 0,1 7,10.5A1.5,1.5 0 0,1 8.5,9A1.5,1.5 0 0,1 10,10.5Z";
const VIDEO_FILE_SVG: &str = "M14,2L20,8V20A2,2 0 0,1 18,22H6A2,2 0 0,1 4,20V4A2,2 0 0,1 6,2H14M18,20V9H13V4H6V20H18M16,18L13.5,16.3V18H8V13H13.5V14.7L16,13V18Z";
const AUDIO_FILE_SVG: &str = "M14,2L20,8V20A2,2 0 0,1 18,22H6A2,2 0 0,1 4,20V4A2,2 0 0,1 6,2H14M18,20V9H13V4H6V20H18M13,10V12H11V17A2,2 0 0,1 9,19A2,2 0 0,1 7,17A2,2 0 0,1 9,15C9.4,15 9.7,15.1 10,15.3V10H13Z";
const SHEET_SVG: &str = "M14 2H6C4.9 2 4 2.9 4 4V20C4 21.1 4.9 22 6 22H18C19.1 22 20 21.1 20 20V8L14 2M18 20H6V4H13V9H18V20M9 13V19H7V13H9M15 15V19H17V15H15M11 11V19H13V11H11Z";

static ICON_MAP: LazyLock<HashMap<&str, &str>> = LazyLock::new(|| {
    HashMap::from([
        ("", FILE_SVG),
        ("png", IMAGE_FILE_SVG),
        ("svg", IMAGE_FILE_SVG),
        ("jpg", IMAGE_FILE_SVG),
        ("jpeg", IMAGE_FILE_SVG),
        ("heic", IMAGE_FILE_SVG),
        ("bmp", IMAGE_FILE_SVG),
        ("tiff", IMAGE_FILE_SVG),
        ("gif", IMAGE_FILE_SVG),
        ("webp", IMAGE_FILE_SVG),
        ("mid", AUDIO_FILE_SVG),
        ("midi", AUDIO_FILE_SVG),
        ("mp3", AUDIO_FILE_SVG),
        ("flac", AUDIO_FILE_SVG),
        ("m4p", AUDIO_FILE_SVG),
        ("m4a", AUDIO_FILE_SVG),
        ("hevc", VIDEO_FILE_SVG),
        ("m4v", VIDEO_FILE_SVG),
        ("mov", VIDEO_FILE_SVG),
        ("mkv", VIDEO_FILE_SVG),
        ("avi", VIDEO_FILE_SVG),
        ("html", DOCUMENT_SVG),
        ("rtf", DOCUMENT_SVG),
        ("info", DOCUMENT_SVG),
        ("doc", DOCUMENT_SVG),
        ("docx", DOCUMENT_SVG),
        ("pdf", DOCUMENT_SVG),
        ("md", DOCUMENT_SVG),
        ("txt", DOCUMENT_SVG),
        ("csv", SHEET_SVG),
        ("ods", SHEET_SVG),
        ("numbers", SHEET_SVG),
        ("xls", SHEET_SVG),
        ("xlsx", SHEET_SVG),
    ])
});
pub fn fayl_icon(fayl: &ExistingFayl) -> Markup {
    let data = if fayl.kind == FaylKind::Directory {
        FOLDER_SVG
    } else {
        let ext = fayl
            .name
            .split('.')
            .next_back()
            .unwrap_or("")
            .to_lowercase();
        ICON_MAP.get(ext.as_str()).unwrap_or(&FILE_SVG)
    };
    html! {
        svg fill="currentColor" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" {
            path d=(data);
        } {}
    }
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
