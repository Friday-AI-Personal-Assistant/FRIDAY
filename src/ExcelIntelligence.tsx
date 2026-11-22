import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type ColumnAnalysis = {
  name: string;
  nonEmpty: number;
  missing: number;
  numericCount: number;
  sum: number | null;
  average: number | null;
  minimum: number | null;
  maximum: number | null;
  topValues: { value: string; count: number }[];
  frequencyComplete: boolean;
};

type DataQualityFinding = {
  severity: "warning" | "info" | string;
  category: string;
  title: string;
  description: string;
  occurrences: number;
  sampleRows: number[];
};

type AccountingFieldSuggestion = {
  header: string;
  fieldId: string | null;
  possibleFields: string[];
  status: "suggested" | "ambiguous" | "unmapped" | string;
  sourceColumnIndex: number;
  headerCell: string;
  dataRange: string;
};

type SheetAnalysis = {
  name: string;
  rowCount: number;
  columnCount: number;
  nonEmptyCells: number;
  blankCells: number;
  formulaCells: number;
  preview: string[][];
  columns: ColumnAnalysis[];
  qualityFindings: DataQualityFinding[];
  accountingFieldSuggestions: AccountingFieldSuggestion[];
};

type ConfirmedAccountingMapping = {
  fieldId: string;
  sheetName: string;
  header: string;
  sourceColumnIndex: number;
  headerCell: string;
  dataRange: string;
  sourceSha256: string;
};

type ExcelQuestionAnswer = {
  status: "answered" | "clarification" | "unsupported";
  answer: string;
  interpretation: string;
  evidence: string[];
  method: string;
};

type WorkbookAccountingCalculation = {
  sourceSha256: string;
  fileName: string;
  sheetName: string;
  formula: {
    formulaId: string;
    formulaVersion: number;
    status: string;
    outputValue: string | null;
    outputUnit: string;
    currency: string | null;
    missingInputs: string[];
    trace: string[];
    warnings: string[];
  };
  confirmedMappings: ConfirmedAccountingMapping[];
  sourceCells: { fieldId: string; cell: string; value: string }[];
  sourceRowsIncluded: number;
  selectedStartRow: number;
  selectedEndRow: number;
  blankCellsSkipped: number;
  cachedFormulaCellsUsed: number;
};

type WorkbookAccountingRangePreview = {
  sourceSha256: string;
  fileName: string;
  sheetName: string;
  formulaId: string;
  selectedStartRow: number;
  selectedEndRow: number;
  selectedRowCount: number;
  missingCachedFormulaCount: number;
  previewRows: { worksheetRow: number; possibleTotal: boolean; cells: { fieldId: string; cell: string; value: string; isFormulaCell: boolean; missingCachedValue: boolean }[] }[];
  previewTruncated: boolean;
  possibleTotalRowCount: number;
  possibleTotalRows: { worksheetRow: number; labels: string[] }[];
  totalCandidatesTruncated: boolean;
  confirmedMappings: ConfirmedAccountingMapping[];
};

type WorkbookAnalysis = {
  sessionId: string;
  fileName: string;
  format: string;
  sourceSha256: string;
  sheets: SheetAnalysis[];
  totalRows: number;
  totalNonEmptyCells: number;
  notes: string[];
};

function numberLabel(value: number | null) {
  if (value === null || !Number.isFinite(value)) return "—";
  return new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 }).format(value);
}

