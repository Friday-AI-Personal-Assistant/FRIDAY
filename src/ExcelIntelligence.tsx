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
};

type ExcelQuestionAnswer = {
  status: "answered" | "clarification" | "unsupported";
  answer: string;
  interpretation: string;
  evidence: string[];
  method: string;
};

type WorkbookAnalysis = {
  fileName: string;
  format: string;
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

  async function chooseWorkbook() {
    setBusy(true);
    setError("");
    try {
      const result = await invoke<WorkbookAnalysis>("pick_and_analyze_workbook");
      setAnalysis(result);
      setSelectedSheet(0);
      setQuestion("");
      setQuestionAnswer(null);
    } catch (reason) {
      const message = String(reason);
      if (!message.toLowerCase().includes("cancel")) setError(message);
    } finally {
      setBusy(false);
    }
  }

  const sheet = analysis?.sheets[selectedSheet];

  async function askQuestion() {
    if (!sheet || !question.trim() || asking) return;
    setAsking(true);
    setQuestionAnswer(null);
    try {
      const result = await invoke<ExcelQuestionAnswer>("answer_excel_question", {
        question: question.trim(),
        sheetName: sheet.name,
        rowCount: sheet.rowCount,
        columns: sheet.columns,
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
              <select value={selectedSheet} onChange={event => setSelectedSheet(Number(event.target.value))}>
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


          <section className="excel-section excel-question-section">
            <div className="excel-section-heading">
              <div><span className="excel-step">05 / ASK YOUR SPREADSHEET</span><h3>What would you like to know?</h3></div>
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
