use calamine::{open_workbook_from_rs, Data, Reader, Xlsx};
use csv::ReaderBuilder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use rand::{rngs::OsRng, RngCore};
use std::{
    collections::{HashMap, HashSet},
    fs,
    fs::{File, OpenOptions},
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{AppHandle, State};
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
const MAX_WORKBOOK_SESSIONS: usize = 8;
const WORKBOOK_SESSION_TTL: Duration = Duration::from_secs(30 * 60);
const MAX_SESSION_SUMMARY_BYTES: usize = 32 * 1024 * 1024;
const MAX_SESSION_SOURCE_BYTES: usize = 40 * 1024 * 1024;
const MAX_ACCOUNTING_SOURCE_REFERENCES: usize = 20_000;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookAnalysis {
    pub session_id: String,
    pub file_name: String,
    pub format: String,
    pub source_sha256: String,
    pub source_byte_length: u64,
    pub sheets: Vec<SheetAnalysis>,
    pub total_rows: usize,
    pub total_non_empty_cells: usize,
    pub notes: Vec<String>,
    #[serde(skip)]
    source_sheets: Vec<WorkbookSourceSheet>,
}

#[derive(Clone, Serialize)]
struct WorkbookSourceSheet {
    name: String,
    rows: Vec<Vec<String>>,
    start_row: usize,
    start_column: usize,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DataQualityFinding {
    pub severity: String,
    pub category: String,
    pub title: String,
    pub description: String,
    pub occurrences: usize,
    pub sample_rows: Vec<usize>,
}

#[derive(Serialize, Clone)]
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

#[derive(Serialize, Clone)]
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

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmedAccountingMapping {
    pub field_id: String,
    pub sheet_name: String,
    pub header: String,
    pub source_column_index: usize,
    pub header_cell: String,
    pub data_range: String,
    pub source_sha256: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AccountingSourceCell {
    pub field_id: String,
    pub cell: String,
    pub value: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookAccountingCalculation {
    pub source_sha256: String,
    pub file_name: String,
    pub sheet_name: String,
    pub formula: crate::domain_formula::CalculationResult,
    pub confirmed_mappings: Vec<ConfirmedAccountingMapping>,
    pub source_cells: Vec<AccountingSourceCell>,
    pub source_rows_included: usize,
    pub selected_start_row: usize,
    pub selected_end_row: usize,
    pub blank_cells_skipped: usize,
}

struct WorkbookSession {
    created_at: Instant,
    source_sha256: String,
    source_byte_length: u64,
    format: String,
    file_name: String,
    sheets: Vec<SheetAnalysis>,
    confirmed_mappings: HashMap<String, ConfirmedAccountingMapping>,
    source_sheets: Vec<WorkbookSourceSheet>,
    summary_bytes: usize,
    source_bytes: usize,
}

pub struct WorkbookSessionStore {
    sessions: Mutex<HashMap<String, WorkbookSession>>,
}

impl Default for WorkbookSessionStore {
    fn default() -> Self { Self { sessions: Mutex::new(HashMap::new()) } }
}


fn is_accounting_total_candidate(row: &[String]) -> bool {
    row.iter().take(4).filter(|value| !value.trim().is_empty()).any(|value| {
        let normalized = value.trim().to_ascii_lowercase()
            .chars().map(|ch| if ch.is_ascii_alphanumeric() { ch } else { ' ' })
            .collect::<String>();
        let label = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
        label == "total" || label == "subtotal" || label == "grand total"
            || label == "net total" || label == "total sales" || label == "total revenue"
            || label.starts_with("subtotal ") || label.starts_with("grand total ")
            || label.ends_with(" subtotal") || label.ends_with(" grand total")
    })
}

impl WorkbookSessionStore {
    fn create(&self, analysis: &WorkbookAnalysis) -> Result<String, String> {
        let summary_bytes = serde_json::to_vec(&analysis.sheets)
            .map_err(|_| "FRIDAY could not prepare the private workbook session.".to_string())?.len();
        let source_bytes = serde_json::to_vec(&analysis.source_sheets)
            .map_err(|_| "FRIDAY could not prepare the private workbook source session.".to_string())?.len();
        if summary_bytes > MAX_SESSION_SUMMARY_BYTES || source_bytes > MAX_SESSION_SOURCE_BYTES {
            return Err("The workbook analysis or retained source data exceeds FRIDAY's safe session memory limit.".into());
        }
        let mut sessions = self.sessions.lock()
            .map_err(|_| "The workbook session store is unavailable. Import the workbook again.".to_string())?;
        sessions.retain(|_, item| item.created_at.elapsed() < WORKBOOK_SESSION_TTL);
        while sessions.len() >= MAX_WORKBOOK_SESSIONS
            || sessions.values().map(|item| item.summary_bytes).sum::<usize>()
                .saturating_add(summary_bytes) > MAX_SESSION_SUMMARY_BYTES
            || sessions.values().map(|item| item.source_bytes).sum::<usize>()
                .saturating_add(source_bytes) > MAX_SESSION_SOURCE_BYTES {
            let oldest = sessions.iter().min_by_key(|(_, item)| item.created_at).map(|(id, _)| id.clone());
            if let Some(id) = oldest { sessions.remove(&id); } else { break; }
        }
        let mut bytes = [0_u8; 16];
        OsRng.fill_bytes(&mut bytes);
        let id = bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        sessions.insert(id.clone(), WorkbookSession {
            created_at: Instant::now(),
            source_sha256: analysis.source_sha256.clone(),
            source_byte_length: analysis.source_byte_length,
            format: analysis.format.clone(),
            file_name: analysis.file_name.clone(),
            sheets: analysis.sheets.clone(),
            confirmed_mappings: HashMap::new(),
            source_sheets: analysis.source_sheets.clone(),
            summary_bytes,
            source_bytes,
        });
        Ok(id)
    }

    fn answer(&self, id: &str, sheet_name: &str, question: &str) -> Result<ExcelQuestionAnswer, String> {
        if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("This workbook analysis session is invalid. Import the workbook again.".into());
        }
        if sheet_name.chars().count() > 180 || question.chars().count() > 300 {
            return Err("The worksheet name or question exceeds FRIDAY's safe limits.".into());
        }
        let mut sessions = self.sessions.lock()
            .map_err(|_| "The workbook session store is unavailable. Import the workbook again.".to_string())?;
        sessions.retain(|_, item| item.created_at.elapsed() < WORKBOOK_SESSION_TTL);
        let session = sessions.get(id)
            .ok_or_else(|| "This workbook analysis session has expired or was replaced. Import the workbook again.".to_string())?;
        let sheet = session.sheets.iter().find(|sheet| sheet.name == sheet_name)
            .ok_or_else(|| "That worksheet does not belong to this workbook session.".to_string())?;
        let mut answer = answer_question(question, &sheet.name, sheet.row_count, &sheet.columns);
        answer.evidence.push(format!("Workbook: {} ({})", session.file_name, session.format));
        answer.evidence.push(format!("Source SHA-256: {}… ({} bytes)", &session.source_sha256[..12], session.source_byte_length));
        answer.method.push_str("; bound to the imported workbook session");
        Ok(answer)
    }

    fn calculate_accounting_formula(
        &self,
        id: &str,
        sheet_name: &str,
        formula_id: &str,
        currency: &str,
        start_row: usize,
        end_row: usize,
    ) -> Result<WorkbookAccountingCalculation, String> {
        if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("This workbook analysis session is invalid. Import the workbook again.".into());
        }
        if sheet_name.chars().count() > 180 || formula_id.len() > 100 {
            return Err("The worksheet or formula selector exceeds FRIDAY's safe limits.".into());
        }
        let mut sessions = self.sessions.lock()
            .map_err(|_| "The workbook session store is unavailable. Import the workbook again.".to_string())?;
        sessions.retain(|_, item| item.created_at.elapsed() < WORKBOOK_SESSION_TTL);
        let session = sessions.get(id)
            .ok_or_else(|| "This workbook analysis session has expired or was replaced. Import the workbook again.".to_string())?;
        let sheet = session.sheets.iter().find(|sheet| sheet.name == sheet_name)
            .ok_or_else(|| "That worksheet does not belong to this workbook session.".to_string())?;
        let source = session.source_sheets.iter().find(|source| source.name == sheet_name)
            .ok_or_else(|| "The original source cells are unavailable in this workbook session. Import the workbook again.".to_string())?;
        let first_data_row = source.start_row.saturating_add(2);
        let last_data_row = source.start_row.saturating_add(source.rows.len());
        if start_row < first_data_row || end_row < start_row || end_row > last_data_row {
            return Err(format!("Choose a valid inclusive source row range from {first_data_row} to {last_data_row}."));
        }
        if end_row.saturating_sub(start_row).saturating_add(1) > MAX_ROWS {
            return Err("The selected accounting row range exceeds FRIDAY's safe row limit.".into());
        }
        let definition = crate::domain_formula::formula_registry().into_iter()
            .find(|item| item.id == formula_id)
            .ok_or_else(|| "This formula is not in FRIDAY's reviewed formula registry.".to_string())?;
        let mut values = Vec::with_capacity(definition.required_inputs.len());
        let mut mappings = Vec::with_capacity(definition.required_inputs.len());
        let mut source_cells = Vec::new();
        let mut contributing_rows = HashSet::new();
        let mut blank_cells_skipped = 0usize;
        for input_name in &definition.required_inputs {
            let field_id = format!("accounting.{input_name}");
            let mapping = session.confirmed_mappings.values().find(|mapping|
                mapping.sheet_name == sheet_name && mapping.field_id == field_id
            ).ok_or_else(|| format!("Confirm the '{}' source column in this worksheet before calculating.", input_name))?;
            if mapping.source_sha256 != session.source_sha256 {
                return Err("A confirmed accounting mapping no longer matches this workbook source fingerprint.".into());
            }
            let column_index = mapping.source_column_index.checked_sub(source.start_column + 1)
                .ok_or_else(|| "The confirmed source column index is invalid.".to_string())?;
            let header = source.rows.first().and_then(|row| row.get(column_index)).map(String::as_str).unwrap_or("");
            if header != mapping.header {
                return Err("The confirmed accounting header no longer matches the retained source cells.".into());
            }
            let column_label = excel_column_label(column_index + source.start_column);
            let mut numeric_values = Vec::new();
            for (row_index, row) in source.rows.iter().enumerate().skip(1) {
                let absolute_row = source.start_row.saturating_add(row_index).saturating_add(1);
                if absolute_row < start_row || absolute_row > end_row { continue; }
                if is_accounting_total_candidate(row) {
                    return Err(format!("The selected range includes a possible subtotal or total row at worksheet row {absolute_row}. Narrow the range to exclude it, then review the source before recalculating."));
                }
                let raw = row.get(column_index).map(|value| value.trim()).unwrap_or("");
                if raw.is_empty() {
                    blank_cells_skipped = blank_cells_skipped.saturating_add(1);
                    continue;
                }
                if source_cells.len() >= MAX_ACCOUNTING_SOURCE_REFERENCES {
                    return Err("This calculation exceeds FRIDAY's 20,000-cell evidence limit. Narrow the source data before calculating.".into());
                }
                numeric_values.push(raw.to_string());
                contributing_rows.insert(row_index);
                source_cells.push(AccountingSourceCell {
                    field_id: field_id.clone(),
                    cell: format!("{column_label}{}", source.start_row + row_index + 1),
                    value: raw.to_string(),
                });
            }
            if numeric_values.is_empty() {
                return Err(format!("The confirmed '{}' column contains no numeric source values.", input_name));
            }
            let total = crate::domain_formula::sum_currency_values(&numeric_values)
                .map_err(|_| format!("The confirmed '{}' column contains a non-numeric or unsupported value. No calculation was performed.", input_name))?;
            values.push((input_name.to_string(), total));
            mappings.push(mapping.clone());
        }
        let source_rows_included = contributing_rows.len();
        let mut formula = crate::domain_formula::calculate_from_workbook(formula_id, currency, values)?;
        for mapping in &mappings {
            formula.trace.push(format!(
                "Source mapping: {} <- {}!{} ({})",
                mapping.field_id, mapping.sheet_name, mapping.header_cell, mapping.header
            ));
        }
        formula.trace.push(format!("Source cells retained in evidence: {}", source_cells.len()));
        if blank_cells_skipped > 0 {
            formula.warnings.push(format!("{blank_cells_skipped} blank source cell(s) were skipped during column aggregation."));
        }
        Ok(WorkbookAccountingCalculation {
            source_sha256: session.source_sha256.clone(),
            file_name: session.file_name.clone(),
            sheet_name: sheet_name.to_string(),
            formula,
            confirmed_mappings: mappings,
            source_cells,
            source_rows_included,
            selected_start_row: start_row,
            selected_end_row: end_row,
            blank_cells_skipped,
        })
    }

    fn confirm_accounting_mapping(
        &self,
        id: &str,
        sheet_name: &str,
        source_column_index: usize,
        field_id: &str,
    ) -> Result<ConfirmedAccountingMapping, String> {
        const ALLOWED_FIELDS: &[&str] = &[
            "accounting.gross_sales", "accounting.discounts", "accounting.sales_returns", "accounting.cost_of_goods_sold", "accounting.net_sales",
        ];
        if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("This workbook analysis session is invalid. Import the workbook again.".into());
        }
        if !ALLOWED_FIELDS.contains(&field_id) || sheet_name.chars().count() > 180 {
            return Err("The accounting field or worksheet selector is not supported.".into());
        }
        let mut sessions = self.sessions.lock()
            .map_err(|_| "The workbook session store is unavailable. Import the workbook again.".to_string())?;
        sessions.retain(|_, item| item.created_at.elapsed() < WORKBOOK_SESSION_TTL);
        let session = sessions.get_mut(id)
            .ok_or_else(|| "This workbook analysis session has expired or was replaced. Import the workbook again.".to_string())?;
        let sheet = session.sheets.iter().find(|sheet| sheet.name == sheet_name)
            .ok_or_else(|| "That worksheet does not belong to this workbook session.".to_string())?;
        let suggestion = sheet.accounting_field_suggestions.iter()
            .find(|item| item.source_column_index == source_column_index)
            .ok_or_else(|| "That source column is not part of the selected worksheet.".to_string())?;
        if suggestion.status != "suggested" || suggestion.field_id.as_deref() != Some(field_id) {
            return Err("Only an unambiguous FRIDAY suggestion can be confirmed. Ambiguous or unmapped fields require a separate review flow.".into());
        }
        if session.confirmed_mappings.values().any(|mapping|
            mapping.field_id == field_id || (mapping.sheet_name == sheet_name && mapping.source_column_index == source_column_index)
        ) {
            return Err("This accounting field or source column already has a confirmed mapping in this workbook session.".into());
        }
        let mapping = ConfirmedAccountingMapping {
            field_id: field_id.to_string(),
            sheet_name: sheet.name.clone(),
            header: suggestion.header.clone(),
            source_column_index,
            header_cell: suggestion.header_cell.clone(),
            data_range: suggestion.data_range.clone(),
            source_sha256: session.source_sha256.clone(),
        };
        session.confirmed_mappings.insert(
            format!("{}:{}:{}", sheet_name, source_column_index, field_id),
            mapping.clone(),
        );
        Ok(mapping)
    }

    #[cfg(test)]
    fn session_count(&self) -> usize { self.sessions.lock().map(|items| items.len()).unwrap_or(0) }
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

fn accounting_field_suggestions(
    header: &[String], row_count: usize, column_count: usize, start_row: usize, start_column: usize,
) -> Vec<AccountingFieldSuggestion> {
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
        let letter = excel_column_label(column + start_column);
        let field_id = if candidates.len() == 1 { Some(candidates[0].to_string()) } else { None };
        let status = if candidates.len() > 1 { "ambiguous" } else if candidates.len() == 1 { "suggested" } else { "unmapped" };
        suggestions.push(AccountingFieldSuggestion {
            header: if name.is_empty() { format!("Column {}", column + 1) } else { name.chars().take(180).collect() },
            field_id,
            possible_fields: candidates.iter().map(|candidate| candidate.to_string()).collect(),
            status: status.into(),
            source_column_index: column + start_column + 1,
            header_cell: format!("{letter}{}", start_row + 1),
            data_range: if row_count > 1 { format!("{letter}{}:{letter}{}", start_row + 2, start_row + row_count) } else { format!("{letter}{} (no data rows)", start_row + 2) },
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

fn analyze_matrix(name: String, rows: Vec<Vec<String>>, formula_cells: usize) -> Result<SheetAnalysis, String> {
    analyze_matrix_with_origin(name, rows, formula_cells, 0, 0)
}

fn analyze_matrix_with_origin(
    name: String,
    rows: Vec<Vec<String>>,
    formula_cells: usize,
    start_row: usize,
    start_column: usize,
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
    let accounting_field_suggestions = accounting_field_suggestions(&header, rows.len(), column_count, start_row, start_column);
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
    let source_sheets = vec![WorkbookSourceSheet { name: "CSV data".into(), rows: rows.clone(), start_row: 0, start_column: 0 }];
    let sheet = analyze_matrix("CSV data".into(), rows, 0)?;
    let total_rows = sheet.row_count;
    let total_non_empty_cells = sheet.non_empty_cells;
    Ok(WorkbookAnalysis {
        session_id: String::new(),
        file_name, format: "CSV".into(), source_sha256, source_byte_length, sheets: vec![sheet], total_rows,
        total_non_empty_cells,
        source_sheets,
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
    let mut source_sheets = Vec::with_capacity(sheet_names.len());
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
        let (start_row, start_column) = range.start().unwrap_or((0, 0));
        let start_row = start_row as usize;
        let start_column = start_column as usize;
        let rows = range.rows().map(|row| row.iter().map(|cell| {
            if *cell == Data::Empty { String::new() } else { cell.to_string() }
        }).collect::<Vec<_>>()).collect::<Vec<_>>();
        source_sheets.push(WorkbookSourceSheet { name: name.clone(), rows: rows.clone(), start_row, start_column });
        let formula_cells = workbook.worksheet_formula(&name).ok()
            .map(|formulas| formulas.rows().flat_map(|row| row.iter()).filter(|formula| !formula.trim().is_empty()).count())
            .unwrap_or(0);
        sheets.push(analyze_matrix_with_origin(name, rows, formula_cells, start_row, start_column)?);
    }
    let total_rows = sheets.iter().map(|sheet| sheet.row_count).sum();
    let total_non_empty_cells = sheets.iter().map(|sheet| sheet.non_empty_cells).sum();
    Ok(WorkbookAnalysis {
        session_id: String::new(),
        file_name, format: "XLSX".into(), source_sha256, source_byte_length, sheets, total_rows, total_non_empty_cells,
        source_sheets,
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
    session_id: String,
    sheet_name: String,
    sessions: State<'_, WorkbookSessionStore>,
) -> Result<ExcelQuestionAnswer, String> {
    crate::permissions::authorize_command("excel.answer_question", "excel.analysis.local")?;
    sessions.answer(&session_id, &sheet_name, &question)
}

#[tauri::command]
pub fn confirm_excel_accounting_mapping(
    session_id: String,
    sheet_name: String,
    source_column_index: usize,
    field_id: String,
    sessions: State<'_, WorkbookSessionStore>,
) -> Result<ConfirmedAccountingMapping, String> {
    crate::permissions::authorize_command("excel.confirm_accounting_mapping", "excel.mapping.confirm")?;
    sessions.confirm_accounting_mapping(&session_id, &sheet_name, source_column_index, &field_id)
}

#[tauri::command]
pub fn calculate_excel_accounting_formula(
    session_id: String,
    sheet_name: String,
    formula_id: String,
    currency: String,
    start_row: usize,
    end_row: usize,
    sessions: State<'_, WorkbookSessionStore>,
) -> Result<WorkbookAccountingCalculation, String> {
    crate::permissions::authorize_command("excel.calculate_accounting_formula", "excel.accounting.calculate")?;
    sessions.calculate_accounting_formula(&session_id, &sheet_name, &formula_id, &currency, start_row, end_row)
}

#[tauri::command]
pub async fn pick_and_analyze_workbook(app: AppHandle, sessions: State<'_, WorkbookSessionStore>) -> Result<WorkbookAnalysis, String> {
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
    let session_id = sessions.create(&analysis)?;
    let mut analysis = analysis;
    analysis.session_id = session_id;
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

    fn session_test_analysis() -> WorkbookAnalysis {
        analyze_csv("session.csv".into(), b"Product,Revenue\nTea,10\nCoffee,20\n").unwrap()
    }

    #[test]
    fn workbook_sessions_retain_exact_source_cells_only_in_backend_state() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv("source.csv".into(), b"Product,Gross Sales\nTea,10\nCoffee,20\n").unwrap();
        let id = store.create(&analysis).unwrap();
        assert!(!serde_json::to_string(&analysis).unwrap().contains("sourceSheets"));
        let sessions = store.sessions.lock().unwrap();
        let source = sessions.get(&id).unwrap().source_sheets.iter()
            .find(|sheet| sheet.name == "CSV data").unwrap();
        assert_eq!(source.rows[0], vec!["Product", "Gross Sales"]);
        assert_eq!(source.rows[1], vec!["Tea", "10"]);
        assert_eq!(source.rows[2], vec!["Coffee", "20"]);
    }

    #[test]
    fn workbook_sessions_answer_from_backend_held_analysis() {
        let store = WorkbookSessionStore::default();
        let id = store.create(&session_test_analysis()).unwrap();
        let answer = store.answer(&id, "CSV data", "total revenue").unwrap();
        assert_eq!(answer.status, "answered");
        assert!(answer.answer.contains("30.00"));
    }

    #[test]
    fn confirmed_accounting_mapping_is_bound_to_session_sheet_column_and_source() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv("mapping.csv".into(), b"Product,Gross Sales\nTea,10\n").unwrap();
        let id = store.create(&analysis).unwrap();
        let mapping = store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        assert_eq!(mapping.field_id, "accounting.gross_sales");
        assert_eq!(mapping.header, "Gross Sales");
        assert_eq!(mapping.header_cell, "B1");
        assert_eq!(mapping.data_range, "B2:B2");
        assert_eq!(mapping.source_sha256, analysis.source_sha256);
    }

    #[test]
    fn accounting_calculation_uses_only_confirmed_backend_source_cells_and_keeps_trace() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "sales.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\nCoffee,50,3,1\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();
        let result = store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, 3).unwrap();
        assert_eq!(result.formula.status, "calculated");
        assert_eq!(result.formula.output_value.as_deref(), Some("139"));
        assert_eq!(result.formula.currency.as_deref(), Some("PKR"));
        assert_eq!(result.confirmed_mappings.len(), 3);
        assert_eq!(result.source_cells.len(), 6);
        assert!(result.source_cells.iter().any(|cell| cell.cell == "B2" && cell.value == "100"));
        assert!(result.source_cells.iter().any(|cell| cell.cell == "D3" && cell.value == "1"));
        assert!(result.formula.trace.iter().any(|line| line.contains("Source mapping: accounting.gross_sales")));
        assert_eq!(result.source_sha256, analysis.source_sha256);
    }

    #[test]
    fn accounting_calculation_honors_an_explicit_inclusive_row_range() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "sales.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\nCoffee,50,3,1\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();
        let result = store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, 2).unwrap();
        assert_eq!(result.formula.output_value.as_deref(), Some("93"));
        assert_eq!(result.selected_start_row, 2);
        assert_eq!(result.selected_end_row, 2);
        assert_eq!(result.source_rows_included, 1);
        assert_eq!(result.source_cells.len(), 3);
        assert!(store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 1, 2).is_err());
        assert!(store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 3, 2).is_err());
        assert!(store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, 4).is_err());
    }

    #[test]
    fn accounting_calculation_rejects_ranges_containing_possible_total_rows() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "sales.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\nCoffee,50,3,1\nGrand Total,150,8,3\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();
        let error = match store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, 4) {
            Ok(_) => panic!("a range containing a possible total row must be rejected"),
            Err(error) => error,
        };
        assert!(error.contains("possible subtotal or total row"));
        assert!(error.contains("row 4"));
        let result = store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, 3).unwrap();
        assert_eq!(result.formula.output_value.as_deref(), Some("139"));
    }

    #[test]
    fn accounting_calculation_requires_confirmed_mappings_and_rejects_non_numeric_source_cells() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "sales.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\nCoffee,not-a-number,3,1\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        assert!(store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, 3).is_err());
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();
        let error = match store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, 3) {
            Ok(_) => panic!("non-numeric source data must not calculate"),
            Err(error) => error,
        };
        assert!(error.contains("non-numeric"));
    }

    #[test]
    fn accounting_calculation_skips_blank_cells_with_an_explicit_warning() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "sales.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\nCoffee,,3,1\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();
        let result = store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, 3).unwrap();
        assert_eq!(result.formula.output_value.as_deref(), Some("89"));
        assert_eq!(result.blank_cells_skipped, 1);
        assert!(result.formula.warnings.iter().any(|warning| warning.contains("blank source cell")));
    }

    #[test]
    fn accounting_source_references_preserve_non_a1_worksheet_coordinates() {
        let rows = vec![
            vec!["Product".to_string(), "Gross Sales".to_string()],
            vec!["Tea".to_string(), "10".to_string()],
        ];
        let sheet = analyze_matrix_with_origin("Offset".into(), rows, 0, 4, 2).unwrap();
        let suggestion = sheet.accounting_field_suggestions.iter()
            .find(|item| item.field_id.as_deref() == Some("accounting.gross_sales")).unwrap();
        assert_eq!(suggestion.source_column_index, 4);
        assert_eq!(suggestion.header_cell, "D5");
        assert_eq!(suggestion.data_range, "D6:D6");
    }

    #[test]
    fn accounting_mapping_rejects_ambiguous_unknown_and_duplicate_mappings() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv("mapping.csv".into(), b"Product,Revenue,Gross Sales\nTea,10,12\n").unwrap();
        let id = store.create(&analysis).unwrap();
        assert!(store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").is_err());
        assert!(store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.net_sales").is_err());
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.gross_sales").unwrap();
        assert!(store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.gross_sales").is_err());
        assert!(store.confirm_accounting_mapping(&id, "Other sheet", 3, "accounting.gross_sales").is_err());
        assert!(store.confirm_accounting_mapping("00000000000000000000000000000000", "CSV data", 3, "accounting.gross_sales").is_err());
    }

    #[test]
    fn workbook_sessions_reject_forged_ids_and_unknown_worksheets() {
        let store = WorkbookSessionStore::default();
        let id = store.create(&session_test_analysis()).unwrap();
        assert!(store.answer("00000000000000000000000000000000", "CSV data", "total revenue").is_err());
        assert!(store.answer(&id, "Other sheet", "total revenue").is_err());
        assert!(store.answer("not-a-session", "CSV data", "total revenue").is_err());
    }

    #[test]
    fn workbook_sessions_expire_and_require_reimport() {
        let store = WorkbookSessionStore::default();
        let id = store.create(&session_test_analysis()).unwrap();
        {
            let mut sessions = store.sessions.lock().unwrap();
            sessions.get_mut(&id).unwrap().created_at = Instant::now() - WORKBOOK_SESSION_TTL - Duration::from_secs(1);
        }
        assert!(store.answer(&id, "CSV data", "total revenue").is_err());
        assert_eq!(store.session_count(), 0);
    }

    #[test]
    fn workbook_answers_include_bound_source_fingerprint() {
        let store = WorkbookSessionStore::default();
        let analysis = session_test_analysis();
        let expected = analysis.source_sha256[..12].to_string();
        let id = store.create(&analysis).unwrap();
        let answer = store.answer(&id, "CSV data", "total revenue").unwrap();
        assert!(answer.evidence.iter().any(|item| item.contains(&expected)));
    }

    #[test]
    fn workbook_sessions_are_bounded_and_evict_oldest() {
        let store = WorkbookSessionStore::default();
        let analysis = session_test_analysis();
        let mut ids = Vec::new();
        for _ in 0..=MAX_WORKBOOK_SESSIONS { ids.push(store.create(&analysis).unwrap()); }
        assert_eq!(store.session_count(), MAX_WORKBOOK_SESSIONS);
        assert!(store.answer(&ids[0], "CSV data", "total revenue").is_err());
        assert!(store.answer(ids.last().unwrap(), "CSV data", "total revenue").is_ok());
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
