use calamine::{open_workbook_from_rs, Data, Reader, Xlsx};
use csv::ReaderBuilder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    fs::{File, OpenOptions},
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
const MAX_QUALITY_FINDINGS: usize = 40;
const MAX_FINDING_SAMPLE_ROWS: usize = 5;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookAnalysis {
    pub file_name: String,
    pub format: String,
    pub source_sha256: String,
    pub source_byte_length: u64,
    pub sheets: Vec<SheetAnalysis>,
    pub total_rows: usize,
    pub total_non_empty_cells: usize,
    pub notes: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataQualityFinding {
    pub severity: String,
    pub category: String,
    pub title: String,
    pub description: String,
    pub occurrences: usize,
    pub sample_rows: Vec<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountingFieldSuggestion {
    pub header: String,
    pub field_id: Option<String>,
    pub possible_fields: Vec<String>,
    pub status: String,
    pub source_column_index: usize,
    pub header_cell: String,
    pub data_range: String,
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
    pub quality_findings: Vec<DataQualityFinding>,
    pub accounting_field_suggestions: Vec<AccountingFieldSuggestion>,
}

#[derive(Serialize, Deserialize, Clone)]
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
    pub top_values: Vec<ValueFrequency>,
    pub frequency_complete: bool,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ValueFrequency {
    pub value: String,
    pub count: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExcelQuestionAnswer {
    pub status: String,
    pub answer: String,
    pub interpretation: String,
    pub evidence: Vec<String>,
    pub method: String,
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

fn open_selected_file_nofollow(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path).map_err(|_| "FRIDAY could not safely open the selected workbook.".to_string())
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

    let mut file = open_selected_file_nofollow(&canonical)?;
    let before = file.metadata()
        .map_err(|_| "FRIDAY could not verify the selected workbook.".to_string())?;
    if is_redirected(&before) || !before.is_file() || before.len() != canonical_meta.len() || before.len() > max_bytes {
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
        let name = entry.name().map_err(|_| "The workbook contains an invalid internal path.".to_string())?.to_ascii_lowercase();
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


fn push_finding(
    findings: &mut Vec<DataQualityFinding>,
    severity: &str,
    category: &str,
    title: &str,
    description: &str,
    occurrences: usize,
    sample_rows: Vec<usize>,
) {
    if findings.len() >= MAX_QUALITY_FINDINGS { return; }
    findings.push(DataQualityFinding {
        severity: severity.into(),
        category: category.into(),
        title: title.into(),
        description: description.into(),
        occurrences,
        sample_rows,
    });
}

fn infer_cell_type(value: &str) -> &'static str {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized == "true" || normalized == "false" || normalized == "yes" || normalized == "no" {
        return "boolean";
    }
    if value.replace(',', "").parse::<f64>().map(|number| number.is_finite()).unwrap_or(false) {
        return "number";
    }
    let parts = value.split(|character: char| character == '-' || character == '/' || character == '.')
        .collect::<Vec<_>>();
    if parts.len() == 3 && parts.iter().all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit())) {
        let year_first = parts[0].len() == 4 && parts[1].len() <= 2 && parts[2].len() <= 2;
        let year_last = parts[2].len() == 4 && parts[0].len() <= 2 && parts[1].len() <= 2;
        if year_first || year_last { return "date-like"; }
    }
    "text"
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() { return 0.0; }
    let position = (sorted.len() - 1) as f64 * fraction.clamp(0.0, 1.0);
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    if lower == upper { sorted[lower] } else {
        sorted[lower] + (sorted[upper] - sorted[lower]) * (position - lower as f64)
    }
}

fn excel_column_label(index: usize) -> String {
    let mut value = index + 1;
    let mut label = String::new();
    while value > 0 {
        let remainder = (value - 1) % 26;
        label.insert(0, (b'A' + remainder as u8) as char);
        value = (value - 1) / 26;
    }
    label
}

fn accounting_field_suggestions(header: &[String], row_count: usize, column_count: usize) -> Vec<AccountingFieldSuggestion> {
    let mut suggestions = Vec::new();
    for column in 0..column_count {
        let name = header.get(column).map(|value| value.trim()).unwrap_or("");
        let normalized = normalized_words(name).join(" ");
        let candidates: Vec<&str> = match normalized.as_str() {
            "gross sales" | "gross revenue" | "total sales" => vec!["accounting.gross_sales"],
            "discounts" | "sales discounts" | "discount allowed" => vec!["accounting.discounts"],
            "sales returns" | "returns and allowances" | "sales refunds" => vec!["accounting.sales_returns"],
            "cost of goods sold" | "cogs" | "cost of sales" => vec!["accounting.cost_of_goods_sold"],
            "net sales" | "net revenue" => vec!["accounting.net_sales"],
            "revenue" | "sales" | "total revenue" => vec!["accounting.gross_sales", "accounting.net_sales"],
            _ => Vec::new(),
        };
        let letter = excel_column_label(column);
        let field_id = if candidates.len() == 1 { Some(candidates[0].to_string()) } else { None };
        let status = if candidates.len() > 1 { "ambiguous" } else if candidates.len() == 1 { "suggested" } else { "unmapped" };
        suggestions.push(AccountingFieldSuggestion {
            header: if name.is_empty() { format!("Column {}", column + 1) } else { name.chars().take(180).collect() },
            field_id,
            possible_fields: candidates.iter().map(|candidate| candidate.to_string()).collect(),
            status: status.into(),
            source_column_index: column + 1,
            header_cell: format!("{letter}1"),
            data_range: if row_count > 1 { format!("{letter}2:{letter}{row_count}") } else { format!("{letter}2 (no data rows)") },
        });
    }
    let mut field_counts: HashMap<String, usize> = HashMap::new();
    for suggestion in &suggestions {
        if let Some(field) = &suggestion.field_id {
            *field_counts.entry(field.clone()).or_default() += 1;
        }
    }
    for suggestion in &mut suggestions {
        if suggestion.field_id.as_ref().is_some_and(|field| field_counts.get(field).copied().unwrap_or(0) > 1) {
            suggestion.status = "ambiguous".into();
            suggestion.possible_fields = suggestion.field_id.take().into_iter().collect();
        }
    }
    suggestions
}

fn analyze_matrix(
    name: String,
    rows: Vec<Vec<String>>,
    formula_cells: usize,
) -> Result<SheetAnalysis, String> {
    if rows.is_empty() {
        return Ok(SheetAnalysis {
            name, row_count: 0, column_count: 0, non_empty_cells: 0,
            blank_cells: 0, formula_cells, preview: Vec::new(), columns: Vec::new(), quality_findings: Vec::new(),
            accounting_field_suggestions: Vec::new(),
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
    let mut quality_findings = Vec::new();

    let mut blank_headers = Vec::new();
    let mut header_groups: HashMap<String, Vec<usize>> = HashMap::new();
    for column in 0..column_count {
        let raw = header.get(column).map(String::as_str).unwrap_or("").trim();
        if raw.is_empty() {
            blank_headers.push(column + 1);
        } else {
            header_groups.entry(raw.to_lowercase()).or_default().push(column + 1);
        }
    }
    if !blank_headers.is_empty() {
        push_finding(&mut quality_findings, "warning", "blank_header",
            "Blank column headers",
            &format!("{} column(s) have no header. FRIDAY labels them by position in the preview.", blank_headers.len()),
            blank_headers.len(), blank_headers.iter().take(MAX_FINDING_SAMPLE_ROWS).copied().collect());
    }
    for (name, positions) in header_groups.iter().filter(|(_, positions)| positions.len() > 1) {
        push_finding(&mut quality_findings, "warning", "duplicate_header",
            "Duplicate column headers",
            &format!("The header '{}' appears in columns {}. This can make later references ambiguous.", name, positions.iter().map(usize::to_string).collect::<Vec<_>>().join(", ")),
            positions.len() - 1, positions.iter().take(MAX_FINDING_SAMPLE_ROWS).copied().collect());
    }

    let mut columns = Vec::with_capacity(column_count);
    for column in 0..column_count {
        let header_name = header.get(column).map(|v| v.trim()).filter(|v| !v.is_empty())
            .map(str::to_string).unwrap_or_else(|| format!("Column {}", column + 1));
        let mut non_empty = 0usize;
        let mut numeric_count = 0usize;
        let mut sum = 0.0_f64;
        let mut minimum: Option<f64> = None;
        let mut maximum: Option<f64> = None;
        let mut observed_types: HashSet<&'static str> = HashSet::new();
        let mut numeric_values: Vec<(usize, f64)> = Vec::new();
        let mut frequencies: HashMap<String, usize> = HashMap::new();
        let mut frequency_complete = true;
        for (row_index, row) in rows.iter().enumerate().skip(1) {
            let value = row.get(column).map(String::as_str).unwrap_or("").trim();
            if value.is_empty() { continue; }
            non_empty += 1;
            observed_types.insert(infer_cell_type(value));
            if frequency_complete {
                if let Some(count) = frequencies.get_mut(value) {
                    *count += 1;
                } else if frequencies.len() < 512 {
                    frequencies.insert(value.to_string(), 1);
                } else {
                    frequency_complete = false;
                    frequencies.clear();
                }
            }
            if let Ok(number) = value.replace(',', "").parse::<f64>() {
                if number.is_finite() {
                    numeric_count += 1;
                    sum += number;
                    minimum = Some(minimum.map_or(number, |current| current.min(number)));
                    maximum = Some(maximum.map_or(number, |current| current.max(number)));
                    numeric_values.push((row_index + 1, number));
                }
            }
        }
        let missing = rows.len().saturating_sub(1).saturating_sub(non_empty);
        if observed_types.len() > 1 {
            push_finding(&mut quality_findings, "warning", "mixed_types",
                "Mixed data types",
                &format!("Column '{}' contains more than one data type. Check whether values are consistently formatted.", header_name),
                observed_types.len(), Vec::new());
        }
        if numeric_values.len() >= 4 {
            let mut sorted = numeric_values.iter().map(|(_, value)| *value).collect::<Vec<_>>();
            sorted.sort_by(f64::total_cmp);
            let q1 = percentile(&sorted, 0.25);
            let q3 = percentile(&sorted, 0.75);
            let iqr = q3 - q1;
            if iqr.is_finite() && iqr > 0.0 {
                let lower = q1 - 1.5 * iqr;
                let upper = q3 + 1.5 * iqr;
                let outliers = numeric_values.iter().filter(|(_, value)| *value < lower || *value > upper).collect::<Vec<_>>();
                if !outliers.is_empty() {
                    push_finding(&mut quality_findings, "info", "numeric_outlier",
                        "Potential numeric outliers",
                        &format!("{} value(s) in '{}' fall outside the 1.5× IQR range. These are review suggestions, not confirmed errors.", outliers.len(), header_name),
                        outliers.len(), outliers.iter().take(MAX_FINDING_SAMPLE_ROWS).map(|(row, _)| *row).collect());
                }
            }
        }
        let mut top_values = if frequency_complete {
            frequencies.into_iter().map(|(value, count)| ValueFrequency {
                value: value.chars().take(100).collect(), count
            }).collect::<Vec<_>>()
        } else { Vec::new() };
        top_values.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
        top_values.truncate(5);
        columns.push(ColumnAnalysis {
            name: header_name, non_empty, missing, numeric_count,
            sum: (numeric_count > 0 && sum.is_finite()).then_some(sum),
            average: (numeric_count > 0 && sum.is_finite()).then_some(sum / numeric_count as f64),
            minimum, maximum, top_values, frequency_complete,
        });
    }

    let mut row_occurrences: HashMap<String, (usize, Vec<usize>)> = HashMap::new();
    for (row_index, row) in rows.iter().enumerate().skip(1) {
        if row.iter().all(|value| value.trim().is_empty()) { continue; }
        let key = row.iter().map(|value| {
            let value = value.trim();
            format!("{}:{}", value.len(), value)
        }).collect::<String>();
        let entry = row_occurrences.entry(key).or_insert((0, Vec::new()));
        entry.0 += 1;
        if entry.1.len() < MAX_FINDING_SAMPLE_ROWS {
            entry.1.push(row_index + 1);
        }
    }
    let duplicate_rows = row_occurrences.values().map(|(count, _)| count.saturating_sub(1)).sum::<usize>();
    if duplicate_rows > 0 {
        let sample_rows = row_occurrences.values()
            .filter(|(count, _)| *count > 1)
            .flat_map(|(_, rows)| rows.iter().skip(1).copied())
            .take(MAX_FINDING_SAMPLE_ROWS)
            .collect::<Vec<_>>();
        push_finding(&mut quality_findings, "info", "duplicate_rows",
            "Duplicate rows",
            "Some non-empty data rows repeat exactly after trimming surrounding whitespace. Review them before removing anything.",
            duplicate_rows, sample_rows);
    }

    let preview = rows.iter().take(PREVIEW_ROWS).map(|row| {
        row.iter().take(PREVIEW_COLUMNS).map(|value| cell_text(value)).collect()
    }).collect();
    let accounting_field_suggestions = accounting_field_suggestions(&header, rows.len(), column_count);
    Ok(SheetAnalysis {
        name, row_count: rows.len(), column_count, non_empty_cells, blank_cells,
        formula_cells, preview, columns, quality_findings, accounting_field_suggestions,
    })
}

fn analyze_csv(file_name: String, bytes: &[u8]) -> Result<WorkbookAnalysis, String> {
    let source_sha256 = format!("{:x}", Sha256::digest(bytes));
    let source_byte_length = bytes.len() as u64;
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
        file_name, format: "CSV".into(), source_sha256, source_byte_length, sheets: vec![sheet], total_rows,
        total_non_empty_cells,
        notes: vec![
            "The first row is treated as column headers.".into(),
            "CSV files do not contain workbook formulas or formatting.".into(),
            "Analysis is local. FRIDAY does not upload this file.".into(),
        ],
    })
}

fn analyze_xlsx(file_name: String, bytes: Vec<u8>) -> Result<WorkbookAnalysis, String> {
    let source_sha256 = format!("{:x}", Sha256::digest(&bytes));
    let source_byte_length = bytes.len() as u64;
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
        file_name, format: "XLSX".into(), source_sha256, source_byte_length, sheets, total_rows, total_non_empty_cells,
        notes: vec![
            "Analysis runs locally on this device; the workbook is not uploaded.".into(),
            "Formulas are counted and displayed as metadata; FRIDAY does not execute or recalculate formulas.".into(),
            "Only cached cell values are analyzed. Results may differ from a workbook recalculated by Excel.".into(),
            "Macros and external workbook links are never executed or followed.".into(),
            "The first row of each worksheet is treated as column headers.".into(),
        ],
    })
}