export default function ExcelIntelligence() {
  const [analysis, setAnalysis] = useState<WorkbookAnalysis | null>(null);
  const [selectedSheet, setSelectedSheet] = useState(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [question, setQuestion] = useState("");
  const [questionAnswer, setQuestionAnswer] = useState<ExcelQuestionAnswer | null>(null);
  const [asking, setAsking] = useState(false);
  const [confirmedMappings, setConfirmedMappings] = useState<ConfirmedAccountingMapping[]>([]);
  const [mappingBusy, setMappingBusy] = useState<number | null>(null);
  const [mappingMessage, setMappingMessage] = useState("");
  const [formulaId, setFormulaId] = useState("accounting.net_sales");
  const [currency, setCurrency] = useState("");
  const [accountingStartRow, setAccountingStartRow] = useState("2");
  const [accountingEndRow, setAccountingEndRow] = useState("2");
  const [calculationBusy, setCalculationBusy] = useState(false);
  const [calculationError, setCalculationError] = useState("");
  const [calculationResult, setCalculationResult] = useState<WorkbookAccountingCalculation | null>(null);
  const [rangePreview, setRangePreview] = useState<WorkbookAccountingRangePreview | null>(null);
  const [rangePreviewBusy, setRangePreviewBusy] = useState(false);
  const [rangePreviewError, setRangePreviewError] = useState("");

  async function chooseWorkbook() {
    setBusy(true);
    setError("");
    try {
      const result = await invoke<WorkbookAnalysis>("pick_and_analyze_workbook");
      setAnalysis(result);
      setSelectedSheet(0);
      setQuestion("");
      setQuestionAnswer(null);
      setConfirmedMappings([]);
      setAccountingStartRow("2");
      setAccountingEndRow(String(result.sheets[0]?.rowCount ?? 2));
      setMappingMessage("");
      setCalculationResult(null);
      setCalculationError("");
      setRangePreview(null);
      setRangePreviewError("");
    } catch (reason) {
      const message = String(reason);
      if (!message.toLowerCase().includes("cancel")) setError(message);
    } finally {
      setBusy(false);
    }
  }

  const sheet = analysis?.sheets[selectedSheet];

  async function confirmAccountingMapping(item: AccountingFieldSuggestion) {
    if (!analysis || !sheet || item.status !== "suggested" || !item.fieldId || mappingBusy !== null) return;
    setMappingBusy(item.sourceColumnIndex);
    setMappingMessage("");
    try {
      const mapping = await invoke<ConfirmedAccountingMapping>("confirm_excel_accounting_mapping", {
        sessionId: analysis.sessionId,
        sheetName: sheet.name,
        sourceColumnIndex: item.sourceColumnIndex,
        fieldId: item.fieldId,
      });
      if (!confirmedMappings.some(existing => existing.sheetName === mapping.sheetName)) {
        const bounds = mapping.dataRange.split(":");
        const first = Number(bounds[0]?.match(/[0-9]+$/)?.[0] ?? 0);
        const last = Number(bounds[1]?.match(/[0-9]+$/)?.[0] ?? 0);
        if (first > 0 && last >= first) {
          setAccountingStartRow(String(first));
          setAccountingEndRow(String(last));
        }
      }
      setConfirmedMappings(current => [...current.filter(existing =>
        !(existing.sheetName === mapping.sheetName && existing.sourceColumnIndex === mapping.sourceColumnIndex)
      ), mapping]);
      setRangePreview(null);
      setRangePreviewError("");
      setMappingMessage("Confirmed " + mapping.header + " as " + mapping.fieldId + ". Source: " + mapping.sheetName + "!" + mapping.headerCell + " and " + mapping.dataRange + ".");
    } catch (reason) {
      setMappingMessage(String(reason));
    } finally {
      setMappingBusy(null);
    }
  }

  const formulaInputs: Record<string, string[]> = {
    "accounting.net_sales": ["accounting.gross_sales", "accounting.discounts", "accounting.sales_returns"],
    "accounting.gross_profit": ["accounting.net_sales", "accounting.cost_of_goods_sold"],
    "accounting.gross_margin": ["accounting.net_sales", "accounting.cost_of_goods_sold"],
  };
  const requiredFormulaFields = formulaInputs[formulaId] ?? [];
  const confirmedFieldIds = confirmedMappings.filter(mapping => mapping.sheetName === sheet?.name).map(mapping => mapping.fieldId);
  const canCalculate = requiredFormulaFields.length > 0 && requiredFormulaFields.every(field => confirmedFieldIds.includes(field));
  const mappingRanges = confirmedMappings.filter(mapping => mapping.sheetName === sheet?.name).map(mapping => mapping.dataRange.split(":").map(cell => Number(cell.match(/[0-9]+$/)?.[0] ?? 0)));
  const accountingMinRow = mappingRanges.length ? Math.max(...mappingRanges.map(range => range[0])) : 2;
  const accountingMaxRow = mappingRanges.length ? Math.min(...mappingRanges.map(range => range[1])) : (sheet?.rowCount ?? 2);
  const validAccountingRows = /^\d+$/.test(accountingStartRow) && /^\d+$/.test(accountingEndRow)
    && Number(accountingStartRow) >= accountingMinRow && Number(accountingEndRow) >= Number(accountingStartRow)
    && Number(accountingEndRow) <= accountingMaxRow;

  const previewMatchesCurrent = !!rangePreview && !!analysis && !!sheet
    && rangePreview.sourceSha256 === analysis.sourceSha256
    && rangePreview.sheetName === sheet.name && rangePreview.formulaId === formulaId
    && rangePreview.selectedStartRow === Number(accountingStartRow)
    && rangePreview.selectedEndRow === Number(accountingEndRow);
  const previewHasNoTotalRows = previewMatchesCurrent && rangePreview?.possibleTotalRowCount === 0
    && rangePreview?.missingCachedFormulaCount === 0;

  async function previewAccountingRange() {
    if (!analysis || !sheet || !canCalculate || !validAccountingRows || rangePreviewBusy) return;
    setRangePreviewBusy(true);
    setRangePreview(null);
    setRangePreviewError("");
    setCalculationResult(null);
    setCalculationError("");
    try {
      const result = await invoke<WorkbookAccountingRangePreview>("preview_excel_accounting_range", {
        sessionId: analysis.sessionId,
        sheetName: sheet.name,
        formulaId,
        startRow: Number(accountingStartRow),
        endRow: Number(accountingEndRow),
      });
      setRangePreview(result);
    } catch (reason) {
      setRangePreviewError(String(reason));
    } finally {
      setRangePreviewBusy(false);
    }
  }

  async function calculateAccounting() {
    if (!analysis || !sheet || !currency.trim() || !canCalculate || calculationBusy || !previewHasNoTotalRows) return;
    setCalculationBusy(true);
    setCalculationError("");
    setCalculationResult(null);
    try {
      const result = await invoke<WorkbookAccountingCalculation>("calculate_excel_accounting_formula", {
        sessionId: analysis.sessionId,
        sheetName: sheet.name,
        formulaId,
        currency: currency.trim().toUpperCase(),
        startRow: Number(accountingStartRow),
        endRow: Number(accountingEndRow),
      });
      setCalculationResult(result);
    } catch (reason) {
      setCalculationError(String(reason));
    } finally {
      setCalculationBusy(false);
    }
  }

  async function askQuestion() {
    if (!sheet || !question.trim() || asking) return;
    setAsking(true);
    setQuestionAnswer(null);
    try {
      const result = await invoke<ExcelQuestionAnswer>("answer_excel_question", {
        question: question.trim(),
        sessionId: analysis!.sessionId,
        sheetName: sheet.name,
      });
      setQuestionAnswer(result);
    } catch (reason) {
      setQuestionAnswer({
        status: "unsupported",
        answer: String(reason),
        interpretation: "FRIDAY could not complete this local analysis request.",
        evidence: [],
        method: "local analysis error",
      });
    } finally {
      setAsking(false);
    }
  }


  return (
    <section className="page excel-page">
      <div className="eyebrow"><span className="pulse" /> LOCAL WORKBOOK ANALYSIS</div>
      <div className="excel-hero">
        <div>
          <h1>Excel Intelligence.<br /><em>Make sense of your data.</em></h1>
          <p className="intro">Choose an Excel workbook or CSV. FRIDAY will inspect the data, highlight missing values and summarize numeric columns—all on this device.</p>
        </div>
        <div className="excel-mark" aria-hidden="true">▦</div>
      </div>

      <div className="excel-privacy-strip">
        <span aria-hidden="true">◈</span>
        <div><strong>Private, read-only analysis</strong><p>Your workbook stays on this device. FRIDAY does not upload it, change it, execute macros or follow external links.</p></div>
      </div>

      <div className="excel-import-panel">
        <div className="excel-import-copy">
          <span className="excel-step">01 / SELECT A WORKBOOK</span>
          <strong>{analysis ? analysis.fileName : "Start with a spreadsheet"}</strong>
          <p>Supported in this preview: .xlsx and .csv · Up to 20 MB for Excel or 10 MB for CSV</p>
        </div>
        <button className="primary-button excel-pick-button" type="button" onClick={chooseWorkbook} disabled={busy}>
          {busy ? "Analyzing locally…" : analysis ? "Choose another file" : "Choose workbook"}
          <span aria-hidden="true">↗</span>
        </button>
      </div>

      {error && <div className="excel-error" role="alert">{error}</div>}

      {!analysis && !error && (
        <div className="excel-empty-state">
          <div className="excel-empty-icon" aria-hidden="true">▤</div>
          <strong>Your spreadsheet, understood.</strong>
          <p>Import a workbook to see sheet sizes, data previews, missing-value counts and quick numeric summaries.</p>
          <span>Nothing is edited or saved back to your file.</span>
        </div>
      )}

      {analysis && sheet && (
        <div className="excel-results">
          <div className="excel-results-heading">
            <div><span className="excel-step">02 / WORKBOOK OVERVIEW</span><h2>{analysis.fileName}</h2><p>{analysis.format} · {analysis.sheets.length} {analysis.sheets.length === 1 ? "sheet" : "sheets"}</p></div>
            <span className="excel-local-tag">LOCAL ANALYSIS</span>
          </div>

          <div className="excel-metric-grid">
            <article><span>ROWS ANALYZED</span><strong>{numberLabel(analysis.totalRows)}</strong></article>
            <article><span>NON-EMPTY CELLS</span><strong>{numberLabel(analysis.totalNonEmptyCells)}</strong></article>
            <article><span>COLUMNS</span><strong>{numberLabel(sheet.columnCount)}</strong></article>
            <article><span>BLANK CELLS</span><strong>{numberLabel(sheet.blankCells)}</strong></article>
          </div>

          {analysis.sheets.length > 1 && (
            <label className="excel-sheet-picker">Worksheet
              <select value={selectedSheet} onChange={event => { setSelectedSheet(Number(event.target.value)); setQuestionAnswer(null); setCalculationResult(null); setCalculationError(""); setRangePreview(null); setRangePreviewError(""); }}>
                {analysis.sheets.map((item, index) => <option key={item.name} value={index}>{item.name}</option>)}
              </select>
            </label>
          )}

          <section className="excel-section">
            <div className="excel-section-heading"><div><span className="excel-step">03 / DATA PREVIEW</span><h3>{sheet.name}</h3></div><span>{sheet.rowCount} rows · {sheet.columnCount} columns</span></div>
            {sheet.preview.length > 0 ? (
              <div className="excel-table-wrap" role="region" aria-label="Spreadsheet data preview" tabIndex={0}>
                <table className="excel-preview-table">
                  <thead><tr>{sheet.preview[0].map((value, index) => <th key={index}>{value || `Column ${index + 1}`}</th>)}</tr></thead>
                  <tbody>{sheet.preview.slice(1).map((row, rowIndex) => <tr key={rowIndex}>{sheet.preview[0].map((_, columnIndex) => <td key={columnIndex}>{row[columnIndex] ?? ""}</td>)}</tr>)}</tbody>
                </table>
              </div>
            ) : <p className="excel-muted">This worksheet contains no previewable rows.</p>}
            <p className="excel-table-caption">Showing up to 11 data rows and 12 columns. The first row is treated as column headers.</p>
          </section>

          <section className="excel-section excel-quality-section">
            <div className="excel-section-heading">
              <div><span className="excel-step">04 / DATA QUALITY REVIEW</span><h3>Findings worth a closer look</h3></div>
              <span>{sheet.qualityFindings.length} finding{sheet.qualityFindings.length === 1 ? "" : "s"}</span>
            </div>
            {sheet.qualityFindings.length > 0 ? (
              <div className="excel-quality-list">
                {sheet.qualityFindings.map((finding, index) => (
                  <article className={`excel-quality-card ${finding.severity === "warning" ? "is-warning" : "is-info"}`} key={`${finding.category}-${index}`}>
                    <div className="excel-quality-card-top">
                      <strong>{finding.title}</strong>
                      <span>{finding.severity === "warning" ? "REVIEW" : "NOTE"}</span>
                    </div>
                    <p>{finding.description}</p>
                    <div className="excel-quality-meta">
                      <span>{finding.occurrences} occurrence{finding.occurrences === 1 ? "" : "s"}</span>
                      {finding.sampleRows.length > 0 && <span>Rows / columns: {finding.sampleRows.join(", ")}</span>}
                    </div>
                  </article>
                ))}
              </div>
            ) : (
              <div className="excel-quality-clear"><span aria-hidden="true">✓</span><div><strong>No obvious quality issues detected</strong><p>FRIDAY did not find blank or duplicate headers, mixed data types, repeated rows or clear IQR-based numeric outliers in this sheet.</p></div></div>
            )}
            <p className="excel-table-caption">Automated checks are review hints, not proof of errors. Outliers can be valid; FRIDAY never changes or removes data.</p>
          </section>



          <section className="excel-section">
            <div className="excel-section-heading">
              <div><span className="excel-step">05 / ACCOUNTING FIELD REVIEW</span><h3>Possible accounting fields</h3></div>
              <span>REVIEW BEFORE USE</span>
            </div>
            <p className="excel-table-caption">FRIDAY compares column headers with a small reviewed glossary. These are suggestions—not confirmed accounting classifications or permission to calculate.</p>
            <div className="excel-column-grid">
              {sheet.accountingFieldSuggestions.filter(item => item.status !== "unmapped").map((item, index) => (
                <article className="excel-column-card" key={item.headerCell + "-" + index}>
                  <strong>{item.header}</strong>
                  <div className="excel-column-stats">
                    <span>{confirmedMappings.some(mapping => mapping.sheetName === sheet.name && mapping.sourceColumnIndex === item.sourceColumnIndex) ? "CONFIRMED IN THIS SESSION" : item.status === "suggested" ? "POSSIBLE MATCH" : "NEEDS REVIEW"}</span>
                    <b>{item.status === "suggested" ? item.fieldId : item.possibleFields.join(" / ")}</b>
                  </div>
                  <p className="excel-muted">Source: {sheet.name}!{item.headerCell} · Values: {sheet.name}!{item.dataRange}</p>
                  {item.status === "suggested" && !confirmedMappings.some(mapping => mapping.sheetName === sheet.name && mapping.sourceColumnIndex === item.sourceColumnIndex) && (
                    <button type="button" className="secondary-button" onClick={() => void confirmAccountingMapping(item)} disabled={mappingBusy !== null}>
                      {mappingBusy === item.sourceColumnIndex ? "Confirming…" : "Confirm this field"}
                    </button>
                  )}
                </article>
              ))}
            </div>
            {mappingMessage && <p className="excel-table-caption" role="status">{mappingMessage}</p>}
            {sheet.accountingFieldSuggestions.every(item => item.status === "unmapped") && (
              <p className="excel-muted">No known accounting headers were recognized. FRIDAY will not guess from generic column names.</p>
            )}
          </section>

          <section className="excel-section">
            <div className="excel-section-heading">
              <div><span className="excel-step">06 / SOURCE-BOUND ACCOUNTING</span><h3>Calculate from confirmed source data</h3></div>
              <span>READ-ONLY · LOCAL</span>
            </div>
            <p className="excel-table-caption">FRIDAY reads the confirmed columns from this workbook session, sums their nonblank numeric cells using exact-decimal arithmetic, and keeps the source-cell references with the result. No workbook cells are changed.</p>
            <div className="excel-question-form">
              <select aria-label="Accounting formula" value={formulaId} onChange={event => { setFormulaId(event.target.value); setCalculationResult(null); setCalculationError(""); setRangePreview(null); setRangePreviewError(""); }}>
                <option value="accounting.net_sales">Net sales</option>
                <option value="accounting.gross_profit">Gross profit</option>
                <option value="accounting.gross_margin">Gross margin</option>
              </select>
              <input aria-label="Source currency code" value={currency} onChange={event => { setCurrency(event.target.value.toUpperCase().slice(0, 3)); setCalculationResult(null); }} placeholder="Currency (e.g. PKR)" maxLength={3} />
              <input aria-label="First source row (inclusive)" type="number" min={accountingMinRow} max={accountingMaxRow} value={accountingStartRow} onChange={event => { setAccountingStartRow(event.target.value); setCalculationResult(null); setRangePreview(null); setRangePreviewError(""); }} placeholder="First row" />
              <input aria-label="Last source row (inclusive)" type="number" min={accountingMinRow} max={accountingMaxRow} value={accountingEndRow} onChange={event => { setAccountingEndRow(event.target.value); setCalculationResult(null); setRangePreview(null); setRangePreviewError(""); }} placeholder="Last row" />
              <button type="button" className="secondary-button" onClick={() => void previewAccountingRange()} disabled={!canCalculate || !validAccountingRows || rangePreviewBusy}>
                {rangePreviewBusy ? "Preparing preview…" : "Preview selected rows"}
              </button>
              <button type="button" onClick={() => void calculateAccounting()} disabled={!canCalculate || !validAccountingRows || !previewHasNoTotalRows || !/^[A-Z]{3}$/.test(currency.trim()) || calculationBusy}>
                {calculationBusy ? "Calculating…" : "Calculate from source"}
              </button>
            </div>
            {!canCalculate && <p className="excel-table-caption">Confirm each required accounting field above before calculating: {requiredFormulaFields.map(field => field.replace("accounting.", "")).join(", ") || "select a supported formula"}.</p>}
            <p className="excel-table-caption">Selected source rows: {accountingStartRow}–{accountingEndRow} (inclusive). FRIDAY rejects a range containing a likely subtotal or grand-total row instead of silently double-counting it. Non-numeric values are rejected; blank cells are skipped with a warning. Currency is supplied explicitly; no conversion occurs.</p>
            {rangePreviewError && <div className="excel-error" role="alert">{rangePreviewError}</div>}
            {rangePreview && previewMatchesCurrent && (
              <div className="excel-answer-card" aria-live="polite">
                <strong>Source-range review</strong>
                <p>{rangePreview.fileName} · {rangePreview.sheetName} · rows {rangePreview.selectedStartRow}–{rangePreview.selectedEndRow} · {rangePreview.selectedRowCount} selected row(s)</p>
                <p>Preview shows {rangePreview.previewRows.length} row(s){rangePreview.previewTruncated ? "; the selection is larger than the preview window." : "."} Values are read from the retained source session.</p>
                {rangePreview.missingCachedFormulaCount > 0 && (
                  <div className="excel-error" role="alert">
                    <strong>Calculation blocked: {rangePreview.missingCachedFormulaCount} formula cell(s) have no cached result.</strong>
                    <p>FRIDAY will not substitute zero, skip these formula cells, or recalculate them. Open the workbook in Excel, recalculate and save it, then reimport the source.</p>
                  </div>
                )}
                {rangePreview.possibleTotalRowCount > 0 ? (
                  <div className="excel-error" role="alert">
                    <strong>Review required: {rangePreview.possibleTotalRowCount} possible subtotal/total row(s) detected.</strong>
                    <p>FRIDAY will not calculate this range while these rows are included. Narrow the range to exclude candidate rows, preview again, and inspect the source workbook yourself.</p>
                    <ul>{rangePreview.possibleTotalRows.map(candidate => <li key={candidate.worksheetRow}>Worksheet row {candidate.worksheetRow}: {candidate.labels.join(" · ") || "label unavailable"}</li>)}</ul>
                    {rangePreview.totalCandidatesTruncated && <p>Additional candidate rows were detected but are not all listed here.</p>}
                  </div>
                ) : <p>No known total-row label pattern was detected in the selected range. This is not a guarantee that all subtotal or total formats have been recognized.</p>}
                <div className="excel-table-wrap">
                  <table className="excel-data-table">
                    <thead><tr><th>Worksheet row</th>{rangePreview.confirmedMappings.map(mapping => <th key={mapping.fieldId}>{mapping.fieldId}</th>)}<th>Review flag</th></tr></thead>
                    <tbody>{rangePreview.previewRows.slice(0, 25).map(row => <tr key={row.worksheetRow}><td>{row.worksheetRow}</td>{row.cells.map(cell => <td key={cell.fieldId}>{cell.cell}: {cell.value || "(blank)"}{cell.isFormulaCell ? (cell.missingCachedValue ? " · formula cache missing" : " · cached formula value") : ""}</td>)}<td>{row.possibleTotal ? "Possible total" : "—"}</td></tr>)}</tbody>
                  </table>
                </div>
                {rangePreview.previewRows.length > 25 && <p>Showing the first 25 preview rows on screen; the preview response is bounded to 100 rows.</p>}
                <p>Review the selected rows and mapping columns before calculating. Formula results shown here are cached workbook values, not recalculated values; they may be stale. Changing the sheet, formula or row range requires a fresh preview.</p>
              </div>
            )}
            {calculationError && <div className="excel-error" role="alert">{calculationError}</div>}
            {calculationResult && (
              <div className="excel-answer-card answered" aria-live="polite">
                <strong>{calculationResult.formula.outputValue ?? "Calculation incomplete"} {calculationResult.formula.currency ?? calculationResult.formula.outputUnit}</strong>
                <p>{calculationResult.formula.formulaId} v{calculationResult.formula.formulaVersion} · {calculationResult.sheetName} · Source SHA-256 {calculationResult.sourceSha256.slice(0, 12)}…</p>
                <p>Selected worksheet rows {calculationResult.selectedStartRow}–{calculationResult.selectedEndRow}; {calculationResult.sourceCells.length} source-cell reference(s) retained across {calculationResult.sourceRowsIncluded} contributing row(s); {calculationResult.blankCellsSkipped} blank cell(s) skipped.</p>
                <ul>{calculationResult.confirmedMappings.map(mapping => <li key={mapping.fieldId}>{mapping.fieldId}: {mapping.sheetName}!{mapping.headerCell} ({mapping.header}) · {mapping.dataRange}</li>)}</ul>
                <strong>Calculation trace</strong>
                <ul>{calculationResult.formula.trace.map((line, index) => <li key={index}>{line}</li>)}</ul>
                {calculationResult.cachedFormulaCellsUsed > 0 && <p>{calculationResult.cachedFormulaCellsUsed} formula cell(s) contributed cached values. FRIDAY did not recalculate them; results may be stale or differ from Excel's next recalculation.</p>}
                {calculationResult.sourceCells.length > 0 && <p>Source evidence sample: {calculationResult.sourceCells.slice(0, 20).map(cell => cell.cell + "=" + cell.value).join(" · ")}{calculationResult.sourceCells.length > 20 ? " · …" : ""}</p>}
                {calculationResult.formula.warnings.map((warning, index) => <p key={index}>{warning}</p>)}
              </div>
            )}
          </section>

          <section className="excel-section excel-question-section">
            <div className="excel-section-heading">
              <div><span className="excel-step">06 / ASK YOUR SPREADSHEET</span><h3>What would you like to know?</h3></div>
              <span>LOCAL · READ-ONLY</span>
            </div>
            <form className="excel-question-form" onSubmit={event => { event.preventDefault(); void askQuestion(); }}>
              <input
                value={question}
                onChange={event => setQuestion(event.target.value)}
                maxLength={300}
                placeholder="e.g. What is the total revenue?"
                aria-label="Ask a question about the selected worksheet"
              />
              <button type="submit" disabled={asking || !question.trim()}>{asking ? "Analyzing…" : "Ask FRIDAY"}</button>
            </form>
            <p className="excel-table-caption">Try totals, averages, highest or lowest numeric values, missing values, most common values, or row counts. Grouped comparisons and trend analysis are not supported yet.</p>
            {questionAnswer && (
              <div className={`excel-answer-card ${questionAnswer.status}`} aria-live="polite">
                <strong>{questionAnswer.answer}</strong>
                <p>{questionAnswer.interpretation}</p>
                {questionAnswer.evidence.length > 0 && <ul>{questionAnswer.evidence.map((item, index) => <li key={index}>{item}</li>)}</ul>}
                <span>{questionAnswer.method}</span>
              </div>
            )}
          </section>

          <section className="excel-section">
            <div className="excel-section-heading"><div><span className="excel-step">06 / COLUMN INSIGHTS</span><h3>Numeric summaries</h3></div><span>{sheet.columns.length} columns</span></div>
            <div className="excel-column-grid">
              {sheet.columns.map((column, index) => (
                <article className="excel-column-card" key={index}>
                  <strong>{column.name}</strong>
                  <div className="excel-column-stats"><span>Filled <b>{column.nonEmpty}</b></span><span>Missing <b>{column.missing}</b></span></div>
                  {column.numericCount > 0 ? (
                    <dl>
                      <div><dt>Average</dt><dd>{numberLabel(column.average)}</dd></div>
                      <div><dt>Minimum</dt><dd>{numberLabel(column.minimum)}</dd></div>
                      <div><dt>Maximum</dt><dd>{numberLabel(column.maximum)}</dd></div>
                      <div><dt>Total</dt><dd>{numberLabel(column.sum)}</dd></div>
                    </dl>
                  ) : <p className="excel-muted">No numeric values detected</p>}
                  {column.frequencyComplete && column.topValues.length > 0 && <div className="excel-top-values"><span>COMMON VALUES</span><p>{column.topValues.slice(0, 3).map(item => `${item.value} (${item.count})`).join(" · ")}</p></div>}
                </article>
              ))}
            </div>
          </section>

          <div className="excel-notes">
            <strong>What FRIDAY did—and did not do</strong>
            {analysis.notes.map((note, index) => <p key={index}><span aria-hidden="true">•</span>{note}</p>)}
            {sheet.formulaCells > 0 && <p><span aria-hidden="true">•</span>{sheet.formulaCells} formula cells detected in this sheet. Their formulas were not run or recalculated.</p>}
          </div>
          <div className="excel-readonly-footer"><span>READ-ONLY PREVIEW</span><p>Your original workbook has not been modified. Approved editing, chart generation and report export are future steps.</p></div>
        </div>
      )}
    </section>
  );
}
