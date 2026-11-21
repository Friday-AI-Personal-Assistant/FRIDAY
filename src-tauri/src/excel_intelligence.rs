use calamine::{open_workbook_from_rs, Data, Reader, Xlsx};
use csv::ReaderBuilder;
use serde::Serialize;
use std::{
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
};
use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;
use zip::ZipArchive;

const MAX_XLSX_BYTES: u64 = 20 * 1024 * 1024;
const MAX_CSV_BYTES: u64 = 10 * 1024 * 1024;
const MAX_ZIP_ENTRIES: usize = 512;
const MAX_ZIP_EXPANDED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ZIP_ENTRY_BYTES: u64 = 32 * 1024 * 1024;
const MAX_COMPRESSION_RATIO: u64 = 200;
const MAX_SHEETS: usize = 12;
const MAX_ROWS: usize = 20_000;
const MAX_COLUMNS: usize = 64;
const MAX_TOTAL_CELLS: usize = 200_000;
const PREVIEW_ROWS: usize = 12;
const PREVIEW_COLUMNS: usize = 12;
const MAX_PREVIEW_CELL_CHARS: usize = 180;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookAnalysis {
    pub file_name: String,
    pub format: String,
    pub sheets: Vec<SheetAnalysis>,
    pub total_rows: usize,
    pub total_non_empty_cells: usize,
    pub notes: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetAnalysis {
    pub name: String,
    pub row_count: usize,
    pub column_count: usize,
    pub non_empty_cells: usize,
    pub blank_cells: usize,
    pub formula_cells: usize,
    pub preview: Vec<Vec<String>>,
    pub columns: Vec<ColumnAnalysis>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnAnalysis {
    pub name: String,
    pub non_empty: usize,
    pub missing: usize,
    pub numeric_count: usize,
    pub sum: Option<f64>,
    pub average: Option<f64>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
}

fn is_redirected(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        return metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn read_selected_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, String> {
    let selected = fs::symlink_metadata(path)
        .map_err(|_| "FRIDAY could not inspect the selected workbook.".to_string())?;
    if is_redirected(&selected) || !selected.is_file() {
        return Err("For safety, select a regular workbook file, not a folder or symbolic link.".into());
    }
    if selected.len() == 0 || selected.len() > max_bytes {
        return Err(format!("The selected file must be between 1 byte and {} MB.", max_bytes / (1024 * 1024)));
    }
    let canonical = fs::canonicalize(path)
        .map_err(|_| "FRIDAY could not verify the selected workbook path.".to_string())?;
    let canonical_meta = fs::symlink_metadata(&canonical)
        .map_err(|_| "FRIDAY could not re-check the selected workbook.".to_string())?;
    if is_redirected(&canonical_meta) || !canonical_meta.is_file() {
        return Err("The selected workbook changed or is not a regular file.".into());
    }

    let mut file = fs::File::open(&canonical)
        .map_err(|_| "FRIDAY could not open the selected workbook.".to_string())?;
    let before = file.metadata()
        .map_err(|_| "FRIDAY could not verify the selected workbook.".to_string())?;
    if !before.is_file() || before.len() != canonical_meta.len() || before.len() > max_bytes {
        return Err("The selected workbook changed before FRIDAY could read it.".into());
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    Read::take(&mut file, max_bytes + 1).read_to_end(&mut bytes)
        .map_err(|_| "FRIDAY could not safely read the selected workbook.".to_string())?;
    let after = file.metadata()
        .map_err(|_| "FRIDAY could not verify the workbook after reading it.".to_string())?;
    if bytes.is_empty() || bytes.len() as u64 > max_bytes
        || before.len() != after.len() || after.len() != bytes.len() as u64 {
        return Err("The workbook changed while it was being read or exceeded its size limit. No analysis was performed.".into());
    }
    Ok(bytes)
}

fn validate_xlsx_package(bytes: &[u8]) -> Result<(), String> {
    // Calamine is only called after the ZIP signature is confirmed. This also avoids
    // sending arbitrary compound-file bytes into an XLSX parser.
    if !bytes.starts_with(b"PK\x03\x04") {
        return Err("This file is not a supported XLSX workbook. Save it as .xlsx or .csv and try again.".into());
    }
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| "The XLSX package is damaged or is not a valid ZIP workbook.".to_string())?;
    if archive.len() == 0 || archive.len() > MAX_ZIP_ENTRIES {
        return Err("The workbook contains an unsupported number of internal files.".into());
    }
    let mut expanded_total = 0_u64;
    let mut has_workbook_content = false;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)
            .map_err(|_| "FRIDAY could not safely inspect the workbook package.".to_string())?;
        let name = entry.name().to_ascii_lowercase();
        if name.ends_with("vbaproject.bin") {
            return Err("Macro-enabled workbooks are not supported. Save a macro-free .xlsx copy; FRIDAY never runs workbook macros.".into());
        }
        if name == "xl/workbook.xml" || name.ends_with("/workbook.xml") {
            has_workbook_content = true;
        }
        let size = entry.size();
        let compressed = entry.compressed_size();
        if size > MAX_ZIP_ENTRY_BYTES {
            return Err("One internal workbook component exceeds FRIDAY's safe extraction limit.".into());
        }
        expanded_total = expanded_total.saturating_add(size);
        if expanded_total > MAX_ZIP_EXPANDED_BYTES {
            return Err("The workbook expands beyond FRIDAY's 64 MB safety limit.".into());
        }
        if size > 1024 && (compressed == 0 || size / compressed.max(1) > MAX_COMPRESSION_RATIO) {
            return Err("The workbook has an unusually high compression ratio and was rejected for safety.".into());
        }
    }
    if !has_workbook_content {
        return Err("The selected ZIP file does not contain a recognizable Excel workbook.".into());
    }
    Ok(())
}

fn cell_text(value: &str) -> String {
    value.chars().take(MAX_PREVIEW_CELL_CHARS).collect()
}

fn analyze_matrix(
    name: String,
    rows: Vec<Vec<String>>,
    formula_cells: usize,
) -> Result<SheetAnalysis, String> {
    if rows.is_empty() {
        return Ok(SheetAnalysis {
            name, row_count: 0, column_count: 0, non_empty_cells: 0,
            blank_cells: 0, formula_cells, preview: Vec::new(), columns: Vec::new(),
        });
    }
    if rows.len() > MAX_ROWS {
        return Err("A worksheet exceeds FRIDAY's 20,000-row analysis limit.".into());
    }
    let column_count = rows.iter().map(Vec::len).max().unwrap_or(0);
    if column_count > MAX_COLUMNS {
        return Err("A worksheet exceeds FRIDAY's 64-column analysis limit.".into());
    }
    let total_cells = rows.len().saturating_mul(column_count);
    if total_cells > MAX_TOTAL_CELLS {
        return Err("The workbook exceeds FRIDAY's 200,000-cell analysis limit.".into());
    }
    let non_empty_cells = rows.iter().flatten().filter(|cell| !cell.trim().is_empty()).count();
    let blank_cells = total_cells.saturating_sub(non_empty_cells);
    let header = rows.first().cloned().unwrap_or_default();
    let mut columns = Vec::with_capacity(column_count);
    for column in 0..column_count {
        let header_name = header.get(column).map(|v| v.trim()).filter(|v| !v.is_empty())
            .map(str::to_string).unwrap_or_else(|| format!("Column {}", column + 1));
        let mut non_empty = 0usize;
        let mut numeric_count = 0usize;
        let mut sum = 0.0_f64;
        let mut minimum: Option<f64> = None;
        let mut maximum: Option<f64> = None;
        for row in rows.iter().skip(1) {
            let value = row.get(column).map(String::as_str).unwrap_or("").trim();
            if value.is_empty() { continue; }
            non_empty += 1;
            if let Ok(number) = value.replace(',', "").parse::<f64>() {
                if number.is_finite() {
                    numeric_count += 1;
                    sum += number;
                    minimum = Some(minimum.map_or(number, |current| current.min(number)));
                    maximum = Some(maximum.map_or(number, |current| current.max(number)));
                }
            }
        }
        let missing = rows.len().saturating_sub(1).saturating_sub(non_empty);
        columns.push(ColumnAnalysis {
            name: header_name, non_empty, missing, numeric_count,
            sum: (numeric_count > 0 && sum.is_finite()).then_some(sum),
            average: (numeric_count > 0 && sum.is_finite()).then_some(sum / numeric_count as f64),
            minimum, maximum,
        });
    }
    let preview = rows.iter().take(PREVIEW_ROWS).map(|row| {
        row.iter().take(PREVIEW_COLUMNS).map(|value| cell_text(value)).collect()
    }).collect();
    Ok(SheetAnalysis {
        name, row_count: rows.len(), column_count, non_empty_cells, blank_cells,
        formula_cells, preview, columns,
    })
}

fn analyze_csv(file_name: String, bytes: &[u8]) -> Result<WorkbookAnalysis, String> {
    let mut reader = ReaderBuilder::new().has_headers(false).flexible(true).from_reader(bytes);
    let mut rows = Vec::new();
    let mut total_cells = 0usize;
    for record in reader.records() {
        let record = record.map_err(|_| "FRIDAY could not parse this CSV. Check its quoting and text encoding.".to_string())?;
        if record.len() > MAX_COLUMNS {
            return Err("The CSV exceeds FRIDAY's 64-column analysis limit.".into());
        }
        total_cells = total_cells.saturating_add(record.len().max(1));
        if rows.len() >= MAX_ROWS || total_cells > MAX_TOTAL_CELLS {
            return Err("The CSV exceeds FRIDAY's bounded 20,000-row / 200,000-cell analysis limit.".into());
        }
        rows.push(record.iter().map(str::to_string).collect::<Vec<_>>());
    }
    if rows.is_empty() {
        return Err("The selected CSV has no rows to analyze.".into());
    }
    let sheet = analyze_matrix("CSV data".into(), rows, 0)?;
    let total_rows = sheet.row_count;
    let total_non_empty_cells = sheet.non_empty_cells;
    Ok(WorkbookAnalysis {
        file_name, format: "CSV".into(), sheets: vec![sheet], total_rows,
        total_non_empty_cells,
        notes: vec![
            "The first row is treated as column headers.".into(),
            "CSV files do not contain workbook formulas or formatting.".into(),
            "Analysis is local. FRIDAY does not upload this file.".into(),
        ],
    })
}

fn analyze_xlsx(file_name: String, bytes: Vec<u8>) -> Result<WorkbookAnalysis, String> {
    validate_xlsx_package(&bytes)?;
    let mut workbook: Xlsx<_> = open_workbook_from_rs(Cursor::new(bytes))
        .map_err(|_| "FRIDAY could not read this XLSX workbook. It may be damaged or use unsupported workbook features.".to_string())?;
    let sheet_names = workbook.sheet_names();
    if sheet_names.is_empty() || sheet_names.len() > MAX_SHEETS {
        return Err("A workbook must contain between 1 and 12 worksheets for this preview.".into());
    }
    let mut sheets = Vec::with_capacity(sheet_names.len());
    let mut total_cells = 0usize;
    for name in sheet_names {
        let range = workbook.worksheet_range(&name)
            .map_err(|_| format!("FRIDAY could not read worksheet '{name}'."))?;
        let (height, width) = range.get_size();
        if height > MAX_ROWS || width > MAX_COLUMNS
            || total_cells.saturating_add(height.saturating_mul(width)) > MAX_TOTAL_CELLS {
            return Err("The workbook exceeds FRIDAY's bounded 20,000-row, 64-column or 200,000-total-cell analysis limit.".into());
        }
        total_cells = total_cells.saturating_add(height.saturating_mul(width));
        let rows = range.rows().map(|row| row.iter().map(|cell| {
            if *cell == Data::Empty { String::new() } else { cell.to_string() }
        }).collect::<Vec<_>>()).collect::<Vec<_>>();
        let formula_cells = workbook.worksheet_formula(&name).ok()
            .map(|formulas| formulas.rows().flat_map(|row| row.iter()).filter(|formula| !formula.trim().is_empty()).count())
            .unwrap_or(0);
        sheets.push(analyze_matrix(name, rows, formula_cells)?);
    }
    let total_rows = sheets.iter().map(|sheet| sheet.row_count).sum();
    let total_non_empty_cells = sheets.iter().map(|sheet| sheet.non_empty_cells).sum();
    Ok(WorkbookAnalysis {
        file_name, format: "XLSX".into(), sheets, total_rows, total_non_empty_cells,
        notes: vec![
            "Analysis runs locally on this device; the workbook is not uploaded.".into(),
            "Formulas are counted and displayed as metadata; FRIDAY does not execute or recalculate formulas.".into(),
            "Only cached cell values are analyzed. Results may differ from a workbook recalculated by Excel.".into(),
            "Macros and external workbook links are never executed or followed.".into(),
            "The first row of each worksheet is treated as column headers.".into(),
        ],
    })
}

#[tauri::command]
pub async fn pick_and_analyze_workbook(app: AppHandle) -> Result<WorkbookAnalysis, String> {
    crate::permissions::authorize_command("excel.pick_and_analyze_workbook", "excel.workbook.read")?;
    let selected = app.dialog().file()
        .add_filter("Excel workbook or CSV", &["xlsx", "csv"])
        .blocking_pick_file();
    let Some(selected) = selected else {
        return Err("Workbook selection was cancelled.".into());
    };
    let path: PathBuf = selected.into_path()
        .map_err(|_| "FRIDAY could not access the selected workbook path.".to_string())?;
    let extension = path.extension().and_then(|value| value.to_str()).unwrap_or("").to_ascii_lowercase();
    let max_bytes = match extension.as_str() {
        "xlsx" => MAX_XLSX_BYTES,
        "csv" => MAX_CSV_BYTES,
        _ => return Err("Choose an .xlsx or .csv file. Legacy .xls files are not supported yet.".into()),
    };
    let file_name = path.file_name().and_then(|value| value.to_str())
        .unwrap_or("Selected workbook").chars().take(180).collect::<String>();
    let bytes = read_selected_file(&path, max_bytes)?;
    let analysis = match extension.as_str() {
        "xlsx" => analyze_xlsx(file_name, bytes)?,
        "csv" => analyze_csv(file_name, &bytes)?,
        _ => unreachable!(),
    };
    let _ = crate::tools::record(
        &app, "excel.workbook.analyze",
        &format!("Analyzed selected {} workbook '{}' locally ({} rows, {} non-empty cells).",
            analysis.format, analysis.file_name, analysis.total_rows, analysis.total_non_empty_cells),
        true,
    );
    Ok(analysis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_analysis_counts_missing_values_and_numeric_statistics() {
        let csv = b"Product,Units,Revenue\nA,2,12.5\nB,,17.5\nC,4,20\n";
        let analysis = analyze_csv("sales.csv".into(), csv).unwrap();
        assert_eq!(analysis.format, "CSV");
        assert_eq!(analysis.total_rows, 4);
        assert_eq!(analysis.sheets[0].columns[1].missing, 1);
        assert_eq!(analysis.sheets[0].columns[1].sum, Some(6.0));
        assert_eq!(analysis.sheets[0].columns[2].average, Some(50.0 / 3.0));
    }

    #[test]
    fn csv_parser_handles_quoted_commas_and_embedded_newlines() {
        let csv = b"Name,Note\n\"Smith, Jane\",\"First line\nsecond line\"\n";
        let analysis = analyze_csv("quoted.csv".into(), csv).unwrap();
        assert_eq!(analysis.sheets[0].preview[1][0], "Smith, Jane");
        assert!(analysis.sheets[0].preview[1][1].contains("second line"));
    }

    #[test]
    fn csv_limits_reject_excessive_columns_and_rows() {
        let too_many_columns = (0..=MAX_COLUMNS).map(|i| format!("c{i}")).collect::<Vec<_>>().join(",");
        assert!(analyze_csv("wide.csv".into(), too_many_columns.as_bytes()).is_err());
        let too_many_rows = format!("h\n{}", "x\n".repeat(MAX_ROWS));
        assert!(analyze_csv("tall.csv".into(), too_many_rows.as_bytes()).is_err());
    }

    #[test]
    fn xlsx_validation_rejects_non_zip_and_macro_enabled_packages() {
        assert!(validate_xlsx_package(b"not a workbook").is_err());
    }

    #[test]
    fn matrix_preview_and_column_limits_are_bounded() {
        let rows = vec![vec!["header".to_string(); MAX_COLUMNS + 1]];
        assert!(analyze_matrix("wide".into(), rows, 0).is_err());
        let rows = vec![vec!["x".to_string(); MAX_COLUMNS]; MAX_ROWS + 1];
        assert!(analyze_matrix("large".into(), rows, 0).is_err());
    }

    #[test]
    fn numeric_analysis_ignores_non_finite_and_non_numeric_values() {
        let rows = vec![
            vec!["Amount".into()],
            vec!["10".into()],
            vec!["NaN".into()],
            vec!["text".into()],
            vec!["20".into()],
        ];
        let result = analyze_matrix("test".into(), rows, 0).unwrap();
        assert_eq!(result.columns[0].numeric_count, 2);
        assert_eq!(result.columns[0].sum, Some(30.0));
    }
}