fn normalized_words(value: &str) -> Vec<String> {
    value.to_lowercase().split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty()).map(str::to_string).collect()
}

fn select_question_column<'a>(
    question: &str,
    columns: &'a [ColumnAnalysis],
    numeric_only: bool,
) -> Result<&'a ColumnAnalysis, ExcelQuestionAnswer> {
    let question_words = normalized_words(question);
    let question_joined = question_words.join(" ");
    let mut ranked = columns.iter().filter(|column| !numeric_only || column.numeric_count > 0)
        .map(|column| {
            let words = normalized_words(&column.name);
            let phrase = words.join(" ");
            let score = if !phrase.is_empty() && question_joined.contains(&phrase) { 100 }
                else { words.iter().filter(|word| question_words.contains(word)).count() as i32 * 10 };
            (column, score)
        }).collect::<Vec<_>>();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.name.cmp(&b.0.name)));
    if ranked.is_empty() {
        return Err(ExcelQuestionAnswer { status: "unsupported".into(),
            answer: "I can't answer that from the available column summaries.".into(),
            interpretation: "No suitable column is available for this question.".into(),
            evidence: Vec::new(), method: "bounded local summary".into() });
    }
    if ranked[0].1 == 0 && ranked.len() == 1 { return Ok(ranked[0].0); }
    if ranked[0].1 == 0 || (ranked.len() > 1 && ranked[0].1 == ranked[1].1) {
        let choices = ranked.iter().take(5).map(|(column, _)| column.name.as_str()).collect::<Vec<_>>().join(", ");
        return Err(ExcelQuestionAnswer { status: "clarification".into(),
            answer: format!("Which column should I use? Possible columns: {choices}."),
            interpretation: "The question doesn't identify one unambiguous column.".into(),
            evidence: Vec::new(), method: "column-name matching".into() });
    }
    Ok(ranked[0].0)
}

