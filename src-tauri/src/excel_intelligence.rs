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
use scraper::{Html, Selector};
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
const MAX_ACCOUNTING_PREVIEW_ROWS: usize = 100;
const MAX_ACCOUNTING_TOTAL_CANDIDATES: usize = 100;

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
    formula_cell_positions: Vec<(usize, usize)>,
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
pub struct AccountingRangePreviewCell {
    pub field_id: String,
    pub cell: String,
    pub value: String,
    pub is_formula_cell: bool,
    pub missing_cached_value: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountingRangePreviewRow {
    pub worksheet_row: usize,
    pub cells: Vec<AccountingRangePreviewCell>,
    pub possible_total: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountingTotalRowCandidate {
    pub worksheet_row: usize,
    pub labels: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookAccountingRangePreview {
    pub source_sha256: String,
    pub file_name: String,
    pub sheet_name: String,
    pub formula_id: String,
    pub selected_start_row: usize,
    pub selected_end_row: usize,
    pub selected_row_count: usize,
    pub missing_cached_formula_count: usize,
    pub preview_rows: Vec<AccountingRangePreviewRow>,
    pub preview_truncated: bool,
    pub possible_total_row_count: usize,
    pub possible_total_rows: Vec<AccountingTotalRowCandidate>,
    pub total_candidates_truncated: bool,
    pub confirmed_mappings: Vec<ConfirmedAccountingMapping>,
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
    pub cached_formula_cells_used: usize,
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


fn normalize_accounting_total_label(value: &str) -> String {
    value.to_lowercase()
        .chars()
        .filter_map(|ch| {
            // Arabic-script spreadsheets commonly mix optional vowel marks,
            // tatweel and regional forms of alef/yeh/kaf. Ignore presentation
            // marks and normalize only well-established letter variants;
            // matching remains against the explicit phrase allowlist below.
            if matches!(ch, '\u{0640}' | '\u{0670}' | '\u{06D6}'..='\u{06DC}' | '\u{06DF}'..='\u{06E4}' | '\u{06E7}'..='\u{06ED}')
                || matches!(ch, '\u{064B}'..='\u{065F}')
                // Bengali virama joins consonants into a single written word.
                || ch == '\u{09CD}'
            {
                return None;
            }
            let ch = match ch {
                'أ' | 'إ' | 'آ' | 'ٱ' => 'ا',
                'ى' | 'ی' => 'ي',
                'ک' => 'ك',
                other => other,
            };
            Some(if ch.is_alphanumeric() { ch } else { ' ' })
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_accounting_total_label(value: &str) -> bool {
    let label = normalize_accounting_total_label(value);
    if label.is_empty() {
        return false;
    }

    // Keep this list explicit: broad "starts with total" matching produces
    // false positives such as "Total Customers" or "Total Salesperson".
    // Prefix/suffix matching permits common qualifiers like currency or period.
    const TOTAL_LABELS: &[&str] = &[
        "total",
        "subtotal",
        "sub total",
        "grand total",
        "overall total",
        "net total",
        "total sales",
        "total revenue",
        "total amount",
        "total due",
        "total payable",
        "total receivable",
        "invoice total",
        "closing balance",
        "ending balance",
        "balance brought forward",
        "balance carried forward",
        "total general",
        "total de ventas",
        "total ingresos",
        "sous total",
        "total des ventes",
        "total des revenus",
        "gesamtsumme",
        "zwischensumme",
        "summe gesamt",
        "totale generale",
        "totale vendite",
        "total geral",
        "total de vendas",
        "total de receitas",
        "total de gastos",
        "total de compras",
        "total de despesas",
        "total de impuestos",
        "importe total",
        "suma total",
        "total facturado",
        "montant total",
        "montant dû",
        "total ttc",
        "total ht",
        "somme totale",
        "gesamtbetrag",
        "gesamt",
        "summe",
        "gesamtergebnis",
        "totale",
        "totale complessivo",
        "importo totale",
        "somma totale",
        "valor total",
        "soma total",
        "total a pagar",
        "valor a pagar",
        "إجمالي المبيعات",
        "إجمالي الإيرادات",
        "إجمالي المبلغ",
        "إجمالي المستحق",
        "مجموعی فروخت",
        "مجموعی آمدنی",
        "کل رقم",
        "کل واجب الادا",
        "الإجمالي",
        "الإجمالي العام",
        "المجموع",
        "المجموع الكلي",
        "المجموع الفرعي",
        "مجموعی کل",
        "ذیلی کل",
        "کل فروخت",
        "کل آمدنی",
        "جمع کل",
        "جمع فرعی",
        // Dutch
        "eindtotaal",
        "subtotaal",
        "algemeen totaal",
        // Turkish
        "genel toplam",
        "ara toplam",
        "toplam",
        // Persian (normalized Arabic-script letter variants)
        "مجموع کل",
        "مبلغ کل",
        "کل فروش",
        "کل درآمد",
        "جمع جزء",
        // Hindi
        "कुल",
        "कुल योग",
        "उप योग",
        // Bengali
        "মোট",
        "সর্বমোট",
        "উপমোট",
    ];

    fn is_period_qualifier(token: &str) -> bool {
        matches!(token, "fy" | "q1" | "q2" | "q3" | "q4" | "h1" | "h2")
            || (token.len() == 4 && token.chars().all(|ch| ch.is_ascii_digit()))
    }

    fn is_currency_qualifier(token: &str) -> bool {
        matches!(
            token,
            "pkr" | "usd" | "eur" | "gbp" | "inr" | "aed" | "sar" | "cad" | "aud" | "jpy"
                | "try" | "chf" | "cny" | "brl" | "mxn" | "nzd" | "bdt"
        )
    }

    TOTAL_LABELS.iter().any(|phrase| {
        // Normalize both sides. Otherwise Arabic alef/yeh/kaf variants in
        // the allowlist differ from the normalized source label.
        let normalized_phrase = normalize_accounting_total_label(phrase);
        let phrase_tokens = normalized_phrase.split_whitespace().collect::<Vec<_>>();
        let label_tokens = label.split_whitespace().collect::<Vec<_>>();
        if label_tokens == phrase_tokens {
            return true;
        }
        if phrase_tokens == ["total"] {
            // A bare "Total" is a candidate; descriptive labels are not.
            return false;
        }
        if label_tokens.len() <= phrase_tokens.len() {
            return false;
        }

        let prefix_matches = label_tokens.ends_with(&phrase_tokens)
            && label_tokens[..label_tokens.len() - phrase_tokens.len()]
                .iter()
                .all(|token| is_period_qualifier(token));
        let suffix_matches = label_tokens.starts_with(&phrase_tokens)
            && label_tokens[phrase_tokens.len()..]
                .iter()
                .all(|token| is_currency_qualifier(token));
        prefix_matches || suffix_matches
    })
}

fn accounting_total_labels(row: &[String]) -> Vec<String> {
    row.iter()
        .filter(|value| is_accounting_total_label(value))
        .map(|value| value.trim().to_string())
        .collect()
}

fn is_accounting_total_candidate(row: &[String]) -> bool {
    !accounting_total_labels(row).is_empty()
}

fn validate_accounting_mapping(
    mapping: &ConfirmedAccountingMapping,
    source: &WorkbookSourceSheet,
    expected_source_sha256: &str,
) -> Result<usize, String> {
    if mapping.source_sha256 != expected_source_sha256 {
        return Err("A confirmed accounting mapping no longer matches this workbook source fingerprint.".into());
    }
    let column_index = mapping.source_column_index.checked_sub(source.start_column + 1)
        .ok_or_else(|| "The confirmed source column index is invalid.".to_string())?;
    let header_row = source.rows.first()
        .ok_or_else(|| "The retained worksheet header row is unavailable.".to_string())?;
    let header = header_row.get(column_index)
        .ok_or_else(|| "The confirmed source column is outside the retained worksheet bounds.".to_string())?;
    if header != &mapping.header {
        return Err("The confirmed accounting header no longer matches the retained source cells.".into());
    }

    let column_label = excel_column_label(source.start_column + column_index);
    let expected_header_cell = format!("{column_label}{}", source.start_row + 1);
    let expected_data_range = if source.rows.len() > 1 {
        format!("{column_label}{}:{column_label}{}", source.start_row + 2, source.start_row + source.rows.len())
    } else {
        format!("{column_label}{} (no data rows)", source.start_row + 2)
    };
    if mapping.header_cell != expected_header_cell || mapping.data_range != expected_data_range {
        return Err("The confirmed accounting mapping range metadata no longer matches the retained worksheet coordinates.".into());
    }
    Ok(column_index)
}

fn validate_session_integrity(session: &WorkbookSession) -> Result<(), String> {
    if session.source_sha256.len() != 64
        || !session.source_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || session.source_byte_length == 0
        || !matches!(session.format.as_str(), "CSV" | "XLSX")
        || session.file_name.is_empty()
    {
        return Err("The retained workbook session metadata is inconsistent. Import the workbook again.".into());
    }
    if session.sheets.is_empty()
        || session.sheets.len() != session.source_sheets.len()
        || session.sheets.len() > MAX_SHEETS
    {
        return Err("The retained worksheet summaries do not match the source session. Import the workbook again.".into());
    }
    let mut names = HashSet::new();
    for (summary, source) in session.sheets.iter().zip(session.source_sheets.iter()) {        if summary.name != source.name
            || !names.insert(summary.name.as_str())
            || summary.row_count != source.rows.len()
            || summary.column_count != source.rows.iter().map(Vec::len).max().unwrap_or(0)
            || source.rows.is_empty()
            || source.rows.len() > MAX_ROWS
            || source.start_row.checked_add(source.rows.len()).map_or(true, |end| end > 1_048_576)
            || source.start_column.checked_add(summary.column_count).map_or(true, |end| end > 16_384)
            || source.formula_cell_positions.iter().any(|(row, column)|
                *row >= source.rows.len() || *column >= summary.column_count
            )
        {
            return Err("The retained worksheet dimensions or source coordinates are inconsistent. Import the workbook again.".into());
        }
    }

    let mut confirmed_fields = HashSet::new();
    let mut confirmed_columns = HashSet::new();
    for (key, mapping) in &session.confirmed_mappings {
        const ALLOWED_FIELDS: &[&str] = &[
            "accounting.gross_sales",
            "accounting.discounts",
            "accounting.sales_returns",
            "accounting.cost_of_goods_sold",
            "accounting.net_sales",
        ];
        if !ALLOWED_FIELDS.contains(&mapping.field_id.as_str())
            || !confirmed_fields.insert(mapping.field_id.as_str())
            || !confirmed_columns.insert((mapping.sheet_name.as_str(), mapping.source_column_index))
            || key != &format!("{}:{}:{}", mapping.sheet_name, mapping.source_column_index, mapping.field_id)
        {
            return Err("The retained confirmed accounting mappings are inconsistent. Re-import the workbook and review the mappings again.".into());
        }
        let source = session.source_sheets.iter()
            .find(|source| source.name == mapping.sheet_name)
            .ok_or_else(|| "A confirmed accounting mapping refers to a worksheet outside this workbook session.".to_string())?;
        validate_accounting_mapping(mapping, source, &session.source_sha256)?;
    }
    Ok(())
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
        validate_session_integrity(session)?;
        let sheet = session.sheets.iter().find(|sheet| sheet.name == sheet_name)
            .ok_or_else(|| "That worksheet does not belong to this workbook session.".to_string())?;
        let mut answer = answer_question(question, &sheet.name, sheet.row_count, &sheet.columns);
        answer.evidence.push(format!("Workbook: {} ({})", session.file_name, session.format));
        answer.evidence.push(format!("Source SHA-256: {}… ({} bytes)", &session.source_sha256[..12], session.source_byte_length));
        answer.method.push_str("; bound to the imported workbook session");
        Ok(answer)
    }

    fn preview_accounting_range(
        &self,
        id: &str,
        sheet_name: &str,
        formula_id: &str,
        start_row: usize,
        end_row: usize,
    ) -> Result<WorkbookAccountingRangePreview, String> {
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
        validate_session_integrity(session)?;
        let _sheet = session.sheets.iter().find(|sheet| sheet.name == sheet_name)
            .ok_or_else(|| "That worksheet does not belong to this workbook session.".to_string())?;
        let source = session.source_sheets.iter().find(|source| source.name == sheet_name)
            .ok_or_else(|| "The original source cells are unavailable in this workbook session. Import the workbook again.".to_string())?;
        let first_data_row = source.start_row.saturating_add(2);
        let last_data_row = source.start_row.saturating_add(source.rows.len());
        if start_row < first_data_row || end_row < start_row || end_row > last_data_row {
            return Err(format!("Choose a valid inclusive source row range from {first_data_row} to {last_data_row}."));
        }
        let selected_row_count = end_row.saturating_sub(start_row).saturating_add(1);
        if selected_row_count > MAX_ROWS {
            return Err("The selected accounting row range exceeds FRIDAY's safe row limit.".into());
        }
        let definition = crate::domain_formula::formula_registry().into_iter()
            .find(|item| item.id == formula_id)
            .ok_or_else(|| "This formula is not in FRIDAY's reviewed formula registry.".to_string())?;
        let mut mappings = Vec::with_capacity(definition.required_inputs.len());
        let mut columns = Vec::with_capacity(definition.required_inputs.len());
        for input_name in &definition.required_inputs {
            let field_id = format!("accounting.{input_name}");
            let mapping = session.confirmed_mappings.values().find(|mapping|
                mapping.sheet_name == sheet_name && mapping.field_id == field_id
            ).ok_or_else(|| format!("Confirm the '{}' source column in this worksheet before previewing.", input_name))?;
            let column_index = validate_accounting_mapping(mapping, source, &session.source_sha256)?;
            columns.push((mapping.field_id.clone(), excel_column_label(column_index + source.start_column), column_index));
            mappings.push(mapping.clone());
        }
        let mut preview_rows = Vec::with_capacity(selected_row_count.min(MAX_ACCOUNTING_PREVIEW_ROWS));
        let mut possible_total_rows = Vec::new();
        let mut possible_total_row_count = 0usize;
        let mut missing_cached_formula_count = 0usize;
        for (row_index, row) in source.rows.iter().enumerate().skip(1) {
            let absolute_row = source.start_row.saturating_add(row_index).saturating_add(1);
            if absolute_row < start_row || absolute_row > end_row { continue; }
            let possible_total = is_accounting_total_candidate(row);
            if possible_total {
                possible_total_row_count = possible_total_row_count.saturating_add(1);
                if possible_total_rows.len() < MAX_ACCOUNTING_TOTAL_CANDIDATES {
                    // Surface the actual matched label cells, not arbitrary leading row values.
                    let labels = accounting_total_labels(row).into_iter().take(3)
                        .map(|value| value.chars().take(100).collect::<String>()).collect::<Vec<_>>();
                    possible_total_rows.push(AccountingTotalRowCandidate { worksheet_row: absolute_row, labels });
                }
            }
            for (_, _, column_index) in &columns {
                let is_formula_cell = source.formula_cell_positions.contains(&(row_index, *column_index));
                let has_cached_value = row.get(*column_index).map(|value| !value.trim().is_empty()).unwrap_or(false);
                if is_formula_cell && !has_cached_value {
                    missing_cached_formula_count = missing_cached_formula_count.saturating_add(1);
                }
            }
            if preview_rows.len() < MAX_ACCOUNTING_PREVIEW_ROWS {
                let cells = columns.iter().map(|(field_id, column_label, column_index)| {
                    let value = row.get(*column_index).map(|value| value.chars().take(120).collect::<String>()).unwrap_or_default();
                    let is_formula_cell = source.formula_cell_positions.contains(&(row_index, *column_index));
                    let missing_cached_value = is_formula_cell && row.get(*column_index).map(|value| value.trim().is_empty()).unwrap_or(true);
                    AccountingRangePreviewCell { field_id: field_id.clone(), cell: format!("{column_label}{absolute_row}"), value, is_formula_cell, missing_cached_value }
                }).collect();
                preview_rows.push(AccountingRangePreviewRow { worksheet_row: absolute_row, cells, possible_total });
            }
        }
        Ok(WorkbookAccountingRangePreview {
            source_sha256: session.source_sha256.clone(),
            file_name: session.file_name.clone(),
            sheet_name: sheet_name.to_string(),
            formula_id: formula_id.to_string(),
            selected_start_row: start_row,
            selected_end_row: end_row,
            selected_row_count,
            missing_cached_formula_count,
            preview_truncated: selected_row_count > preview_rows.len(),
            possible_total_row_count,
            total_candidates_truncated: possible_total_row_count > possible_total_rows.len(),
            preview_rows,
            possible_total_rows,
            confirmed_mappings: mappings,
        })
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
        validate_session_integrity(session)?;
        let sheet = session.sheets.iter().find(|sheet| sheet.name == sheet_name)
            .ok_or_else(|| "That worksheet does not belong to this workbook session.".to_string())?;        let source = session.source_sheets.iter().find(|source| source.name == sheet_name)
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
        let mut cached_formula_cells_used = 0usize;
        for input_name in &definition.required_inputs {
            let field_id = format!("accounting.{input_name}");
            let mapping = session.confirmed_mappings.values().find(|mapping|
                mapping.sheet_name == sheet_name && mapping.field_id == field_id
            ).ok_or_else(|| format!("Confirm the '{}' source column in this worksheet before calculating.", input_name))?;
            let column_index = validate_accounting_mapping(mapping, source, &session.source_sha256)?;
            let column_label = excel_column_label(column_index + source.start_column);
            let mut numeric_values = Vec::new();
            for (row_index, row) in source.rows.iter().enumerate().skip(1) {
                let absolute_row = source.start_row.saturating_add(row_index).saturating_add(1);
                if absolute_row < start_row || absolute_row > end_row { continue; }
                if is_accounting_total_candidate(row) {
                    return Err(format!("The selected range includes a possible subtotal or total row at worksheet row {absolute_row}. Narrow the range to exclude it, then review the source before recalculating."));
                }
                let raw = row.get(column_index).map(|value| value.trim()).unwrap_or("");
                let is_formula_cell = source.formula_cell_positions.contains(&(row_index, column_index));
                if is_formula_cell && raw.is_empty() {
                    return Err(format!("Formula cell {}{} has no cached result. FRIDAY will not recalculate it or treat it as zero; open and recalculate the workbook in Excel, save it, then reimport.", column_label, absolute_row));
                }
                if is_formula_cell {
                    cached_formula_cells_used = cached_formula_cells_used.saturating_add(1);
                }
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
        if cached_formula_cells_used > 0 {
            formula.warnings.push(format!("{cached_formula_cells_used} formula cell(s) contributed cached values. FRIDAY did not recalculate them; cached results may be stale or differ from Excel's next recalculation."));
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
            cached_formula_cells_used,
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
        validate_session_integrity(session)?;
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
    let before = file.metadata()        .map_err(|_| "FRIDAY could not verify the selected workbook.".to_string())?;
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
    let source_sheets = vec![WorkbookSourceSheet { name: "CSV data".into(), rows: rows.clone(), start_row: 0, start_column: 0, formula_cell_positions: Vec::new() }];
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


// Read formula metadata directly from OOXML. Calamine's cached-value-oriented
// formula range can omit formula cells whose cached result is absent.
fn xlsx_part_path(base: &str, target: &str) -> Result<String, String> {
    if target.is_empty() || target.contains('\\') || target.contains('?') || target.contains('#') || target.contains(':') {
        return Err("The workbook contains an unsupported worksheet relationship target.".into());
    }
    let mut parts: Vec<&str> = if target.starts_with('/') { Vec::new() } else { base.split('/').filter(|part| !part.is_empty()).collect() };
    for part in target.trim_start_matches('/').split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err("The workbook contains a worksheet relationship outside its package.".into());
                }
            }
            value => parts.push(value),
        }
    }
    if parts.is_empty() {
        return Err("The workbook contains an invalid worksheet relationship.".into());
    }
    Ok(parts.join("/"))
}

fn excel_cell_reference(reference: &str) -> Option<(usize, usize)> {    let mut column = 0usize;
    let mut letters = 0usize;
    let mut row_start = 0usize;
    for (index, byte) in reference.bytes().enumerate() {
        if byte.is_ascii_alphabetic() {
            column = column.checked_mul(26)?.checked_add((byte.to_ascii_uppercase() - b'A' + 1) as usize)?;
            letters += 1;
            row_start = index + 1;
        } else {
            break;
        }
    }
    if letters == 0 || row_start >= reference.len() { return None; }
    let row = reference.get(row_start..)?.parse::<usize>().ok()?;
    if row == 0 { return None; }
    Some((row - 1, column - 1))
}

fn xlsx_formula_cells_by_sheet(bytes: &[u8]) -> Result<HashMap<String, Vec<(usize, usize)>>, String> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| "The XLSX package is damaged while reading formula metadata.".to_string())?;
    let mut workbook_xml = String::new();
    archive.by_name("xl/workbook.xml")
        .map_err(|_| "The XLSX workbook metadata is missing.".to_string())?
        .read_to_string(&mut workbook_xml)
        .map_err(|_| "The XLSX workbook metadata could not be read.".to_string())?;
    let mut relationships_xml = String::new();
    archive.by_name("xl/_rels/workbook.xml.rels")
        .map_err(|_| "The XLSX worksheet relationships are missing.".to_string())?
        .read_to_string(&mut relationships_xml)
        .map_err(|_| "The XLSX worksheet relationships could not be read.".to_string())?;

    let workbook_document = Html::parse_document(&workbook_xml);
    let relationships_document = Html::parse_document(&relationships_xml);
    let sheet_selector = Selector::parse("sheet").map_err(|_| "FRIDAY could not inspect workbook sheet metadata.".to_string())?;
    let relationship_selector = Selector::parse("relationship").map_err(|_| "FRIDAY could not inspect workbook relationships.".to_string())?;
    let mut targets = HashMap::new();
    for relationship in relationships_document.select(&relationship_selector) {
        if let (Some(id), Some(target)) = (relationship.value().attr("id"), relationship.value().attr("target")) {
            targets.insert(id.to_string(), target.to_string());
        }
    }

    let cell_selector = Selector::parse("c").map_err(|_| "FRIDAY could not inspect worksheet cells.".to_string())?;
    let formula_selector = Selector::parse("f").map_err(|_| "FRIDAY could not inspect worksheet formulas.".to_string())?;
    let mut result = HashMap::new();
    for sheet in workbook_document.select(&sheet_selector) {
        let name = sheet.value().attr("name")
            .ok_or_else(|| "The XLSX workbook contains a worksheet without a name.".to_string())?;
        let relationship_id = sheet.value().attr("r:id").or_else(|| sheet.value().attr("id"))
            .ok_or_else(|| "The XLSX workbook contains a worksheet without a relationship.".to_string())?;
        let target = targets.get(relationship_id)
            .ok_or_else(|| "The XLSX workbook references a missing worksheet relationship.".to_string())?;
        let part_path = xlsx_part_path("xl", target)?;
        let mut worksheet_xml = String::new();
        archive.by_name(&part_path)
            .map_err(|_| "The XLSX workbook references a missing worksheet file.".to_string())?
            .read_to_string(&mut worksheet_xml)
            .map_err(|_| "An XLSX worksheet could not be read for formula metadata.".to_string())?;
        let worksheet_document = Html::parse_document(&worksheet_xml);
        let mut positions = Vec::new();
        for cell in worksheet_document.select(&cell_selector) {
            if cell.select(&formula_selector).next().is_some() {
                if let Some(reference) = cell.value().attr("r") {
                    if let Some(position) = excel_cell_reference(reference) {
                        positions.push(position);
                    }
                }
            }
        }
        result.insert(name.to_string(), positions);
    }
    Ok(result)
}

fn analyze_xlsx(file_name: String, bytes: Vec<u8>) -> Result<WorkbookAnalysis, String> {
    let source_sha256 = format!("{:x}", Sha256::digest(&bytes));
    let source_byte_length = bytes.len() as u64;
    validate_xlsx_package(&bytes)?;
    let mut workbook: Xlsx<_> = open_workbook_from_rs(Cursor::new(bytes.as_slice()))
        .map_err(|_| "FRIDAY could not read this XLSX workbook. It may be damaged or use unsupported workbook features.".to_string())?;
    let sheet_names = workbook.sheet_names();
    if sheet_names.is_empty() || sheet_names.len() > MAX_SHEETS {
        return Err("A workbook must contain between 1 and 12 worksheets for this preview.".into());
    }
    let mut sheets = Vec::with_capacity(sheet_names.len());
    let mut source_sheets = Vec::with_capacity(sheet_names.len());
    let formula_cells_by_sheet = xlsx_formula_cells_by_sheet(&bytes)?;
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
        let formula_positions_absolute = formula_cells_by_sheet.get(&name).cloned().unwrap_or_default();
        let formula_cells = formula_positions_absolute.len();
        let formula_cell_positions = formula_positions_absolute.into_iter().filter_map(|(row, column)| {
            Some((row.checked_sub(start_row)?, column.checked_sub(start_column)?))
        }).filter(|(row, column)| *row < height && *column < width).collect::<Vec<_>>();
        source_sheets.push(WorkbookSourceSheet { name: name.clone(), rows: rows.clone(), start_row, start_column, formula_cell_positions });
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
    let average = has(&["average", "avg", "mean"]);    let maximum = has(&["highest", "maximum", "max", "largest", "biggest"]);
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
pub fn preview_excel_accounting_range(
    session_id: String,
    sheet_name: String,
    formula_id: String,
    start_row: usize,
    end_row: usize,
    sessions: State<'_, WorkbookSessionStore>,
) -> Result<WorkbookAccountingRangePreview, String> {
    crate::permissions::authorize_command("excel.preview_accounting_range", "excel.accounting.preview")?;
    sessions.preview_accounting_range(&session_id, &sheet_name, &formula_id, start_row, end_row)
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
        test_xlsx_with_formula_cache(include_macro, true)
    }

    fn test_xlsx_with_formula_cache(include_macro: bool, include_cached_formula_value: bool) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let cached_value = if include_cached_formula_value { "<v>3</v>" } else { "<v></v>" };
        let worksheet = format!(
            r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Product</t></is></c><c r="B1" t="inlineStr"><is><t>Gross Sales</t></is></c><c r="C1" t="inlineStr"><is><t>Discounts</t></is></c><c r="D1" t="inlineStr"><is><t>Sales Returns</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>Book</t></is></c><c r="B2"><f>1+2</f>{cached_value}</c><c r="C2"><v>1</v></c><c r="D2"><v>0</v></c></row></sheetData></worksheet>"#
        );
        let mut entries: Vec<(&str, String)> = vec![
            ("[Content_Types].xml", r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#.into()),
            ("_rels/.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#.into()),
            ("xl/workbook.xml", r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sales" sheetId="1" r:id="rId1"/></sheets></workbook>"#.into()),
            ("xl/_rels/workbook.xml.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#.into()),
            ("xl/worksheets/sheet1.xml", worksheet),
        ];
        if include_macro {
            entries.push(("xl/vbaProject.bin", "macro-bytes".into()));
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
        assert_eq!(answer.status, "answered");        assert!(answer.answer.contains("30.00"));
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
    fn accounting_preview_and_calculation_reject_tampered_mapping_range_metadata() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "mapping-integrity.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\nCoffee,50,3,1\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();

        {
            let mut sessions = store.sessions.lock().unwrap();
            let session = sessions.get_mut(&id).unwrap();
            let mapping = session.confirmed_mappings.values_mut()
                .find(|mapping| mapping.field_id == "accounting.discounts").unwrap();
            mapping.data_range = "C2:C999".into();
        }

        let preview_error = match store.preview_accounting_range(
            &id, "CSV data", "accounting.net_sales", 2, 3,
        ) {
            Err(error) => error,
            Ok(_) => panic!("preview must reject tampered mapping range metadata"),
        };
        assert!(preview_error.contains("range metadata"));

        let calculation_error = match store.calculate_accounting_formula(
            &id, "CSV data", "accounting.net_sales", "PKR", 2, 3,
        ) {
            Err(error) => error,
            Ok(_) => panic!("calculation must reject tampered mapping range metadata"),
        };
        assert!(calculation_error.contains("range metadata"));
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
    fn accounting_range_preview_is_source_bound_bounded_and_flags_total_candidates() {
        let store = WorkbookSessionStore::default();
        let mut csv = String::from("Product,Gross Sales,Discounts,Sales Returns\n");
        for index in 0..104 {
            csv.push_str(&format!("Item {index},100,5,2\n"));
        }
        csv.push_str("Grand Total,10400,520,208\n");
        let analysis = analyze_csv("sales.csv".into(), csv.as_bytes()).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();
        let preview = store.preview_accounting_range(&id, "CSV data", "accounting.net_sales", 2, 106).unwrap();
        assert_eq!(preview.selected_row_count, 105);
        assert_eq!(preview.preview_rows.len(), MAX_ACCOUNTING_PREVIEW_ROWS);
        assert!(preview.preview_truncated);
        assert_eq!(preview.possible_total_row_count, 1);
        assert_eq!(preview.possible_total_rows[0].worksheet_row, 106);
        assert!(preview.preview_rows[0].cells.iter().any(|cell| cell.cell == "B2" && cell.value == "100"));
        assert!(preview.confirmed_mappings.iter().any(|mapping| mapping.field_id == "accounting.gross_sales"));
    }

    #[test]
    fn accounting_gross_profit_uses_confirmed_net_sales_and_cogs_source_columns() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "gross-profit.csv".into(),
            b"Net Sales,COGS\n1000,650\n500,325\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 1, "accounting.net_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.cost_of_goods_sold").unwrap();

        let preview = store.preview_accounting_range(
            &id, "CSV data", "accounting.gross_profit", 2, 3,
        ).unwrap();
        assert_eq!(preview.selected_row_count, 2);
        assert_eq!(preview.confirmed_mappings.len(), 2);
        assert!(preview.preview_rows.iter().any(|row|
            row.cells.iter().any(|cell| cell.cell == "A2" && cell.value == "1000")
        ));

        let result = store.calculate_accounting_formula(
            &id, "CSV data", "accounting.gross_profit", "PKR", 2, 3,
        ).unwrap();
        assert_eq!(result.formula.output_value.as_deref(), Some("525"));
        assert_eq!(result.formula.output_unit, "currency");
        assert_eq!(result.formula.currency.as_deref(), Some("PKR"));
        assert_eq!(result.source_rows_included, 2);
        assert_eq!(result.source_cells.len(), 4);
        assert!(result.source_cells.iter().any(|cell|
            cell.field_id == "accounting.net_sales" && cell.cell == "A2" && cell.value == "1000"
        ));
        assert!(result.source_cells.iter().any(|cell|
            cell.field_id == "accounting.cost_of_goods_sold" && cell.cell == "B3" && cell.value == "325"
        ));
    }

    #[test]
    fn accounting_gross_margin_is_source_bound_and_preserves_negative_results() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "gross-margin.csv".into(),
            b"Net Sales,Cost of Goods Sold\n100,125\n200,150\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 1, "accounting.net_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.cost_of_goods_sold").unwrap();

        let result = store.calculate_accounting_formula(
            &id, "CSV data", "accounting.gross_margin", "PKR", 2, 3,
        ).unwrap();
        assert_eq!(result.formula.output_value.as_deref(), Some("8.333333"));
        assert_eq!(result.formula.output_unit, "percent");
        assert!(result.formula.currency.is_none());
        assert_eq!(result.source_rows_included, 2);
        assert_eq!(result.source_cells.len(), 4);
        assert!(result.formula.trace.iter().any(|line|
            line.contains("Source mapping: accounting.cost_of_goods_sold")
        ));

        let negative_analysis = analyze_csv(
            "negative-margin.csv".into(),
            b"Net Sales,COGS\n100,125\n",
        ).unwrap();
        let negative_id = store.create(&negative_analysis).unwrap();
        store.confirm_accounting_mapping(&negative_id, "CSV data", 1, "accounting.net_sales").unwrap();
        store.confirm_accounting_mapping(&negative_id, "CSV data", 2, "accounting.cost_of_goods_sold").unwrap();
        let negative = store.calculate_accounting_formula(
            &negative_id, "CSV data", "accounting.gross_margin", "PKR", 2, 2,
        ).unwrap();
        assert_eq!(negative.formula.output_value.as_deref(), Some("-25"));
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
    fn accounting_total_label_detection_covers_common_layouts_and_localized_labels() {
        let positive = [
            "Total",
            "Subtotal:",
            "Sub Total",
            "Grand Total (PKR)",            "Overall Total",
            "Net Total",
            "Total Sales",
            "Total Revenue",
            "Total Amount",
            "Invoice Total",
            "Q1 Subtotal",
            "FY 2026 Grand Total",
            "Grand Total PKR",
            "Total general",
            "Total de ventas",
            "Total ingresos",
            "Sous-total",
            "Total des ventes",
            "Gesamtsumme",
            "Zwischensumme",
            "Totale generale",
            "Total geral",
            "الإجمالي العام",
            "المجموع الكلي",
            "المجموع الفرعي",
            "مجموعی کل",
            "ذیلی کل",
            "کل فروخت",
            "جمع کل",
            "Total de gastos",
            "Total de compras",
            "Importe total",
            "Suma total",
            "Montant total",
            "Montant dû",
            "Gesamtbetrag",
            "Summe",
            "Totale complessivo",
            "Importo totale",
            "Valor total",
            "Soma total",
            "إجمالي المبيعات",
            "إجمالي الإيرادات",
            "إجمالي المبلغ",
            "مجموعی فروخت",
            "مجموعی آمدنی",
            "کل رقم",
        ];
        for label in positive {
            assert!(is_accounting_total_label(label), "expected total label: {label}");
        }
    }

    #[test]
    fn accounting_total_label_normalization_handles_arabic_script_diacritics_and_variants() {
        let positive = [
            "إِجْمَالِيّ الْمَبِيعَات",
            "مجموعی فروخت",
            "مُجْمُوعِي فَرُوخْت",
            "جمعِ کُل",
            "مبلغِ کُل",
        ];
        for label in positive {
            assert!(is_accounting_total_label(label), "expected normalized total label: {label}");
        }
    }

    #[test]
    fn accounting_total_label_detection_covers_additional_locales_without_broad_matching() {
        let positive = [
            "Eindtotaal",
            "Subtotaal",
            "Algemeen totaal",
            "Genel Toplam",
            "Ara Toplam",
            "Toplam",
            "कुल",
            "कुल योग",
            "उप-योग",
            "মোট",
            "সর্বমোট",
            "উপমোট",
            "مجموع کل",
            "مبلغ کل",
            "کل فروش",
            "کل درآمد",
            "Genel Toplam TRY",
            "FY 2026 Eindtotaal",
        ];
        for label in positive {
            assert!(is_accounting_total_label(label), "expected localized total label: {label}");
        }

        let negative = [
            "Totaal klanten",
            "Algemeen totaal klanten",
            "Genel Toplam tahmin",
            "Ara Toplam ürünler",
            "कुल ग्राहक",
            "सर्वमोटर",
            "उपयोग",
            "মোট কর্মচারী",
            "সর্বমোট পণ্য",
            "মোট কর্মচারী",
            "جمع کل مشتریان",
            "کل فروشندگان",
            "Genel Toplam Q1",
        ];
        for label in negative {
            assert!(!is_accounting_total_label(label), "unexpected localized total label: {label}");
        }
    }

    #[test]
    fn accounting_total_label_currency_qualifiers_remain_explicit_and_bounded() {
        for label in ["Grand Total TRY", "Eindtotaal CHF", "Genel Toplam CNY", "مبلغ کل BDT"] {
            assert!(is_accounting_total_label(label), "expected currency-qualified total: {label}");
        }
        for label in ["Grand Total Customers", "Eindtotaal EUR Customers", "Genel Toplam Q1"] {
            assert!(!is_accounting_total_label(label), "unexpected qualified total: {label}");
        }
    }

    #[test]
    fn accounting_total_label_normalization_handles_unusual_whitespace_and_line_breaks() {
        let positive = [
            "Grand\nTotal",
            "Grand\u{00A0}Total",
            "FY\t2026\tGrand\tTotal",
            "Grand—Total",
            "Eindtotaal\u{00A0}CHF",
            "مبلغ\u{00A0}کل",
            "سَرْبَمُوت",
        ];
        for label in positive {
            assert!(is_accounting_total_label(label), "expected unusual-layout total label: {label}");
        }

        let negative = [
            "Total\nCustomers",
            "Grand\tTotalist",
            "Overall\nTotal\nForecast",
            "Eindtotaal\u{00A0}klanten",
        ];
        for label in negative {
            assert!(!is_accounting_total_label(label), "unexpected unusual-layout total label: {label}");
        }
    }

    #[test]
    fn accounting_preview_and_calculation_block_multiline_total_labels_without_blocking_descriptive_rows() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "multiline-total.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\nCoffee,50,3,1\n\"Grand\nTotal\",150,8,3\n\"Grand\nTotal Forecast\",200,10,4\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();

        let preview = store.preview_accounting_range(
            &id, "CSV data", "accounting.net_sales", 2, 5,
        ).unwrap();
        assert_eq!(preview.possible_total_row_count, 1);
        assert_eq!(preview.possible_total_rows[0].worksheet_row, 4);
        assert_eq!(preview.possible_total_rows[0].labels, vec!["Grand\nTotal".to_string()]);
        assert!(preview.preview_rows.iter().any(|row| row.worksheet_row == 5 && !row.possible_total));

        let error = match store.calculate_accounting_formula(
            &id, "CSV data", "accounting.net_sales", "PKR", 2, 5,
        ) {
            Err(error) => error,
            Ok(_) => panic!("calculation must reject the selected range containing a multiline total row"),
        };
        assert!(error.contains("possible subtotal or total row at worksheet row 4"));
    }

    #[test]
    fn accounting_total_label_detection_avoids_broad_total_word_false_positives() {
        let negative = [
            "",
            "Total Customers",
            "Total Salesperson",
            "Total Products",
            "Total Forecast",
            "Sales Totality",
            "Grand Totalist",
            "Customer Subtotalization",
            "Grand Total Customers",
            "Grand Total Products",
            "Total Salesperson PKR",
            "Subtotal Forecast",
            "Grand Totalist PKR",
            "Total Expenses",
            "Total Employees",
            "Total Customers USD",
            "Total Forecast 2026",
            "Grand Total Products PKR",
            "Total Due Date",
            "Total Revenue Growth",
            "Sum Totality",
            "Gesamtbetrag Customers",
            "Montant total clients",
            "Importo totale articoli",
            "Valor total clientes",
            "Grand Total Q1",
        ];
        for label in negative {
            assert!(!is_accounting_total_label(label), "unexpected total label: {label}");
        }
    }

    #[test]
    fn accounting_total_detection_checks_every_cell_without_requiring_first_column() {
        let row = vec![
            "Tea".to_string(),
            "100".to_string(),
            "Grand Total (PKR)".to_string(),
            "100".to_string(),
        ];
        assert!(is_accounting_total_candidate(&row));
        assert!(!is_accounting_total_candidate(&[
            "Tea".to_string(),
            "100".to_string(),
            "Retail".to_string(),
        ]));
    }

    #[test]
    fn accounting_total_detection_handles_label_positions_sparse_rows_and_punctuation() {
        let cases = [
            vec!["Grand Total".to_string(), "".to_string(), "".to_string()],
            vec!["".to_string(), "Subtotal:".to_string(), "".to_string()],
            vec!["".to_string(), "".to_string(), "Grand Total (PKR)".to_string()],
            vec!["Tea".to_string(), "".to_string(), "  Grand   Total — PKR  ".to_string()],
            vec!["Tea".to_string(), "\tGesamtsumme\t".to_string(), "".to_string()],
        ];
        for row in cases {
            assert!(is_accounting_total_candidate(&row), "expected candidate row: {row:?}");
        }
        assert!(!is_accounting_total_candidate(&[
            "Tea".to_string(),
            "".to_string(),
            "Retail".to_string(),
        ]));
    }

    #[test]
    fn accounting_total_detection_reports_only_the_cells_that_matched() {
        let row = vec![
            "Tea".to_string(),
            "150".to_string(),
            "Grand Total (PKR)".to_string(),
            "8".to_string(),
            "Subtotal".to_string(),
        ];
        assert_eq!(
            accounting_total_labels(&row),
            vec!["Grand Total (PKR)".to_string(), "Subtotal".to_string()]
        );
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
    fn accounting_calculation_rejects_localized_total_rows_end_to_end() {
        let labels = [
            "Subtotal", "Grand Total (PKR)", "Invoice Total", "Gesamtsumme", "Zwischensumme",
            "Total de ventas", "Total ingresos", "Sous total", "Total des ventes", "Total des revenus",
            "Summe gesamt", "Totale generale", "Totale vendite", "Total geral", "Total de vendas",
            "Total de receitas", "Total de gastos", "Total de compras", "Total de despesas",
            "Total de impuestos", "Importe total", "Suma total", "Total facturado", "Montant total",
            "Montant dû", "Total TTC", "Total HT", "Somme totale", "Gesamtbetrag", "Gesamt",
            "Summe", "Gesamtergebnis", "Totale", "Totale complessivo", "Importo totale",
            "Somma totale", "Valor total", "Soma total", "Total a pagar", "Valor a pagar",
            "إجمالي المبيعات", "إجمالي الإيرادات", "إجمالي المبلغ", "إجمالي المستحق",
            "مجموعی فروخت", "مجموعی آمدنی", "کل رقم", "کل واجب الادا", "الإجمالي",
            "الإجمالي العام", "المجموع", "المجموع الكلي", "المجموع الفرعي", "مجموعی کل",
            "ذیلی کل", "کل فروخت", "کل آمدنی", "جمع کل", "جمع فرعی",
        ];
        for label in labels {
            let csv = format!(
                "Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\nCoffee,50,3,1\n{label},150,8,3\n"
            );
            let store = WorkbookSessionStore::default();
            let analysis = analyze_csv("localized-sales.csv".into(), csv.as_bytes()).unwrap();
            let id = store.create(&analysis).unwrap();
            store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
            store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
            store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();

            let preview = store.preview_accounting_range(&id, "CSV data", "accounting.net_sales", 2, 4).unwrap();
            assert_eq!(preview.possible_total_row_count, 1, "label should be surfaced in preview: {label}");
            let error = match store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, 4) {
                Err(error) => error,
                Ok(_) => panic!("a localized total row must block aggregation: {label}"),
            };
            assert!(error.contains("possible subtotal or total row"), "unexpected error for {label}: {error}");
        }
    }

    #[test]
    fn accounting_preview_and_calculation_agree_when_total_label_is_in_last_column() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "last-column-total.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns,Note\nTea,100,5,2,\nCoffee,50,3,1,\n150,8,3,Grand Total (PKR)\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();

        let preview = store.preview_accounting_range(&id, "CSV data", "accounting.net_sales", 2, 4).unwrap();
        assert_eq!(preview.possible_total_row_count, 1);
        assert_eq!(preview.possible_total_rows[0].worksheet_row, 4);
        assert_eq!(preview.possible_total_rows[0].labels, vec!["Grand Total (PKR)".to_string()]);

        let error = match store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, 4) {
            Err(error) => error,
            Ok(_) => panic!("calculation must reject the same row preview flagged"),
        };
        assert!(error.contains("row 4"));
    }

    #[test]
    fn accounting_preview_and_calculation_reject_total_labels_across_sparse_column_positions() {
        let cases = [
            (0usize, "Gesamtsumme"),
            (4usize, "إجمالي المبيعات"),
            (7usize, "مجموعی فروخت"),
        ];
        for (label_column, label) in cases {
            let headers = ["Product", "Gross Sales", "Discounts", "Sales Returns", "Note 1", "Note 2", "Note 3", "Note 4"];
            let mut total = vec![String::new(); headers.len()];
            total[0] = if label_column == 0 { label.to_string() } else { "Products".to_string() };
            total[1] = "150".to_string();
            total[2] = "8".to_string();
            total[3] = "3".to_string();
            total[label_column] = label.to_string();
            let mut csv = format!("{}\n", headers.join(","));
            csv.push_str("Tea,100,5,2,,,,\n");
            csv.push_str("Coffee,50,3,1,,,,\n");
            csv.push_str(&format!("{}\n", total.join(",")));

            let store = WorkbookSessionStore::default();
            let analysis = analyze_csv("sparse-total-layout.csv".into(), csv.as_bytes()).unwrap();
            let id = store.create(&analysis).unwrap();
            store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
            store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
            store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();

            let preview = store.preview_accounting_range(&id, "CSV data", "accounting.net_sales", 2, 4).unwrap();
            assert_eq!(preview.possible_total_row_count, 1, "label column {label_column} must be detected");
            assert_eq!(preview.possible_total_rows[0].worksheet_row, 4);
            assert_eq!(preview.possible_total_rows[0].labels, vec![label.to_string()]);
            let error = match store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, 4) {
                Ok(_) => panic!("every candidate column position must block aggregation"),
                Err(error) => error,
            };
            assert!(error.contains("row 4"), "unexpected error for {label}: {error}");
        }
    }

    #[test]
    fn accounting_preview_and_calculation_ignore_descriptive_labels_across_sparse_columns() {
        let cases = [
            (0usize, "Total Expenses"),
            (4usize, "Gesamtbetrag Customers"),
            (7usize, "Montant total clients"),
        ];
        for (label_column, label) in cases {
            let headers = ["Product", "Gross Sales", "Discounts", "Sales Returns", "Note 1", "Note 2", "Note 3", "Note 4"];
            let mut descriptive = vec![String::new(); headers.len()];
            descriptive[0] = if label_column == 0 { label.to_string() } else { "Products".to_string() };
            descriptive[1] = "10".to_string();
            descriptive[2] = "1".to_string();
            descriptive[3] = "1".to_string();
            descriptive[label_column] = label.to_string();
            let mut csv = format!("{}\n", headers.join(","));
            csv.push_str("Tea,100,5,2,,,,\n");
            csv.push_str("Coffee,50,3,1,,,,\n");
            csv.push_str(&format!("{}\n", descriptive.join(",")));

            let store = WorkbookSessionStore::default();
            let analysis = analyze_csv("sparse-descriptive-layout.csv".into(), csv.as_bytes()).unwrap();
            let id = store.create(&analysis).unwrap();
            store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
            store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
            store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();

            let preview = store.preview_accounting_range(&id, "CSV data", "accounting.net_sales", 2, 4).unwrap();
            assert_eq!(preview.possible_total_row_count, 0, "descriptive label in column {label_column} must not be flagged: {label}");
            let result = store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, 4)
                .unwrap_or_else(|error| panic!("descriptive label in column {label_column} must not block calculation ({label}): {error}"));
            assert_eq!(result.formula.output_value.as_deref(), Some("147"), "unexpected result for label: {label}");
        }
    }

    #[test]
    fn accounting_preview_preserves_only_genuine_total_labels_when_row_has_mixed_evidence() {
        let headers = ["Product", "Gross Sales", "Discounts", "Sales Returns", "Note 1", "Note 2", "Note 3", "Note 4"];
        let mut candidate = vec![String::new(); headers.len()];
        candidate[0] = "Total Expenses".to_string();
        candidate[1] = "150".to_string();
        candidate[2] = "8".to_string();
        candidate[3] = "3".to_string();
        candidate[4] = "Grand Total (PKR)".to_string();
        candidate[6] = "Montant total".to_string();
        candidate[7] = "Total Customers".to_string();

        let csv = format!(
            "{}\nTea,100,5,2,,,,\nCoffee,50,3,1,,,,\n{}\n",
            headers.join(","),
            candidate.join(",")
        );
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv("mixed-total-evidence.csv".into(), csv.as_bytes()).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();

        let preview = store.preview_accounting_range(&id, "CSV data", "accounting.net_sales", 2, 4).unwrap();
        assert_eq!(preview.possible_total_row_count, 1);
        assert_eq!(preview.possible_total_rows.len(), 1);
        assert_eq!(preview.possible_total_rows[0].worksheet_row, 4);
        assert_eq!(
            preview.possible_total_rows[0].labels,
            vec!["Grand Total (PKR)".to_string(), "Montant total".to_string()],
            "preview must preserve every genuine matched label and exclude descriptive lookalikes"
        );

        let error = match store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, 4) {
            Err(error) => error,
            Ok(_) => panic!("a row containing genuine total labels must block aggregation even alongside descriptive labels"),
        };
        assert!(error.contains("row 4"), "unexpected error: {error}");
    }

    #[test]
    fn accounting_preview_tracks_multiple_total_candidates_at_exact_worksheet_rows() {
        let mut csv = String::from("Product,Gross Sales,Discounts,Sales Returns,Note 1,Note 2\n");
        csv.push_str("Tea,100,5,2,,\n");
        csv.push_str("Grand Total (PKR),100,5,2,,\n");
        csv.push_str("Coffee,50,3,1,,\n");
        csv.push_str("إجمالي الإيرادات,150,8,3,,\n");
        csv.push_str("Total Customers,10,1,1,,\n");
        csv.push_str("مجموعی فروخت,150,8,3,,\n");
        csv.push_str("Biscuits,25,2,1,,\n");

        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv("multiple-total-candidates.csv".into(), csv.as_bytes()).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();

        let preview = store.preview_accounting_range(&id, "CSV data", "accounting.net_sales", 2, 8).unwrap();
        assert_eq!(preview.selected_start_row, 2);
        assert_eq!(preview.selected_end_row, 8);
        assert_eq!(preview.possible_total_row_count, 3);
        assert_eq!(
            preview.possible_total_rows.iter().map(|candidate| candidate.worksheet_row).collect::<Vec<_>>(),
            vec![3, 5, 7],
            "all genuine candidates must retain their absolute worksheet row numbers"
        );
        assert_eq!(
            preview.possible_total_rows.iter().map(|candidate| candidate.labels.clone()).collect::<Vec<_>>(),
            vec![
                vec!["Grand Total (PKR)".to_string()],
                vec!["إجمالي الإيرادات".to_string()],
                vec!["مجموعی فروخت".to_string()],
            ],
            "localized genuine labels must be preserved while descriptive lookalikes remain excluded"
        );
        assert_eq!(
            preview.preview_rows.iter()
                .filter(|row| row.possible_total)
                .map(|row| row.worksheet_row)
                .collect::<Vec<_>>(),
            vec![3, 5, 7],
            "row-level preview flags must agree with the candidate list"
        );

        let full_range_error = match store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, 8) {
            Err(error) => error,
            Ok(_) => panic!("multiple candidate total rows must block aggregation"),
        };
        assert!(full_range_error.contains("row 3"), "unexpected first-candidate error: {full_range_error}");

        let narrowed_preview = store.preview_accounting_range(&id, "CSV data", "accounting.net_sales", 4, 8).unwrap();
        assert_eq!(narrowed_preview.possible_total_row_count, 2);
        assert_eq!(
            narrowed_preview.possible_total_rows.iter().map(|candidate| candidate.worksheet_row).collect::<Vec<_>>(),
            vec![5, 7]
        );
        let narrowed_error = match store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 4, 8) {
            Err(error) => error,
            Ok(_) => panic!("remaining candidate total rows must still block aggregation"),
        };
        assert!(narrowed_error.contains("row 5"), "unexpected narrowed-range error: {narrowed_error}");
    }

    #[test]
    fn accounting_preview_and_calculation_preserve_non_a1_rows_across_blank_rows() {
        let rows = vec![
            vec!["Product".to_string(), "Gross Sales".to_string(), "Discounts".to_string(), "Sales Returns".to_string()],
            vec!["Tea".to_string(), "100".to_string(), "5".to_string(), "2".to_string()],
            vec!["".to_string(), "".to_string(), "".to_string(), "".to_string()],
            vec!["Coffee".to_string(), "50".to_string(), "3".to_string(), "1".to_string()],
            vec!["Grand Total (PKR)".to_string(), "150".to_string(), "8".to_string(), "3".to_string()],
            vec!["Biscuits".to_string(), "25".to_string(), "2".to_string(), "1".to_string()],
        ];
        let sheet = analyze_matrix_with_origin("Offset".into(), rows.clone(), 0, 4, 3).unwrap();
        assert_eq!(sheet.accounting_field_suggestions[0].header_cell, "D5");
        assert_eq!(sheet.accounting_field_suggestions[0].data_range, "D6:D10");
        let source_sha256 = format!("{:x}", Sha256::digest(b"non-a1-source"));
        let analysis = WorkbookAnalysis {
            session_id: String::new(),
            file_name: "non-a1-blank-rows.xlsx".into(),
            format: "XLSX".into(),
            source_sha256: source_sha256.clone(),
            source_byte_length: 13,
            sheets: vec![sheet],
            total_rows: rows.len(),
            total_non_empty_cells: rows.iter().flatten().filter(|cell| !cell.trim().is_empty()).count(),
            notes: Vec::new(),
            source_sheets: vec![WorkbookSourceSheet {
                name: "Offset".into(),
                rows,
                start_row: 4,
                start_column: 3,
                formula_cell_positions: Vec::new(),
            }],
        };

        let store = WorkbookSessionStore::default();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "Offset", 5, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "Offset", 6, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "Offset", 7, "accounting.sales_returns").unwrap();

        let preview = store.preview_accounting_range(&id, "Offset", "accounting.net_sales", 6, 10).unwrap();
        assert_eq!(preview.selected_start_row, 6);
        assert_eq!(preview.selected_end_row, 10);
        assert_eq!(
            preview.preview_rows.iter().map(|row| row.worksheet_row).collect::<Vec<_>>(),
            vec![6, 7, 8, 9, 10],
            "blank rows must not collapse or renumber absolute worksheet coordinates"
        );
        assert_eq!(preview.possible_total_row_count, 1);
        assert_eq!(preview.possible_total_rows[0].worksheet_row, 9);

        let total_error = match store.calculate_accounting_formula(&id, "Offset", "accounting.net_sales", "PKR", 6, 10) {
            Err(error) => error,
            Ok(_) => panic!("the total row at its absolute worksheet coordinate must block calculation"),
        };
        assert!(total_error.contains("row 9"), "unexpected total-row error: {total_error}");

        let narrowed = store.preview_accounting_range(&id, "Offset", "accounting.net_sales", 6, 8).unwrap();
        assert_eq!(narrowed.possible_total_row_count, 0);
        let calculation = store.calculate_accounting_formula(&id, "Offset", "accounting.net_sales", "PKR", 6, 8).unwrap();
        assert_eq!(calculation.formula.output_value.as_deref(), Some("139"));
        assert_eq!(calculation.source_rows_included, 2);
        assert_eq!(calculation.blank_cells_skipped, 3);
        assert!(calculation.source_cells.iter().any(|cell| cell.cell == "E6"));
        assert!(calculation.source_cells.iter().any(|cell| cell.cell == "E8"));
        assert!(calculation.source_cells.iter().all(|cell| !cell.cell.ends_with('7')));
    }

