use anyhow::{Result, anyhow, bail};
use dotext::doc::{HasKind, OpenOfficeDoc};
use dotext::{Docx, MsDoc, Odp, Ods, Odt, Pptx, Xlsx};
use epub::doc::EpubDoc;
use pdf_oxide::PdfDocument;
use std::io::{Cursor, Read};
use std::path::Path;

fn main() -> Result<()> {
    let arg = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow!("no file provided\nUSAGE: extractpdf [FILE]"))?;

    let path = Path::new(&arg);

    match path
        .extension()
        .expect("No file extension found")
        .to_string_lossy()
        .to_ascii_lowercase()
        .as_ref()
    {
        "pdf" => pdf(path),
        "docx" => ms_office(Docx::open(path).expect("Cannot open file")),
        "pptx" => ms_office(Pptx::open(path).expect("Cannot open file")),
        "xlsx" => ms_office(Xlsx::open(path).expect("Cannot open file")),
        "odp" => open_office(Odp::open(path).expect("Cannot open file")),
        "ods" => open_office(Ods::open(path).expect("Cannot open file")),
        "odt" => open_office(Odt::open(path).expect("Cannot open file")),
        "epub" => epub(path),
        _ => bail!("Unknown file format"),
    }
}

fn ms_office<F: MsDoc<T>, T: Read + HasKind>(mut file: F) -> Result<()> {
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    println!("{contents}");

    Ok(())
}

fn open_office<F: OpenOfficeDoc<T>, T: Read + HasKind>(mut file: F) -> Result<()> {
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    println!("{contents}");

    Ok(())
}

fn pdf(path: &Path) -> Result<()> {
    let doc = PdfDocument::open(path)?;
    let len = doc.page_count()?;

    for i in 0..len {
        println!("{}", doc.extract_text(i)?);
    }

    Ok(())
}

fn epub(path: &Path) -> Result<()> {
    let contents = std::fs::read(path)?;
    let cursor = Cursor::new(contents);
    let mut doc = EpubDoc::from_reader(cursor).map_err(|e| anyhow::anyhow!(e))?;

    if let Some(title) = doc.get_title() {
        println!("{title}");
    }

    if let Some((chapter, _)) = doc.get_current_str() {
        println!("{chapter}");
    }

    while doc.go_next() {
        if let Some((chapter, _)) = doc.get_current_str() {
            println!("{chapter}");
        }
    }

    Ok(())
}