fn answer_question(
    question: &str,
    sheet_name: &str,
    row_count: usize,
    columns: &[ColumnAnalysis],
) -> ExcelQuestionAnswer {
    let q = question.to_lowercase();
    let words = normalized_words(&q);
    let has = |terms: &[&str]| terms.iter().any(|term| words.iter().any(|word| word == term));
    let grouped = has(&["by", "per", "group", "grouped"]) || q.contains("sold most") || q.contains("by product");
    if grouped {
        return ExcelQuestionAnswer { status: "unsupported".into(),
            answer: "Grouped comparisons aren't supported in this first analysis release yet.".into(),
            interpretation: "This question appears to require grouping rows by one column and aggregating another.".into(),
            evidence: Vec::new(), method: "not yet supported".into() };
    }
    if question.trim().is_empty() || question.chars().count() > 300 {
        return ExcelQuestionAnswer { status: "unsupported".into(),
            answer: "Enter a question of up to 300 characters.".into(),
            interpretation: "Question length is outside the supported range.".into(),
            evidence: Vec::new(), method: "input validation".into() };
    }
    if has(&["rows", "records", "entries"]) && has(&["many", "count", "number", "total"]) {
        let data_rows = row_count.saturating_sub(1);
        return ExcelQuestionAnswer { status: "answered".into(),
            answer: format!("This sheet contains {data_rows} data rows (excluding the header)."),
            interpretation: "Count data rows in the selected worksheet.".into(),
            evidence: vec![format!("Worksheet: {sheet_name}"), format!("Rows including header: {row_count}")],
            method: "exact worksheet dimensions".into() };
    }
    let missing = has(&["missing", "blank", "empty", "null"]);
    let sum = has(&["sum", "total", "combined"]);
    let average = has(&["average", "avg", "mean"]);
    let maximum = has(&["highest", "maximum", "max", "largest", "biggest"]);
    let minimum = has(&["lowest", "minimum", "min", "smallest"]);
    let frequent = q.contains("most common") || q.contains("most frequent") || q.contains("mode") || q.contains("appears most");
    let count = !missing && has(&["count", "many", "number"]);
    let intents = [missing, sum, average, maximum, minimum, frequent, count].iter().filter(|v| **v).count();
    if intents != 1 {
        return ExcelQuestionAnswer { status: "unsupported".into(),
            answer: "I couldn't identify one supported calculation. Try asking for a total, average, highest or lowest value, missing values, most common value, or row count.".into(),
            interpretation: "The question is ambiguous or outside the current deterministic analysis set.".into(),
            evidence: Vec::new(), method: "intent classification".into() };
    }
    let numeric_only = sum || average || maximum || minimum;
    let column = match select_question_column(question, columns, numeric_only) {
        Ok(column) => column,
        Err(answer) => return answer,
    };
    let label = &column.name;
    let (answer, interpretation, evidence) = if missing {
        (format!("Column '{label}' has {} missing value(s) across {} data rows.", column.missing, row_count.saturating_sub(1)),
         format!("Count blank cells in '{label}'."), vec![format!("Column: {label}"), format!("Missing: {}", column.missing), format!("Filled: {}", column.non_empty)])
    } else if sum {
        match column.sum { Some(value) => (format!("The total for '{label}' is {value:.2}."),
            format!("Sum numeric values in '{label}'."), vec![format!("Column: {label}"), format!("Numeric values: {}", column.numeric_count)]),
            None => return ExcelQuestionAnswer { status: "unsupported".into(), answer: format!("Column '{label}' has no usable numeric values."),
                interpretation: "A total requires numeric values.".into(), evidence: vec![format!("Column: {label}")], method: "local numeric summary".into() } }
    } else if average {
        match column.average { Some(value) => (format!("The average for '{label}' is {value:.2}."),
            format!("Average numeric values in '{label}'."), vec![format!("Column: {label}"), format!("Numeric values: {}", column.numeric_count)]),
            None => return ExcelQuestionAnswer { status: "unsupported".into(), answer: format!("Column '{label}' has no usable numeric values."),
                interpretation: "An average requires numeric values.".into(), evidence: vec![format!("Column: {label}")], method: "local numeric summary".into() } }
    } else if maximum {
        match column.maximum { Some(value) => (format!("The highest value in '{label}' is {value:.2}."),
            format!("Find the maximum numeric value in '{label}'."), vec![format!("Column: {label}"), format!("Numeric values: {}", column.numeric_count)]),
            None => return ExcelQuestionAnswer { status: "unsupported".into(), answer: format!("Column '{label}' has no usable numeric values."),
                interpretation: "A maximum requires numeric values.".into(), evidence: vec![format!("Column: {label}")], method: "local numeric summary".into() } }
    } else if minimum {
        match column.minimum { Some(value) => (format!("The lowest value in '{label}' is {value:.2}."),
            format!("Find the minimum numeric value in '{label}'."), vec![format!("Column: {label}"), format!("Numeric values: {}", column.numeric_count)]),
            None => return ExcelQuestionAnswer { status: "unsupported".into(), answer: format!("Column '{label}' has no usable numeric values."),
                interpretation: "A minimum requires numeric values.".into(), evidence: vec![format!("Column: {label}")], method: "local numeric summary".into() } }
    } else if frequent {
        if !column.frequency_complete || column.top_values.is_empty() {
            return ExcelQuestionAnswer { status: "unsupported".into(), answer: format!("I can't safely determine the most common value in '{label}' because its distinct-value summary exceeded the local limit."),
                interpretation: "Frequency tracking is deliberately bounded.".into(), evidence: vec![format!("Column: {label}")], method: "bounded local frequency summary".into() };
        }
        let top = &column.top_values[0];
        (format!("The most common value in '{label}' is '{}' ({} occurrence(s)).", top.value, top.count),
         format!("Find the highest-frequency value in '{label}'."), vec![format!("Column: {label}"), format!("Value: {}", top.value), format!("Occurrences: {}", top.count)])
    } else {
        (format!("Column '{label}' contains {} non-empty value(s).", column.non_empty),
         format!("Count non-empty cells in '{label}'."), vec![format!("Column: {label}"), format!("Missing: {}", column.missing)])
    };
    ExcelQuestionAnswer { status: "answered".into(), answer, interpretation, evidence, method: "deterministic local summary; no AI or network request".into() }
}