    #[test]
    fn accounting_preview_scans_past_visible_rows_and_calculation_still_rejects_late_total() {
        let store = WorkbookSessionStore::default();
        let mut csv = String::from("Product,Gross Sales,Discounts,Sales Returns,Note\n");
        for index in 0..120 {
            csv.push_str(&format!("Item {index},100,5,2,\n"));
        }
        csv.push_str("12000,600,240,Grand Total (PKR)\n");
        let analysis = analyze_csv("late-total.csv".into(), csv.as_bytes()).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();

        let preview = store.preview_accounting_range(&id, "CSV data", "accounting.net_sales", 2, 122).unwrap();
        assert_eq!(preview.preview_rows.len(), MAX_ACCOUNTING_PREVIEW_ROWS);
        assert!(preview.preview_truncated);
        assert_eq!(preview.possible_total_row_count, 1);
        assert_eq!(preview.possible_total_rows[0].worksheet_row, 122);

        let error = match store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, 122) {
            Err(error) => error,
            Ok(_) => panic!("a total row beyond the visible preview must still block calculation"),
        };
        assert!(error.contains("row 122"));
    }

    #[test]    fn accounting_preview_bounds_candidate_list_but_counts_every_candidate() {
        let store = WorkbookSessionStore::default();
        let mut csv = String::from("Product,Gross Sales,Discounts,Sales Returns\n");
        for index in 0..(MAX_ACCOUNTING_TOTAL_CANDIDATES + 5) {
            csv.push_str(&format!("Grand Total,{},1,1\n", index + 10));
        }
        let analysis = analyze_csv("many-totals.csv".into(), csv.as_bytes()).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();

        let end_row = 1 + MAX_ACCOUNTING_TOTAL_CANDIDATES + 5;
        let preview = store.preview_accounting_range(&id, "CSV data", "accounting.net_sales", 2, end_row).unwrap();
        assert_eq!(preview.possible_total_row_count, MAX_ACCOUNTING_TOTAL_CANDIDATES + 5);
        assert_eq!(preview.possible_total_rows.len(), MAX_ACCOUNTING_TOTAL_CANDIDATES);
        assert!(preview.total_candidates_truncated);
        assert_eq!(preview.possible_total_rows.first().unwrap().worksheet_row, 2);
        assert_eq!(preview.possible_total_rows.last().unwrap().worksheet_row, 1 + MAX_ACCOUNTING_TOTAL_CANDIDATES);

        let error = match store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, end_row) {
            Err(error) => error,
            Ok(_) => panic!("candidate-list truncation must never permit calculation"),
        };
        assert!(error.contains("row 2"));
    }

    #[test]
    fn accounting_preview_limits_matched_labels_per_candidate_row() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "many-labels.csv".into(),
            b"Gross Sales,Discounts,Sales Returns,Note 1,Note 2,Note 3,Note 4\n100,5,2,Grand Total,Subtotal,Grand Total (PKR),Overall Total\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 1, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.sales_returns").unwrap();

        let preview = store.preview_accounting_range(&id, "CSV data", "accounting.net_sales", 2, 2).unwrap();
        assert_eq!(preview.possible_total_row_count, 1);
        assert_eq!(preview.possible_total_rows[0].labels.len(), 3);
        assert_eq!(preview.possible_total_rows[0].labels, vec![
            "Grand Total".to_string(),
            "Subtotal".to_string(),
            "Grand Total (PKR)".to_string(),
        ]);
    }

    #[test]
    fn accounting_calculation_does_not_reject_ordinary_labels_containing_total() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "customers.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\nCoffee,50,3,1\nTotal Customers,10,1,1\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();

        let preview = store.preview_accounting_range(&id, "CSV data", "accounting.net_sales", 2, 4).unwrap();
        assert_eq!(preview.possible_total_row_count, 0);
        let result = store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, 4).unwrap();
        assert_eq!(result.formula.output_value.as_deref(), Some("147"));
    }

    #[test]
    fn accounting_preview_and_calculation_ignore_descriptive_total_lookalikes_end_to_end() {
        let labels = [
            "Total Expenses", "Total Employees", "Total Customers USD",
            "Total Forecast 2026", "Grand Total Products PKR", "Total Due Date",
            "Total Revenue Growth", "Grand Total Q1", "Gesamtbetrag Customers",
            "Montant total clients", "Importo totale articoli", "Valor total clientes",
            "Total Salesperson", "Subtotal Forecast",
        ];
        for label in labels {
            let csv = format!(
                "Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\nCoffee,50,3,1\n{label},10,1,1\n"
            );
            let store = WorkbookSessionStore::default();
            let analysis = analyze_csv("descriptive-labels.csv".into(), csv.as_bytes()).unwrap();
            let id = store.create(&analysis).unwrap();
            store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
            store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
            store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();

            let preview = store.preview_accounting_range(&id, "CSV data", "accounting.net_sales", 2, 4).unwrap();
            assert_eq!(preview.possible_total_row_count, 0, "descriptive label must not be flagged: {label}");
            let result = store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, 4)
                .unwrap_or_else(|error| panic!("descriptive label must not block calculation ({label}): {error}"));
            assert_eq!(result.formula.output_value.as_deref(), Some("147"), "unexpected result for label: {label}");
        }
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
    fn accounting_mapping_confirmation_rejects_inconsistent_session_state() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "integrity.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        {
            let mut sessions = store.sessions.lock().unwrap();
            sessions.get_mut(&id).unwrap().sheets[0].row_count += 1;
        }
        assert!(store.confirm_accounting_mapping(
            &id, "CSV data", 2, "accounting.gross_sales"
        ).is_err());

        let second_store = WorkbookSessionStore::default();
        let second_analysis = analyze_csv(
            "identity.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\n",
        ).unwrap();
        let second_id = second_store.create(&second_analysis).unwrap();
        {
            let mut sessions = second_store.sessions.lock().unwrap();
            sessions.get_mut(&second_id).unwrap().source_sheets[0].name = "Renamed source".into();
        }
        assert!(second_store.confirm_accounting_mapping(
            &second_id, "CSV data", 2, "accounting.gross_sales"
        ).is_err());
    }

    #[test]
    fn workbook_operations_reject_tampered_confirmed_mapping_metadata() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "mapping-integrity.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        {
            let mut sessions = store.sessions.lock().unwrap();
            let mapping = sessions.get_mut(&id).unwrap().confirmed_mappings.values_mut().next().unwrap();
            mapping.data_range = "Z999:Z1000".into();
        }
        assert!(store.answer(&id, "CSV data", "total gross sales").is_err());
        assert!(store.preview_accounting_range(&id, "CSV data", "accounting.net_sales", 2, 2).is_err());
        assert!(store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").is_err());

        let second_store = WorkbookSessionStore::default();
        let second_analysis = analyze_csv(
            "mapping-fingerprint.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\n",
        ).unwrap();
        let second_id = second_store.create(&second_analysis).unwrap();
        second_store.confirm_accounting_mapping(&second_id, "CSV data", 2, "accounting.gross_sales").unwrap();
        {
            let mut sessions = second_store.sessions.lock().unwrap();
            let mapping = sessions.get_mut(&second_id).unwrap().confirmed_mappings.values_mut().next().unwrap();
            mapping.source_sha256 = "0".repeat(64);
        }
        assert!(second_store.calculate_accounting_formula(
            &second_id, "CSV data", "accounting.net_sales", "PKR", 2, 2
        ).is_err());
    }

    #[test]
    fn accounting_preview_and_calculation_reject_a_session_source_mismatch() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv("sales.csv".into(), b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\n").unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();
        {
            let mut sessions = store.sessions.lock().unwrap();
            sessions.get_mut(&id).unwrap().source_sha256 = "0".repeat(64);
        }
        // Every operation that consumes this session must reject the same
        // stale source identity, not only the calculation endpoints.
        assert!(store.answer(&id, "CSV data", "total gross sales").is_err());
        assert!(store.confirm_accounting_mapping(
            &id, "CSV data", 2, "accounting.gross_sales"
        ).is_err());
        assert!(store.preview_accounting_range(&id, "CSV data", "accounting.net_sales", 2, 2).is_err());
        assert!(store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, 2).is_err());
    }


    #[test]
    fn workbook_operations_reject_inconsistent_retained_session_dimensions_and_coordinates() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "integrity.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        {
            let mut sessions = store.sessions.lock().unwrap();
            sessions.get_mut(&id).unwrap().sheets[0].row_count += 1;
        }
        assert!(store.answer(&id, "CSV data", "total gross sales").is_err());
        assert!(store.preview_accounting_range(&id, "CSV data", "accounting.net_sales", 2, 2).is_err());
        assert!(store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, 2).is_err());

        let second = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "coordinates.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\n",
        ).unwrap();
        let id = second.create(&analysis).unwrap();
        {
            let mut sessions = second.sessions.lock().unwrap();
            sessions.get_mut(&id).unwrap().source_sheets[0].formula_cell_positions.push((99, 99));
        }
        assert!(second.answer(&id, "CSV data", "total gross sales").is_err());
    }

    #[test]
    fn workbook_session_rejects_duplicate_or_mismatched_worksheet_identity() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "identity.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        {
            let mut sessions = store.sessions.lock().unwrap();
            sessions.get_mut(&id).unwrap().source_sheets[0].name = "Renamed source".into();
        }
        assert!(store.answer(&id, "CSV data", "total gross sales").is_err());
        assert!(store.preview_accounting_range(&id, "CSV data", "accounting.net_sales", 2, 2).is_err());
    }

    #[test]
    fn accounting_calculation_rejects_comma_grouped_values_without_guessing() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "sales.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,\"1,000\",0,0\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();
        let result = store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, 2);
        assert!(result.is_err());
    }

    #[test]
    fn accounting_calculation_fails_closed_when_source_evidence_limit_is_exceeded() {
        let store = WorkbookSessionStore::default();
        let mut csv = String::from("Product,Gross Sales,Discounts,Sales Returns\n");
        for index in 0..6_667 {
            csv.push_str(&format!("Item {index},1,1,1\n"));
        }
        let analysis = analyze_csv("large-sales.csv".into(), csv.as_bytes()).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();
        let result = store.calculate_accounting_formula(&id, "CSV data", "accounting.net_sales", "PKR", 2, 6_668);
        assert!(result.is_err());
        assert!(result.err().unwrap().contains("20,000-cell evidence limit"));
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
        let rows = vec![            vec!["Product".to_string(), "Gross Sales".to_string()],
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
    fn duplicate_accounting_headers_are_ambiguous_and_cannot_be_confirmed() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "duplicate-headers.csv".into(),
            b"Product,Gross Sales,Gross Sales\nTea,10,12\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        let sheet = &analysis.sheets[0];
        for source_column_index in [2, 3] {
            let suggestion = sheet.accounting_field_suggestions.iter()
                .find(|item| item.source_column_index == source_column_index).unwrap();
            assert_eq!(suggestion.status, "ambiguous");
            assert_eq!(suggestion.field_id, None);
            assert_eq!(suggestion.possible_fields, vec!["accounting.gross_sales".to_string()]);
            assert!(store.confirm_accounting_mapping(
                &id, "CSV data", source_column_index, "accounting.gross_sales"
            ).is_err());
        }
        assert!(store.sessions.lock().unwrap().get(&id).unwrap().confirmed_mappings.is_empty());
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
    fn workbook_sessions_evict_oldest_when_aggregate_source_budget_is_exceeded() {
        let store = WorkbookSessionStore::default();
        let analysis = session_test_analysis();
        let first_id = store.create(&analysis).unwrap();
        let incoming_source_bytes = serde_json::to_vec(&analysis.source_sheets).unwrap().len();
        {
            let mut sessions = store.sessions.lock().unwrap();
            sessions.get_mut(&first_id).unwrap().source_bytes =
                MAX_SESSION_SOURCE_BYTES.saturating_sub(incoming_source_bytes).saturating_add(1);
        }

        let second_id = store.create(&analysis).unwrap();
        assert_eq!(store.session_count(), 1);
        assert!(store.answer(&first_id, "CSV data", "total revenue").is_err());
        assert!(store.answer(&second_id, "CSV data", "total revenue").is_ok());
    }

    #[test]
    fn workbook_sessions_evict_oldest_when_aggregate_summary_budget_is_exceeded() {
        let store = WorkbookSessionStore::default();
        let analysis = session_test_analysis();
        let first_id = store.create(&analysis).unwrap();
        let incoming_summary_bytes = serde_json::to_vec(&analysis.sheets).unwrap().len();
        {
            let mut sessions = store.sessions.lock().unwrap();
            sessions.get_mut(&first_id).unwrap().summary_bytes =
                MAX_SESSION_SUMMARY_BYTES.saturating_sub(incoming_summary_bytes).saturating_add(1);
        }

        let second_id = store.create(&analysis).unwrap();
        assert_eq!(store.session_count(), 1);
        assert!(store.answer(&first_id, "CSV data", "total revenue").is_err());
        assert!(store.answer(&second_id, "CSV data", "total revenue").is_ok());
    }

    #[test]
    fn workbook_sessions_prune_expired_entries_before_accepting_new_sessions() {
        let store = WorkbookSessionStore::default();
        let expired_id = store.create(&session_test_analysis()).unwrap();
        {
            let mut sessions = store.sessions.lock().unwrap();
            sessions.get_mut(&expired_id).unwrap().created_at =
                Instant::now() - WORKBOOK_SESSION_TTL - Duration::from_secs(1);
        }

        let current_id = store.create(&session_test_analysis()).unwrap();
        assert_eq!(store.session_count(), 1);
        assert!(store.answer(&expired_id, "CSV data", "total revenue").is_err());
        assert!(store.answer(&current_id, "CSV data", "total revenue").is_ok());
    }

    #[test]
    fn workbook_sessions_keep_backend_snapshot_when_client_analysis_is_mutated() {
        let store = WorkbookSessionStore::default();
        let mut analysis = session_test_analysis();
        let id = store.create(&analysis).unwrap();

        // The serialized analysis is a client-facing snapshot, not an authority.
        // Mutating it after session creation must not alter backend-held answers.
        analysis.sheets[0].row_count = 999_999;
        analysis.sheets[0].columns[0].sum = Some(999_999.0);

        let answer = store.answer(&id, "CSV data", "total revenue").unwrap();
        assert!(answer.answer.contains("30.00"));
        assert!(!answer.answer.contains("999999"));
    }

    #[test]
    fn confirmed_accounting_mappings_expire_with_their_workbook_session() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "mapped-expiry.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        assert_eq!(store.sessions.lock().unwrap().get(&id).unwrap().confirmed_mappings.len(), 1);

        {
            let mut sessions = store.sessions.lock().unwrap();
            sessions.get_mut(&id).unwrap().created_at =
                Instant::now() - WORKBOOK_SESSION_TTL - Duration::from_secs(1);
        }

        assert!(store.preview_accounting_range(
            &id, "CSV data", "accounting.net_sales", 2, 2
        ).is_err());
        assert_eq!(store.session_count(), 0);
    }

    #[test]
    fn confirmed_accounting_mappings_are_evicted_with_their_workbook_session() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "mapped-eviction.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\n",
        ).unwrap();
        let first_id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(
            &first_id, "CSV data", 2, "accounting.gross_sales"
        ).unwrap();

        let mut latest_id = String::new();
        for _ in 0..MAX_WORKBOOK_SESSIONS {
            latest_id = store.create(&analysis).unwrap();
        }

        assert_eq!(store.session_count(), MAX_WORKBOOK_SESSIONS);
        assert!(store.preview_accounting_range(
            &first_id, "CSV data", "accounting.net_sales", 2, 2
        ).is_err());
        assert!(store.confirm_accounting_mapping(
            &latest_id, "CSV data", 2, "accounting.gross_sales"
        ).is_ok());
    }

    #[test]
    fn session_integrity_rejects_forged_mapping_keys_and_duplicate_assignments() {
        let analysis = analyze_csv(
            "mapping-invariants.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\n",
        ).unwrap();

        let key_store = WorkbookSessionStore::default();
        let key_id = key_store.create(&analysis).unwrap();
        key_store.confirm_accounting_mapping(
            &key_id, "CSV data", 2, "accounting.gross_sales"
        ).unwrap();
        {
            let mut sessions = key_store.sessions.lock().unwrap();
            let session = sessions.get_mut(&key_id).unwrap();
            let mapping = session.confirmed_mappings.values().next().unwrap().clone();
            session.confirmed_mappings.clear();
            session.confirmed_mappings.insert(
                "forged:mapping:key".into(), mapping
            );
        }
        assert!(key_store.answer(&key_id, "CSV data", "total gross sales").is_err());

        let field_store = WorkbookSessionStore::default();
        let field_id = field_store.create(&analysis).unwrap();
        field_store.confirm_accounting_mapping(
            &field_id, "CSV data", 2, "accounting.gross_sales"
        ).unwrap();
        {
            let mut sessions = field_store.sessions.lock().unwrap();
            let session = sessions.get_mut(&field_id).unwrap();
            let mut duplicate = session.confirmed_mappings.values().next().unwrap().clone();
            duplicate.source_column_index = 3;
            duplicate.header = "Discounts".into();
            duplicate.header_cell = "C1".into();
            duplicate.data_range = "C2:C2".into();
            let key = format!(
                "{}:{}:{}",
                duplicate.sheet_name, duplicate.source_column_index, duplicate.field_id
            );
            session.confirmed_mappings.insert(key, duplicate);
        }
        assert!(field_store.answer(&field_id, "CSV data", "total gross sales").is_err());

        let column_store = WorkbookSessionStore::default();
        let column_id = column_store.create(&analysis).unwrap();
        column_store.confirm_accounting_mapping(
            &column_id, "CSV data", 2, "accounting.gross_sales"
        ).unwrap();
        {
            let mut sessions = column_store.sessions.lock().unwrap();
            let session = sessions.get_mut(&column_id).unwrap();
            let mut duplicate = session.confirmed_mappings.values().next().unwrap().clone();
            duplicate.field_id = "accounting.discounts".into();
            let key = format!(
                "{}:{}:{}",
                duplicate.sheet_name, duplicate.source_column_index, duplicate.field_id
            );
            session.confirmed_mappings.insert(key, duplicate);
        }
        assert!(column_store.answer(&column_id, "CSV data", "total gross sales").is_err());
    }

    #[test]
    fn failed_mapping_confirmation_preserves_existing_confirmed_state() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "mapping-atomicity.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(
            &id, "CSV data", 2, "accounting.gross_sales"
        ).unwrap();

        assert!(store.confirm_accounting_mapping(
            &id, "CSV data", 3, "accounting.gross_sales"
        ).is_err());

        let sessions = store.sessions.lock().unwrap();
        let mappings = &sessions.get(&id).unwrap().confirmed_mappings;
        assert_eq!(mappings.len(), 1);
        assert!(mappings.values().any(|mapping|
            mapping.field_id == "accounting.gross_sales"
                && mapping.source_column_index == 2
                && mapping.header == "Gross Sales"
        ));
        validate_session_integrity(sessions.get(&id).unwrap()).unwrap();
    }

    #[test]
    fn concurrent_duplicate_mapping_confirmations_accept_exactly_one_assignment() {
        let store = std::sync::Arc::new(WorkbookSessionStore::default());
        let analysis = analyze_csv(
            "concurrent-duplicate.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let mut workers = Vec::new();

        for _ in 0..8 {
            let store = std::sync::Arc::clone(&store);
            let barrier = std::sync::Arc::clone(&barrier);
            let id = id.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                store.confirm_accounting_mapping(
                    &id, "CSV data", 2, "accounting.gross_sales"
                ).is_ok()
            }));
        }

        let successes = workers.into_iter()
            .map(|worker| worker.join().expect("mapping confirmation worker should not panic"))
            .filter(|success| *success)
            .count();
        assert_eq!(successes, 1);

        let sessions = store.sessions.lock().unwrap();
        let session = sessions.get(&id).unwrap();
        assert_eq!(session.confirmed_mappings.len(), 1);
        validate_session_integrity(session).unwrap();
    }

    #[test]
    fn concurrent_distinct_mapping_confirmations_produce_one_consistent_calculation() {
        let store = std::sync::Arc::new(WorkbookSessionStore::default());
        let analysis = analyze_csv(
            "concurrent-mappings.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\nCoffee,50,3,1\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let assignments = [
            (2usize, "accounting.gross_sales"),
            (3usize, "accounting.discounts"),
            (4usize, "accounting.sales_returns"),
        ];
        let mut workers = Vec::new();

        for (column, field) in assignments {
            let store = std::sync::Arc::clone(&store);
            let barrier = std::sync::Arc::clone(&barrier);
            let id = id.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                store.confirm_accounting_mapping(&id, "CSV data", column, field)
            }));
        }

        for worker in workers {
            worker.join()
                .expect("mapping confirmation worker should not panic")
                .expect("distinct valid mapping confirmations should all succeed");
        }

        let result = store.calculate_accounting_formula(
            &id, "CSV data", "accounting.net_sales", "PKR", 2, 3
        ).unwrap();
        assert_eq!(result.formula.status, "calculated");
        assert_eq!(result.formula.output_value.as_deref(), Some("139"));
        assert_eq!(result.confirmed_mappings.len(), 3);
        assert_eq!(result.source_cells.len(), 6);

        let sessions = store.sessions.lock().unwrap();
        validate_session_integrity(sessions.get(&id).unwrap()).unwrap();
    }

    #[test]
    fn concurrent_workbook_sessions_never_mix_accounting_source_evidence() {
        let store = std::sync::Arc::new(WorkbookSessionStore::default());
        let first_analysis = analyze_csv(
            "session-alpha.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\n",
        ).unwrap();
        let second_analysis = analyze_csv(
            "session-beta.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,250,10,5\n",
        ).unwrap();
        let first_fingerprint = first_analysis.source_sha256.clone();
        let second_fingerprint = second_analysis.source_sha256.clone();
        assert_ne!(first_fingerprint, second_fingerprint);

        let first_id = store.create(&first_analysis).unwrap();
        let second_id = store.create(&second_analysis).unwrap();
        for id in [&first_id, &second_id] {
            store.confirm_accounting_mapping(id, "CSV data", 2, "accounting.gross_sales").unwrap();
            store.confirm_accounting_mapping(id, "CSV data", 3, "accounting.discounts").unwrap();
            store.confirm_accounting_mapping(id, "CSV data", 4, "accounting.sales_returns").unwrap();
        }

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let requests = [
            (first_id.clone(), "session-alpha.csv", first_fingerprint.clone(), "93", "100", "5", "2"),
            (second_id.clone(), "session-beta.csv", second_fingerprint.clone(), "235", "250", "10", "5"),
        ];
        let workers = requests.into_iter().map(
            |(id, file_name, fingerprint, expected_total, gross_sales, discounts, returns)| {
                let store = std::sync::Arc::clone(&store);
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let preview = store.preview_accounting_range(
                        &id, "CSV data", "accounting.net_sales", 2, 2,
                    ).unwrap();
                    let result = store.calculate_accounting_formula(
                        &id, "CSV data", "accounting.net_sales", "PKR", 2, 2,
                    ).unwrap();
                    (preview, result, file_name, fingerprint, expected_total, gross_sales, discounts, returns)
                })
            }
        ).collect::<Vec<_>>();

        let results = workers.into_iter()
            .map(|worker| worker.join().expect("workbook session worker should not panic"))
            .collect::<Vec<_>>();
        assert_eq!(results.len(), 2);

        for (preview, result, file_name, fingerprint, expected_total, gross_sales, discounts, returns) in &results {
            assert_eq!(preview.source_sha256, *fingerprint);
            assert_eq!(preview.formula_id, "accounting.net_sales");
            assert_eq!(preview.selected_start_row, 2);
            assert_eq!(preview.selected_end_row, 2);
            assert_eq!(preview.selected_row_count, 1);
            assert_eq!(result.file_name, *file_name);
            assert_eq!(result.sheet_name, "CSV data");
            assert_eq!(result.source_sha256, *fingerprint);
            assert_eq!(result.formula.formula_id, "accounting.net_sales");
            assert_eq!(result.formula.output_value.as_deref(), Some(*expected_total));
            assert_eq!(result.formula.currency.as_deref(), Some("PKR"));
            assert_eq!(result.confirmed_mappings.len(), 3);
            assert_eq!(result.source_cells.len(), 3);
            assert!(result.confirmed_mappings.iter().all(|mapping|
                mapping.source_sha256 == *fingerprint
                    && mapping.sheet_name == "CSV data"
            ));

            let mut evidence = result.source_cells.iter()
                .map(|cell| (cell.field_id.as_str(), cell.cell.as_str(), cell.value.as_str()))
                .collect::<Vec<_>>();
            evidence.sort_unstable();
            assert_eq!(evidence, vec![
                ("accounting.discounts", "C2", *discounts),
                ("accounting.gross_sales", "B2", *gross_sales),
                ("accounting.sales_returns", "D2", *returns),
            ]);
        }

        let first_result = results.iter().find(|(_, result, file_name, ..)|
            *file_name == "session-alpha.csv"
        ).unwrap();
        let second_result = results.iter().find(|(_, result, file_name, ..)|
            *file_name == "session-beta.csv"
        ).unwrap();
        assert_eq!(first_result.1.formula.output_value.as_deref(), Some("93"));
        assert_eq!(second_result.1.formula.output_value.as_deref(), Some("235"));
        assert_ne!(first_result.1.source_sha256, second_result.1.source_sha256);
        assert!(first_result.1.source_cells.iter().all(|cell|
            !second_result.1.source_cells.iter().any(|other|
                other.field_id == cell.field_id && other.value == cell.value
            )
        ));

        let sessions = store.sessions.lock().unwrap();
        validate_session_integrity(sessions.get(&first_id).unwrap()).unwrap();
        validate_session_integrity(sessions.get(&second_id).unwrap()).unwrap();
    }

    #[test]
    fn expired_session_cannot_be_revived_by_reimporting_identical_source() {
        let store = WorkbookSessionStore::default();
        let source = b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\n";
        let original_analysis = analyze_csv("reimported-sales.csv".into(), source).unwrap();
        let fingerprint = original_analysis.source_sha256.clone();
        let original_id = store.create(&original_analysis).unwrap();
        store.confirm_accounting_mapping(
            &original_id, "CSV data", 2, "accounting.gross_sales"
        ).unwrap();
        store.confirm_accounting_mapping(
            &original_id, "CSV data", 3, "accounting.discounts"
        ).unwrap();
        store.confirm_accounting_mapping(
            &original_id, "CSV data", 4, "accounting.sales_returns"
        ).unwrap();

        {
            let mut sessions = store.sessions.lock().unwrap();
            sessions.get_mut(&original_id).unwrap().created_at =
                Instant::now() - WORKBOOK_SESSION_TTL - Duration::from_secs(1);
        }

        let reimported_analysis = analyze_csv("reimported-sales.csv".into(), source).unwrap();
        assert_eq!(reimported_analysis.source_sha256, fingerprint);
        let replacement_id = store.create(&reimported_analysis).unwrap();
        assert_ne!(replacement_id, original_id);

        // Matching filename and content hash do not revive the expired session
        // or transfer its previously confirmed user mappings.
        assert!(store.preview_accounting_range(
            &original_id, "CSV data", "accounting.net_sales", 2, 2
        ).is_err());
        assert!(store.calculate_accounting_formula(
            &original_id, "CSV data", "accounting.net_sales", "PKR", 2, 2
        ).is_err());
        assert!(store.preview_accounting_range(
            &replacement_id, "CSV data", "accounting.net_sales", 2, 2
        ).is_err());
        assert!(store.calculate_accounting_formula(
            &replacement_id, "CSV data", "accounting.net_sales", "PKR", 2, 2
        ).is_err());
        assert!(store.sessions.lock().unwrap()
            .get(&replacement_id).unwrap().confirmed_mappings.is_empty());

        store.confirm_accounting_mapping(
            &replacement_id, "CSV data", 2, "accounting.gross_sales"
        ).unwrap();
        store.confirm_accounting_mapping(
            &replacement_id, "CSV data", 3, "accounting.discounts"
        ).unwrap();
        store.confirm_accounting_mapping(
            &replacement_id, "CSV data", 4, "accounting.sales_returns"
        ).unwrap();
        let result = store.calculate_accounting_formula(
            &replacement_id, "CSV data", "accounting.net_sales", "PKR", 2, 2
        ).unwrap();
        assert_eq!(result.source_sha256, fingerprint);
        assert_eq!(result.formula.output_value.as_deref(), Some("93"));
        assert_eq!(result.source_cells.len(), 3);
    }

    #[test]
    fn evicted_session_cannot_be_rebound_to_a_new_session_with_same_source() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "evicted-reimport.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\n",
        ).unwrap();
        let fingerprint = analysis.source_sha256.clone();
        let original_id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(
            &original_id, "CSV data", 2, "accounting.gross_sales"
        ).unwrap();

        for _ in 0..MAX_WORKBOOK_SESSIONS {
            store.create(&analysis).unwrap();
        }
        assert_eq!(store.session_count(), MAX_WORKBOOK_SESSIONS);
        assert!(store.preview_accounting_range(
            &original_id, "CSV data", "accounting.net_sales", 2, 2
        ).is_err());

        let replacement_id = store.create(&analysis).unwrap();
        assert_ne!(replacement_id, original_id);
        assert_eq!(
            store.sessions.lock().unwrap()
                .get(&replacement_id).unwrap().source_sha256,
            fingerprint
        );
        assert!(store.sessions.lock().unwrap()
            .get(&replacement_id).unwrap().confirmed_mappings.is_empty());
        assert!(store.preview_accounting_range(
            &replacement_id, "CSV data", "accounting.net_sales", 2, 2
        ).is_err());
        assert!(store.calculate_accounting_formula(
            &replacement_id, "CSV data", "accounting.net_sales", "PKR", 2, 2
        ).is_err());
    }

    #[test]
    fn concurrent_accounting_formulas_keep_their_own_inputs_and_source_evidence() {
        let store = std::sync::Arc::new(WorkbookSessionStore::default());
        let analysis = analyze_csv(
            "concurrent-formula-evidence.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns,Net Sales,Cost of Goods Sold\nTea,100,5,2,93,60\nCoffee,50,3,1,46,30\n",
        ).unwrap();
        let expected_fingerprint = analysis.source_sha256.clone();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 5, "accounting.net_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 6, "accounting.cost_of_goods_sold").unwrap();

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let requests = [
            ("accounting.net_sales", "PKR"),
            ("accounting.gross_profit", "PKR"),
            ("accounting.gross_margin", "PKR"),
        ];
        let workers = requests.into_iter().map(|(formula_id, currency)| {
            let store = std::sync::Arc::clone(&store);
            let barrier = std::sync::Arc::clone(&barrier);
            let id = id.clone();
            let formula_id = formula_id.to_string();
            let currency = currency.to_string();
            std::thread::spawn(move || {
                barrier.wait();
                let preview = store.preview_accounting_range(
                    &id, "CSV data", &formula_id, 2, 2,
                ).unwrap();
                let result = store.calculate_accounting_formula(
                    &id, "CSV data", &formula_id, &currency, 2, 2,
                ).unwrap();
                (preview, result)
            })
        }).collect::<Vec<_>>();

        let results = workers.into_iter()
            .map(|worker| worker.join().expect("formula worker should not panic"))
            .map(|(preview, result)| (preview, result))
            .collect::<Vec<_>>();
        assert_eq!(results.len(), 3);

        for (preview, result) in &results {
            assert_eq!(preview.source_sha256, expected_fingerprint);
            assert_eq!(preview.selected_start_row, 2);
            assert_eq!(preview.selected_end_row, 2);
            assert_eq!(preview.selected_row_count, 1);
            assert_eq!(result.source_sha256, expected_fingerprint);
            assert_eq!(result.sheet_name, "CSV data");
            assert_eq!(result.selected_start_row, 2);
            assert_eq!(result.selected_end_row, 2);
            assert_eq!(result.source_rows_included, 1);
            assert!(result.confirmed_mappings.iter().all(|mapping|
                mapping.source_sha256 == expected_fingerprint
                    && mapping.sheet_name == "CSV data"
            ));
        }

        let net_sales = results.iter().find(|(_, result)|
            result.formula.formula_id == "accounting.net_sales"
        ).unwrap();
        assert_eq!(net_sales.1.formula.formula_version, 1);
        assert_eq!(net_sales.1.formula.output_value.as_deref(), Some("93"));
        assert_eq!(net_sales.1.formula.output_unit, "currency");
        assert_eq!(net_sales.1.formula.currency.as_deref(), Some("PKR"));
        assert_eq!(net_sales.1.confirmed_mappings.len(), 3);
        assert_eq!(net_sales.1.source_cells.len(), 3);
        assert!(net_sales.1.source_cells.iter().all(|cell|
            ["accounting.gross_sales", "accounting.discounts", "accounting.sales_returns"]
                .contains(&cell.field_id.as_str())
                && cell.cell.ends_with('2')
        ));
        assert!(net_sales.1.source_cells.iter().any(|cell|
            cell.field_id == "accounting.gross_sales" && cell.cell == "B2" && cell.value == "100"
        ));

        let gross_profit = results.iter().find(|(_, result)|
            result.formula.formula_id == "accounting.gross_profit"
        ).unwrap();
        assert_eq!(gross_profit.1.formula.formula_version, 1);
        assert_eq!(gross_profit.1.formula.output_value.as_deref(), Some("33"));
        assert_eq!(gross_profit.1.formula.output_unit, "currency");
        assert_eq!(gross_profit.1.formula.currency.as_deref(), Some("PKR"));
        assert_eq!(gross_profit.1.confirmed_mappings.len(), 2);
        assert_eq!(gross_profit.1.source_cells.len(), 2);
        assert!(gross_profit.1.source_cells.iter().all(|cell|
            ["accounting.net_sales", "accounting.cost_of_goods_sold"]
                .contains(&cell.field_id.as_str())
        ));
        assert!(gross_profit.1.source_cells.iter().any(|cell|
            cell.field_id == "accounting.net_sales" && cell.cell == "E2" && cell.value == "93"
        ));

        let gross_margin = results.iter().find(|(_, result)|
            result.formula.formula_id == "accounting.gross_margin"
        ).unwrap();
        assert_eq!(gross_margin.1.formula.formula_version, 1);
        assert_eq!(gross_margin.1.formula.output_value.as_deref(), Some("35.483871"));
        assert_eq!(gross_margin.1.formula.output_unit, "percent");
        assert!(gross_margin.1.formula.currency.is_none());
        assert_eq!(gross_margin.1.confirmed_mappings.len(), 2);
        assert_eq!(gross_margin.1.source_cells.len(), 2);
        assert!(gross_margin.1.source_cells.iter().all(|cell|
            ["accounting.net_sales", "accounting.cost_of_goods_sold"]
                .contains(&cell.field_id.as_str())
        ));
        assert!(gross_margin.1.source_cells.iter().any(|cell|
            cell.field_id == "accounting.cost_of_goods_sold" && cell.cell == "F2" && cell.value == "60"
        ));

        let sessions = store.sessions.lock().unwrap();
        validate_session_integrity(sessions.get(&id).unwrap()).unwrap();
    }

    #[test]
    fn concurrent_accounting_calculations_return_consistent_complete_evidence() {
        let store = std::sync::Arc::new(WorkbookSessionStore::default());
        let analysis = analyze_csv(
            "concurrent-calculations.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\nCoffee,50,3,1\n",
        ).unwrap();
        let expected_fingerprint = analysis.source_sha256.clone();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(6));
        let mut workers = Vec::new();
        for _ in 0..6 {
            let store = std::sync::Arc::clone(&store);
            let barrier = std::sync::Arc::clone(&barrier);
            let id = id.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                store.calculate_accounting_formula(
                    &id, "CSV data", "accounting.net_sales", "PKR", 2, 3
                ).unwrap()
            }));
        }

        let results = workers.into_iter()
            .map(|worker| worker.join().expect("calculation worker should not panic"))
            .collect::<Vec<_>>();
        for result in &results {
            assert_eq!(result.source_sha256, expected_fingerprint);
            assert_eq!(result.formula.status, "calculated");
            assert_eq!(result.formula.output_value.as_deref(), Some("139"));
            assert_eq!(result.confirmed_mappings.len(), 3);
            assert_eq!(result.source_cells.len(), 6);
            assert_eq!(result.source_rows_included, 2);
            assert_eq!(result.selected_start_row, 2);
            assert_eq!(result.selected_end_row, 3);
            assert!(result.formula.trace.iter().any(|line|
                line.contains("Source cells retained in evidence: 6")
            ));
        }

        let first = &results[0];
        for result in results.iter().skip(1) {
            assert_eq!(result.source_sha256, first.source_sha256);
            assert_eq!(result.formula.output_value, first.formula.output_value);
            let mapping_signature = |items: &[ConfirmedAccountingMapping]| items.iter().map(|item| (
                item.field_id.clone(), item.sheet_name.clone(), item.header.clone(),
                item.source_column_index, item.header_cell.clone(), item.data_range.clone(),
                item.source_sha256.clone(),
            )).collect::<Vec<_>>();
            let cell_signature = |items: &[AccountingSourceCell]| items.iter().map(|item| (
                item.field_id.clone(), item.cell.clone(), item.value.clone(),
            )).collect::<Vec<_>>();
            assert_eq!(mapping_signature(&result.confirmed_mappings), mapping_signature(&first.confirmed_mappings));
            assert_eq!(cell_signature(&result.source_cells), cell_signature(&first.source_cells));
        }
        let sessions = store.sessions.lock().unwrap();
        validate_session_integrity(sessions.get(&id).unwrap()).unwrap();
    }

    #[test]
    fn completed_accounting_calculation_evidence_survives_session_eviction() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "immutable-calculation.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\nCoffee,50,3,1\n",
        ).unwrap();
        let expected_fingerprint = analysis.source_sha256.clone();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();

        let calculation = store.calculate_accounting_formula(
            &id, "CSV data", "accounting.net_sales", "PKR", 2, 3
        ).unwrap();
        let expected_cells = calculation.source_cells.clone();
        let expected_mappings = calculation.confirmed_mappings.clone();
        let expected_output = calculation.formula.output_value.clone();

        // Fill the bounded store until the original session is evicted.
        for _ in 0..MAX_WORKBOOK_SESSIONS {
            store.create(&analysis).unwrap();
        }
        assert!(store.answer(&id, "CSV data", "total revenue").is_err());
        assert_eq!(store.session_count(), MAX_WORKBOOK_SESSIONS);

        // The completed result owns its evidence; it must not depend on the
        // continued presence or mutability of the source session.
        assert_eq!(calculation.source_sha256, expected_fingerprint);
        let cell_signature = |items: &[AccountingSourceCell]| items.iter().map(|item| (
            item.field_id.clone(), item.cell.clone(), item.value.clone(),
        )).collect::<Vec<_>>();
        let mapping_signature = |items: &[ConfirmedAccountingMapping]| items.iter().map(|item| (
            item.field_id.clone(), item.sheet_name.clone(), item.header.clone(),
            item.source_column_index, item.header_cell.clone(), item.data_range.clone(),
            item.source_sha256.clone(),
        )).collect::<Vec<_>>();
        assert_eq!(cell_signature(&calculation.source_cells), cell_signature(&expected_cells));
        assert_eq!(mapping_signature(&calculation.confirmed_mappings), mapping_signature(&expected_mappings));
        assert_eq!(calculation.formula.output_value, expected_output);
        assert_eq!(calculation.formula.output_value.as_deref(), Some("139"));
        assert_eq!(calculation.source_cells.len(), 6);
    }

    #[test]
    fn accounting_calculation_evidence_is_bound_to_formula_mapping_and_source_identity() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "evidence-identity.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\nCoffee,50,3,1\n",
        ).unwrap();
        let expected_fingerprint = analysis.source_sha256.clone();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();

        let result = store.calculate_accounting_formula(
            &id, "CSV data", "accounting.net_sales", "PKR", 2, 3
        ).unwrap();

        assert_eq!(result.source_sha256, expected_fingerprint);
        assert_eq!(result.source_sha256.len(), 64);
        assert_eq!(result.file_name, "evidence-identity.csv");
        assert_eq!(result.sheet_name, "CSV data");
        assert_eq!(result.formula.formula_id, "accounting.net_sales");
        assert_eq!(result.formula.formula_version, 1);
        assert_eq!(result.formula.currency.as_deref(), Some("PKR"));
        assert_eq!(result.formula.output_unit, "currency");
        assert_eq!(result.formula.status, "calculated");
        assert_eq!(result.formula.output_value.as_deref(), Some("139"));

        let mapping_fields = result.confirmed_mappings.iter()
            .map(|mapping| mapping.field_id.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(mapping_fields, HashSet::from([
            "accounting.gross_sales",
            "accounting.discounts",
            "accounting.sales_returns",
        ]));
        assert!(result.confirmed_mappings.iter().all(|mapping|
            mapping.sheet_name == result.sheet_name
                && mapping.source_sha256 == result.source_sha256
        ));
        assert!(result.source_cells.iter().all(|cell|
            mapping_fields.contains(cell.field_id.as_str())
                && matches!(cell.cell.as_str(), "B2" | "B3" | "C2" | "C3" | "D2" | "D3")
        ));
        assert_eq!(result.source_cells.len(), 6);
        assert!(result.formula.trace.iter().any(|line|
            line.contains("Source mapping: accounting.gross_sales")
        ));
        assert!(result.formula.trace.iter().any(|line|
            line.contains("Source cells retained in evidence: 6")
        ));
    }

    #[test]
    fn accounting_calculation_evidence_contains_only_selected_inclusive_source_rows() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "selected-range-evidence.csv".into(),
            b"Product,Gross Sales,Discounts,Sales Returns\nTea,100,5,2\nCoffee,50,3,1\nCake,20,1,0\n",
        ).unwrap();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 4, "accounting.sales_returns").unwrap();

        // Worksheet row 3 is the Coffee record; rows 2 and 4 must not leak
        // into either the aggregate or its retained source-cell evidence.
        let result = store.calculate_accounting_formula(
            &id, "CSV data", "accounting.net_sales", "PKR", 3, 3
        ).unwrap();

        assert_eq!(result.selected_start_row, 3);
        assert_eq!(result.selected_end_row, 3);
        assert_eq!(result.source_rows_included, 1);
        assert_eq!(result.formula.output_value.as_deref(), Some("46"));
        assert_eq!(result.source_cells.len(), 3);
        let mut evidence = result.source_cells.iter()
            .map(|cell| (cell.field_id.as_str(), cell.cell.as_str(), cell.value.as_str()))
            .collect::<Vec<_>>();
        evidence.sort_unstable();
        assert_eq!(evidence, vec![
            ("accounting.discounts", "C3", "3"),
            ("accounting.gross_sales", "B3", "50"),
            ("accounting.sales_returns", "D3", "1"),
        ]);
        assert_eq!(result.blank_cells_skipped, 0);
        assert_eq!(result.cached_formula_cells_used, 0);
    }

    #[test]
    fn gross_profit_calculation_retains_exact_formula_and_source_evidence() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "gross-profit-evidence.csv".into(),
            b"Product,Net Sales,COGS\nTea,1000,650\nCoffee,500,325\nCake,200,125\n",
        ).unwrap();
        let expected_fingerprint = analysis.source_sha256.clone();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.net_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.cost_of_goods_sold").unwrap();

        let preview = store.preview_accounting_range(
            &id, "CSV data", "accounting.gross_profit", 3, 3,
        ).unwrap();
        assert_eq!(preview.source_sha256, expected_fingerprint);
        assert_eq!(preview.formula_id, "accounting.gross_profit");
        assert_eq!(preview.selected_start_row, 3);
        assert_eq!(preview.selected_end_row, 3);
        assert_eq!(preview.selected_row_count, 1);
        assert_eq!(preview.confirmed_mappings.len(), 2);
        assert!(preview.preview_rows.iter().any(|row|
            row.worksheet_row == 3
                && row.cells.iter().any(|cell| cell.cell == "B3" && cell.value == "500")
                && row.cells.iter().any(|cell| cell.cell == "C3" && cell.value == "325")
        ));

        let result = store.calculate_accounting_formula(
            &id, "CSV data", "accounting.gross_profit", "PKR", 3, 3,
        ).unwrap();
        assert_eq!(result.source_sha256, expected_fingerprint);
        assert_eq!(result.file_name, "gross-profit-evidence.csv");
        assert_eq!(result.sheet_name, "CSV data");
        assert_eq!(result.formula.formula_id, "accounting.gross_profit");
        assert_eq!(result.formula.formula_version, 1);
        assert_eq!(result.formula.status, "calculated");
        assert_eq!(result.formula.output_value.as_deref(), Some("175"));
        assert_eq!(result.formula.output_unit, "currency");
        assert_eq!(result.formula.currency.as_deref(), Some("PKR"));
        assert_eq!(result.selected_start_row, 3);
        assert_eq!(result.selected_end_row, 3);
        assert_eq!(result.source_rows_included, 1);
        assert_eq!(result.confirmed_mappings.len(), 2);
        assert!(result.confirmed_mappings.iter().all(|mapping|
            mapping.source_sha256 == result.source_sha256
                && mapping.sheet_name == result.sheet_name
        ));

        let mut evidence = result.source_cells.iter()
            .map(|cell| (cell.field_id.as_str(), cell.cell.as_str(), cell.value.as_str()))
            .collect::<Vec<_>>();
        evidence.sort_unstable();
        assert_eq!(evidence, vec![
            ("accounting.cost_of_goods_sold", "C3", "325"),
            ("accounting.net_sales", "B3", "500"),
        ]);
        assert!(result.formula.trace.iter().any(|line|
            line.contains("Formula: accounting.gross_profit v1")
        ));
        assert!(result.formula.trace.iter().any(|line|
            line.contains("Source mapping: accounting.net_sales")
        ));
        assert!(result.formula.trace.iter().any(|line|
            line.contains("Source mapping: accounting.cost_of_goods_sold")
        ));
        assert!(result.formula.trace.iter().any(|line|
            line.contains("Source cells retained in evidence: 2")
        ));
    }

    #[test]
    fn gross_margin_calculation_retains_source_evidence_and_percentage_semantics() {
        let store = WorkbookSessionStore::default();
        let analysis = analyze_csv(
            "gross-margin-evidence.csv".into(),
            b"Product,Net Sales,Cost of Goods Sold\nTea,100,60\nCoffee,200,150\nCake,300,225\n",
        ).unwrap();
        let expected_fingerprint = analysis.source_sha256.clone();
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 2, "accounting.net_sales").unwrap();
        store.confirm_accounting_mapping(&id, "CSV data", 3, "accounting.cost_of_goods_sold").unwrap();

        let preview = store.preview_accounting_range(
            &id, "CSV data", "accounting.gross_margin", 4, 4,
        ).unwrap();
        assert_eq!(preview.source_sha256, expected_fingerprint);
        assert_eq!(preview.formula_id, "accounting.gross_margin");
        assert_eq!(preview.selected_row_count, 1);
        assert_eq!(preview.confirmed_mappings.len(), 2);
        assert!(preview.preview_rows.iter().any(|row|
            row.worksheet_row == 4
                && row.cells.iter().any(|cell| cell.cell == "B4" && cell.value == "300")
                && row.cells.iter().any(|cell| cell.cell == "C4" && cell.value == "225")
        ));

        let result = store.calculate_accounting_formula(
            &id, "CSV data", "accounting.gross_margin", "PKR", 4, 4,
        ).unwrap();
        assert_eq!(result.source_sha256, expected_fingerprint);
        assert_eq!(result.formula.formula_id, "accounting.gross_margin");
        assert_eq!(result.formula.formula_version, 1);
        assert_eq!(result.formula.status, "calculated");
        assert_eq!(result.formula.output_value.as_deref(), Some("25"));
        assert_eq!(result.formula.output_unit, "percent");
        assert!(result.formula.currency.is_none());
        assert_eq!(result.selected_start_row, 4);
        assert_eq!(result.selected_end_row, 4);
        assert_eq!(result.source_rows_included, 1);
        assert_eq!(result.confirmed_mappings.len(), 2);
        let mut evidence = result.source_cells.iter()
            .map(|cell| (cell.field_id.as_str(), cell.cell.as_str(), cell.value.as_str()))
            .collect::<Vec<_>>();
        evidence.sort_unstable();
        assert_eq!(evidence, vec![
            ("accounting.cost_of_goods_sold", "C4", "225"),
            ("accounting.net_sales", "B4", "300"),
        ]);
        assert!(result.formula.trace.iter().any(|line|
            line.contains("Formula: accounting.gross_margin v1")
        ));
        assert!(result.formula.trace.iter().any(|line|
            line.contains("Calculation: ((net_sales - cost_of_goods_sold) / net_sales) * 100")
        ));
        assert!(result.formula.trace.iter().any(|line|
            line.contains("Source cells retained in evidence: 2")
        ));
    }

    #[test]
    fn concurrent_workbook_imports_preserve_session_count_and_unique_ids() {
        let store = std::sync::Arc::new(WorkbookSessionStore::default());
        let analysis = std::sync::Arc::new(session_test_analysis());
        let mut workers = Vec::new();

        for _ in 0..(MAX_WORKBOOK_SESSIONS * 4) {
            let store = std::sync::Arc::clone(&store);
            let analysis = std::sync::Arc::clone(&analysis);
            workers.push(std::thread::spawn(move || store.create(&analysis).unwrap()));
        }

        let ids = workers.into_iter()
            .map(|worker| worker.join().expect("concurrent workbook import should not panic"))
            .collect::<Vec<_>>();
        let unique_ids = ids.iter().collect::<HashSet<_>>();

        assert_eq!(ids.len(), MAX_WORKBOOK_SESSIONS * 4);
        assert_eq!(unique_ids.len(), ids.len());
        assert_eq!(store.session_count(), MAX_WORKBOOK_SESSIONS);

        // The mutex-protected insertion/eviction path must preserve both
        // session-count and aggregate serialized-memory limits under import
        // contention, not merely when sessions are created sequentially.
        {
            let sessions = store.sessions.lock().unwrap();
            let aggregate_source_bytes = sessions.values()
                .map(|session| session.source_bytes)
                .fold(0usize, usize::saturating_add);
            let aggregate_summary_bytes = sessions.values()
                .map(|session| session.summary_bytes)
                .fold(0usize, usize::saturating_add);
            assert!(aggregate_source_bytes <= MAX_SESSION_SOURCE_BYTES);
            assert!(aggregate_summary_bytes <= MAX_SESSION_SUMMARY_BYTES);
            for session in sessions.values() {
                validate_session_integrity(session).unwrap();
            }
        }

        // Worker join order is not creation order: a thread that finishes last
        // is not necessarily the session most recently inserted into the store.
        // Verify that exactly the bounded number of sessions remain usable,
        // without assuming which worker wins the final insertion race.
        let retained_count = ids.iter()
            .filter(|id| store.answer(id, "CSV data", "total revenue").is_ok())
            .count();
        assert_eq!(retained_count, MAX_WORKBOOK_SESSIONS);
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
    fn xlsx_formula_metadata_helpers_parse_coordinates_and_safe_relationship_paths() {
        assert_eq!(excel_cell_reference("B2"), Some((1, 1)));
        assert_eq!(excel_cell_reference("AA10"), Some((9, 26)));
        assert_eq!(excel_cell_reference("A0"), None);
        assert_eq!(excel_cell_reference("2B"), None);
        assert_eq!(xlsx_part_path("xl", "worksheets/sheet2.xml").unwrap(), "xl/worksheets/sheet2.xml");
        assert_eq!(xlsx_part_path("xl", "/xl/worksheets/sheet2.xml").unwrap(), "xl/worksheets/sheet2.xml");
        assert!(xlsx_part_path("xl", "../../../outside.xml").is_err());
        assert!(xlsx_part_path("xl", "worksheets\\sheet2.xml").is_err());
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
    fn xlsx_accounting_blocks_formula_cells_without_cached_values_and_warns_when_cache_is_used() {
        let store = WorkbookSessionStore::default();
        let bytes = test_xlsx_with_formula_cache(false, false);
        let analysis = analyze_xlsx("missing-cache.xlsx".into(), bytes).unwrap();
        assert!(analysis.source_sheets[0].formula_cell_positions.contains(&(1, 1)));
        let id = store.create(&analysis).unwrap();
        store.confirm_accounting_mapping(&id, "Sales", 2, "accounting.gross_sales").unwrap();
        store.confirm_accounting_mapping(&id, "Sales", 3, "accounting.discounts").unwrap();
        store.confirm_accounting_mapping(&id, "Sales", 4, "accounting.sales_returns").unwrap();
        let preview = store.preview_accounting_range(&id, "Sales", "accounting.net_sales", 2, 2).unwrap();
        assert_eq!(preview.missing_cached_formula_count, 1);
        assert!(preview.preview_rows[0].cells.iter().any(|cell| cell.cell == "B2" && cell.missing_cached_value));
        let error = match store.calculate_accounting_formula(&id, "Sales", "accounting.net_sales", "PKR", 2, 2) {
            Err(error) => error,
            Ok(_) => panic!("calculation must reject a formula cell with no cached result"),
        };
        assert!(error.contains("has no cached result"));

        let cached_store = WorkbookSessionStore::default();
        let cached_analysis = analyze_xlsx("cached.xlsx".into(), test_xlsx_with_formula_cache(false, true)).unwrap();
        let cached_id = cached_store.create(&cached_analysis).unwrap();
        cached_store.confirm_accounting_mapping(&cached_id, "Sales", 2, "accounting.gross_sales").unwrap();
        cached_store.confirm_accounting_mapping(&cached_id, "Sales", 3, "accounting.discounts").unwrap();
        cached_store.confirm_accounting_mapping(&cached_id, "Sales", 4, "accounting.sales_returns").unwrap();
        let result = cached_store.calculate_accounting_formula(&cached_id, "Sales", "accounting.net_sales", "PKR", 2, 2).unwrap();
        assert_eq!(result.formula.output_value.as_deref(), Some("2"));
        assert_eq!(result.cached_formula_cells_used, 1);
        assert!(result.formula.warnings.iter().any(|warning| warning.contains("may be stale")));
    }

    #[test]
    fn quality_checks_find_blank_duplicate_headers_mixed_types_and_duplicate_rows() {        let rows = vec![
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