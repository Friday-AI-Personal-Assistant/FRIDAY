use lopdf::Document;
use serde::Serialize;
use std::{fs, path::PathBuf};
use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;

const MAX_PDF_BYTES: u64 = 25 * 1024 * 1024;
const MAX_PDF_PAGES: usize = 500;
const MAX_EXTRACTED_BYTES: usize = 120_000;

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
    ocr_needed_pages: Vec<u32>,
}

fn is_pdf_path(path: &std::path::Path) -> bool {
    path.extension().and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
}

fn bounded_text(text: &str, max_bytes: usize) -> String {
    let mut output = String::new();
    for character in text.chars() {
        if output.len().saturating_add(character.len_utf8()) > max_bytes { break; }
        output.push(character);
    }
    output
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
    if !is_pdf_path(&path) {
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
    let mut extracted_bytes = 0usize;
    for page_number in page_map.keys().copied() {
        let text = document.extract_text_with_limit(&[page_number], 2 * 1024 * 1024).unwrap_or_default();
        let remaining = MAX_EXTRACTED_BYTES.saturating_sub(extracted_bytes);
        let bounded = bounded_text(&text, remaining);
        extracted_bytes += bounded.len();
        pages.push(PdfPage { page_number, text: bounded });
        if extracted_bytes >= MAX_EXTRACTED_BYTES { break; }
    }

    let ocr_needed_pages: Vec<u32> = pages.iter()
        .filter(|page| page.text.trim().is_empty())
        .map(|page| page.page_number)
        .collect();
    let needs_ocr = !ocr_needed_pages.is_empty();
    let file_name = path.file_name().and_then(|name| name.to_str())
        .unwrap_or("Selected PDF").chars().take(180).collect();

    Ok(PdfDocument {
        file_name,
        page_count: page_map.len(),
        extracted_chars: pages.iter().map(|page| page.text.chars().count()).sum(),
        pages,
        needs_ocr,
        ocr_needed_pages,
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
    tauri::async_runtime::spawn_blocking(move || extract_selected_pdf(path))
        .await
        .map_err(|_| "FRIDAY's PDF reader worker stopped unexpectedly.".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_text_pdf() -> Vec<u8> {
        let content = b"BT /F1 12 Tf 72 720 Td (Hello Friday) Tj ET";
        let objects: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_vec(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            {
                let mut stream = format!("<< /Length {} >>\\nstream\\n", content.len()).into_bytes();
                stream.extend_from_slice(content);
                stream.extend_from_slice(b"\\nendstream");
                stream
            },
        ];

        let mut pdf = b"%PDF-1.4\\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\\n", index + 1).as_bytes());
            pdf.extend_from_slice(object);
            pdf.extend_from_slice(b"\\nendobj\\n");
        }

        let xref_offset = pdf.len();
        pdf.extend_from_slice(format!("xref\\n0 {}\\n", objects.len() + 1).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \\n");
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \\n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\\n<< /Size {} /Root 1 0 R >>\\nstartxref\\n{xref_offset}\\n%%EOF\\n",
                objects.len() + 1
            ).as_bytes(),
        );
        pdf
    }

    #[test]
    fn extracts_text_from_a_real_minimal_pdf_document() {
        let document = Document::load_mem(&minimal_text_pdf()).expect("minimal PDF should parse");
        let pages = document.get_pages();
        assert_eq!(pages.len(), 1);
        let page_number = *pages.keys().next().expect("one page should exist");
        let text = document
            .extract_text_with_limit(&[page_number], 2 * 1024 * 1024)
            .expect("page text should extract");
        assert!(text.contains("Hello Friday"), "unexpected extracted text: {text:?}");
    }

    #[test]
    fn accepts_pdf_extension_case_insensitively() {
        assert!(is_pdf_path(std::path::Path::new("report.pdf")));
        assert!(is_pdf_path(std::path::Path::new("REPORT.PDF")));
        assert!(!is_pdf_path(std::path::Path::new("report.txt")));
        assert!(!is_pdf_path(std::path::Path::new("report")));
    }

    #[test]
    fn bounds_extracted_text_without_splitting_utf8_characters() {
        assert_eq!(bounded_text("Friday", 3), "Fri");
        assert_eq!(bounded_text("A🦊B", 5), "A🦊");
        assert_eq!(bounded_text("A🦊B", 4), "A");
        assert_eq!(bounded_text("Friday", 0), "");
    }

    #[test]
    fn extraction_limits_are_bounded() {
        assert_eq!(MAX_PDF_BYTES, 25 * 1024 * 1024);
        assert_eq!(MAX_PDF_PAGES, 500);
        assert_eq!(MAX_EXTRACTED_BYTES, 120_000);
    }

    #[test]
    fn empty_extraction_is_not_misreported_as_readable_text() {
        let pages = vec![
            PdfPage { page_number: 1, text: "Recognised text".into() },
            PdfPage { page_number: 2, text: String::new() },
            PdfPage { page_number: 3, text: "   ".into() },
        ];
        let missing: Vec<u32> = pages.iter()
            .filter(|page| page.text.trim().is_empty())
            .map(|page| page.page_number)
            .collect();
        assert_eq!(missing, vec![2, 3]);
    }
}