#[tauri::command]
pub fn answer_excel_question(
    question: String,
    sheet_name: String,
    row_count: usize,
    columns: Vec<ColumnAnalysis>,
) -> Result<ExcelQuestionAnswer, String> {
    crate::permissions::authorize_command("excel.answer_question", "excel.analysis.local")?;
    if sheet_name.chars().count() > 180 || row_count > MAX_ROWS || columns.len() > MAX_COLUMNS {
        return Err("The worksheet summary exceeds FRIDAY's safe question-answering limits.".into());
    }
    let data_rows = row_count.saturating_sub(1);
    if columns.iter().any(|column| {
        column.name.chars().count() > 180
            || column.non_empty.saturating_add(column.missing) > data_rows
            || column.numeric_count > column.non_empty
            || column.top_values.len() > 5
            || column.top_values.iter().any(|value| value.value.chars().count() > 100
                || value.count == 0 || value.count > column.non_empty)
            || (!column.frequency_complete && !column.top_values.is_empty())
            || [column.sum, column.average, column.minimum, column.maximum].iter()
                .flatten().any(|value| !value.is_finite())
    }) {
        return Err("The supplied worksheet summary is invalid or exceeds its bounds.".into());
    }
    if question.chars().count() > 300 {
        return Err("Questions must be 300 characters or fewer.".into());
    }
    Ok(answer_question(&question, &sheet_name, row_count, &columns))
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
        &format!("Analyzed a user-selected {} workbook locally ({} rows, {} non-empty cells).",
            analysis.format, analysis.total_rows, analysis.total_non_empty_cells),
        true,
    );
    Ok(analysis)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn test_xlsx(include_macro: bool) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let mut entries = vec![
            ("[Content_Types].xml", r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#),
            ("_rels/.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#),
            ("xl/workbook.xml", r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sales" sheetId="1" r:id="rId1"/></sheets></workbook>"#),
            ("xl/_rels/workbook.xml.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#),
            ("xl/worksheets/sheet1.xml", r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Product</t></is></c><c r="B1" t="inlineStr"><is><t>Units</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>Book</t></is></c><c r="B2"><f>1+2</f><v>3</v></c></row></sheetData></worksheet>"#),
        ];
        if include_macro {
            entries.push(("xl/vbaProject.bin", "macro-bytes"));
        }
        for (name, body) in entries {
            writer.start_file(name, options).unwrap();
            writer.write_all(body.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn csv_analysis_counts_missing_values_and_numeric_statistics() {
        let csv = b"Product,Units,Revenue\nA,2,12.5\nB,,17.5\nC,4,20\n";
        let analysis = analyze_csv("sales.csv".into(), csv).unwrap();
        assert_eq!(analysis.format, "CSV");
        assert_eq!(analysis.source_sha256, format!("{:x}", Sha256::digest(csv)));
        assert_eq!(analysis.source_byte_length, csv.len() as u64);
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
        let macro_workbook = test_xlsx(true);
        assert!(validate_xlsx_package(&macro_workbook).is_err());
    }

    #[test]
    fn xlsx_import_reads_sheets_cached_values_and_formula_counts_without_execution() {
        let bytes = test_xlsx(false);
        let analysis = analyze_xlsx("sales.xlsx".into(), bytes.clone()).unwrap();
        assert_eq!(analysis.format, "XLSX");
        assert_eq!(analysis.source_sha256, format!("{:x}", Sha256::digest(&bytes)));
        assert_eq!(analysis.source_byte_length, bytes.len() as u64);
        assert_eq!(analysis.sheets.len(), 1);
        assert_eq!(analysis.sheets[0].name, "Sales");
        assert_eq!(analysis.sheets[0].formula_cells, 1);
        assert_eq!(analysis.sheets[0].columns[1].sum, Some(3.0));
        assert!(analysis.notes.iter().any(|note| note.contains("does not execute")));
    }

    #[test]
    fn quality_checks_find_blank_duplicate_headers_mixed_types_and_duplicate_rows() {
        let rows = vec![
            vec!["Name".into(), "Name".into(), "".into(), "Amount".into()],
            vec!["A".into(), "A".into(), "x".into(), "10".into()],
            vec!["B".into(), "B".into(), "y".into(), "text".into()],
            vec!["A".into(), "A".into(), "x".into(), "10".into()],
        ];
        let result = analyze_matrix("quality".into(), rows, 0).unwrap();
        let categories = result.quality_findings.iter().map(|finding| finding.category.as_str()).collect::<HashSet<_>>();
        assert!(categories.contains("blank_header"));
        assert!(categories.contains("duplicate_header"));
        assert!(categories.contains("mixed_types"));
        assert!(categories.contains("duplicate_rows"));
        assert_eq!(result.quality_findings.iter().find(|finding| finding.category == "duplicate_rows").unwrap().occurrences, 1);
    }

    #[test]
    fn outlier_detection_is_suggestive_and_reports_bounded_row_samples() {
        let rows = vec![
            vec!["Amount".into()],
            vec!["10".into()], vec!["11".into()], vec!["10".into()],
            vec!["12".into()], vec!["11".into()], vec!["1000".into()],
        ];
        let result = analyze_matrix("outliers".into(), rows, 0).unwrap();
        let finding = result.quality_findings.iter().find(|finding| finding.category == "numeric_outlier").unwrap();
        assert_eq!(finding.occurrences, 1);
        assert_eq!(finding.sample_rows, vec![7]);
        assert!(finding.description.contains("not confirmed errors"));
    }

    #[test]
    fn quality_finding_output_is_bounded() {
        let rows = (0..=MAX_COLUMNS).take(MAX_COLUMNS).map(|index| vec![format!("H{index}"), "same".into()]).collect::<Vec<_>>();
        let mut expanded = rows;
        for index in 0..MAX_ROWS.min(100) {
            expanded.push(vec![format!("row{index}"), "mixed".into()]);
        }
        let result = analyze_matrix("bounded".into(), expanded, 0).unwrap();
        assert!(result.quality_findings.len() <= MAX_QUALITY_FINDINGS);
    }


    #[test]
    fn local_question_answering_supports_aggregates_and_frequency() {
        let rows = vec![
            vec!["Product".into(), "Revenue".into()],
            vec!["Tea".into(), "10".into()],
            vec!["Coffee".into(), "20".into()],
            vec!["Tea".into(), "30".into()],
        ];
        let sheet = analyze_matrix("Sales".into(), rows, 0).unwrap();
        let total = answer_question("What is the total revenue?", "Sales", sheet.row_count, &sheet.columns);
        assert_eq!(total.status, "answered");
        assert!(total.answer.contains("60.00"));
        let average = answer_question("average revenue", "Sales", sheet.row_count, &sheet.columns);
        assert!(average.answer.contains("20.00"));
        let common = answer_question("most common product", "Sales", sheet.row_count, &sheet.columns);
        assert!(common.answer.contains("Tea"));
        let missing = answer_question("how many missing values in Revenue", "Sales", sheet.row_count, &sheet.columns);
        assert!(missing.answer.contains("0 missing"));
    }


    #[test]
    fn frequency_summary_stops_at_the_distinct_value_limit() {
        let mut rows = vec![vec!["Category".into()]];
        for index in 0..513 { rows.push(vec![format!("value-{index}")]); }
        let sheet = analyze_matrix("high-cardinality".into(), rows, 0).unwrap();
        assert!(!sheet.columns[0].frequency_complete);
        assert!(sheet.columns[0].top_values.is_empty());
        let answer = answer_question("most common category", "high-cardinality", sheet.row_count, &sheet.columns);
        assert_eq!(answer.status, "unsupported");
    }

    #[test]
    fn local_question_answering_clarifies_ambiguous_and_rejects_grouped_queries() {
        let rows = vec![
            vec!["Sales".into(), "Cost".into()],
            vec!["10".into(), "5".into()],
        ];
        let sheet = analyze_matrix("Finance".into(), rows, 0).unwrap();
        let ambiguous = answer_question("what is the total?", "Finance", sheet.row_count, &sheet.columns);
        assert_eq!(ambiguous.status, "clarification");
        let grouped = answer_question("which product sold most by region?", "Finance", sheet.row_count, &sheet.columns);
        assert_eq!(grouped.status, "unsupported");
    }

    #[test]
    fn accounting_field_mapping_is_explicit_ambiguous_and_source_located() {
        let rows = vec![
            vec!["Gross Sales".into(), "Revenue".into(), "COGS".into(), "Gross Sales".into()],
            vec!["100".into(), "100".into(), "60".into(), "90".into()],
        ];
        let sheet = analyze_matrix("Accounts".into(), rows, 0).unwrap();
        let mappings = &sheet.accounting_field_suggestions;
        assert!(mappings[0].field_id.is_none());
        assert_eq!(mappings[0].possible_fields, vec!["accounting.gross_sales"]);
        assert_eq!(mappings[0].status, "ambiguous");
        assert_eq!(mappings[0].header_cell, "A1");
        assert_eq!(mappings[0].data_range, "A2:A2");
        assert_eq!(mappings[1].status, "ambiguous");
        assert_eq!(mappings[1].possible_fields.len(), 2);
        assert_eq!(mappings[2].field_id.as_deref(), Some("accounting.cost_of_goods_sold"));
        assert_eq!(mappings[2].header_cell, "C1");
        assert_eq!(mappings[3].status, "ambiguous");
    }

    #[test]
    fn accounting_field_mapping_does_not_guess_generic_headers() {
        let rows = vec![vec!["Amount".into()], vec!["100".into()]];
        let sheet = analyze_matrix("Unknown".into(), rows, 0).unwrap();
        assert_eq!(sheet.accounting_field_suggestions[0].status, "unmapped");
        assert!(sheet.accounting_field_suggestions[0].field_id.is_none());
    }

    #[test]
    fn matrix_preview_and_column_limits_are_bounded() {
        let rows = vec![vec!["header".to_string(); MAX_COLUMNS + 1]];
        assert!(analyze_matrix("wide".into(), rows, 0).is_err());
        let rows = vec![vec!["x".to_string()]; MAX_ROWS + 1];
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
