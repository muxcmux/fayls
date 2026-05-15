use std::path::Path;

use anyhow::{Result, anyhow};
use pdf_oxide::PdfDocument;

fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow!("no file provided\nUSAGE: extractpdf [FILE]"))?;

    let doc = PdfDocument::open(Path::new(&path))?;
    let len = doc.page_count()?;

    for i in 0..len {
        println!("{}", doc.extract_text(i)?);
    }

    Ok(())
}
