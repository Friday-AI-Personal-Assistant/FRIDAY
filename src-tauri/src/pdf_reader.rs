use lopdf::Document;
use serde::Serialize;
use std::{fs, path::PathBuf};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;

const MAX_PDF_BYTES: u64 = 25 * 1024 * 1024;
const MAX_PDF_PAGES: usize = 500;
const MAX_EXTRACTED_CHARS: usize = 120_000;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfPage {
    page_number: u32,
    text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfDocument {
    file_name: String,
    page_count: usize,
    extracted_chars: usize,
    pages: Vec<PdfPage>,
    needs_ocr: bool,
}

fn extract_selected_pdf(path: PathBuf) -> Result<PdfDocument, String> {
    let metadata = fs::symlink_metadata(&path)
        .map_err(|e| format!("Could not inspect the selected PDF: {e}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("The selected item is not a regular PDF file.".into());
    }
    if metadata.len() == 0 || metadata.len() > MAX_PDF_BYTES {
        return Err("PDF files must be between 1 byte and 25 MB for this first release.".into());
    }
    if !path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("pdf")) {
        return Err("Please select a file with the .pdf extension.".into());
    }

    let document = Document::load(&path)
        .map_err(|_| "FRIDAY could not read this PDF. It may be damaged, password-protected, or unsupported.".to_string())?;
    let page_map = document.get_pages();
    if page_map.is_empty() {
        return Err("This PDF does not contain any readable pages.".into());
    }
    if page_map.len() > MAX_PDF_PAGES {
        return Err("This PDF has more than 500 pages. Please use a smaller document.".into());
    }

    let mut pages = Vec::with_capacity(page_map.len());
    let mut extracted_chars = 0usize;
    for page_number in page_map.keys().copied() {
        let text = document.extract_text_with_limit(&[page_number], 2 * 1024 * 1024).unwrap_or_else(|_| "[This page could not be safely extracted.]".to_string());
        let remaining = MAX_EXTRACTED_CHARS.saturating_sub(extracted_chars);
        let bounded: String = text.chars().take(remaining).collect();
        extracted_chars += bounded.chars().count();
        pages.push(PdfPage { page_number, text: bounded });
        if extracted_chars >= MAX_EXTRACTED_CHARS { break; }
    }

    let needs_ocr = pages.iter().all(|page| page.text.trim().is_empty());
    let file_name = path.file_name().and_then(|name| name.to_str())
        .unwrap_or("Selected PDF").chars().take(180).collect();

    Ok(PdfDocument {
        file_name,
        page_count: page_map.len(),
        extracted_chars,
        pages,
        needs_ocr,
    })
}

#[tauri::command]
pub async fn pick_and_read_pdf(app: AppHandle) -> Result<PdfDocument, String> {
    let selected = app.dialog().file()
        .add_filter("PDF document", &["pdf"])
        .blocking_pick_file();
    let Some(selected) = selected else {
        return Err("PDF selection was cancelled.".into());
    };
    let path = selected.into_path()
        .map_err(|_| "FRIDAY could not access the selected PDF path.".to_string())?;
    extract_selected_pdf(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_pdf_extensions() {
        let path = PathBuf::from("example.txt");
        assert!(!path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("pdf")));
    }

    #[test]
    fn extraction_limits_are_bounded() {
        assert_eq!(MAX_PDF_BYTES, 25 * 1024 * 1024);
        assert_eq!(MAX_PDF_PAGES, 500);
        assert_eq!(MAX_EXTRACTED_CHARS, 120_000);
    }
}
