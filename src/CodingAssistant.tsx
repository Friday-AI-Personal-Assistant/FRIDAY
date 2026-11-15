import { useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type Candidate = { id: string; name: string; relativePath: string; sizeBytes: number; language: string };
type Scan = { projectName: string; files: Candidate[]; scannedEntries: number; truncated: boolean };
type ProjectFile = { name: string; relativePath: string; language: string; content: string; sizeBytes: number };
type Analysis = {
  projectMap: string[]; entryPoints: string[]; dependencies: string[];
  imports: { filePath: string; importText: string; matchedProjectFile?: string | null; line: number }[];
  symbols: { name: string; kind: string; filePath: string; line: number; references: number }[];
  diagnostics: { severity: string; code: string; message: string; filePath: string; line: number }[];
  filesAnalyzed: number; bytesAnalyzed: number; limitations: string[];
};
type Preview = {
  previewId: string; fileName: string; relativePath: string; originalSha256: string;
  proposedSha256: string; originalSizeBytes: number; proposedSizeBytes: number;
  unifiedDiff: string; expiresInSeconds: number;
};
type ApplyResult = { fileName: string; relativePath: string; backupRelativePath: string; bytesWritten: number; undoRecordId: string };

export default function CodingAssistant() {
  const [scan, setScan] = useState<Scan | null>(null);
  const [selected, setSelected] = useState<string[]>([]);
  const [files, setFiles] = useState<ProjectFile[]>([]);
  const [analysis, setAnalysis] = useState<Analysis | null>(null);
  const [activeFileId, setActiveFileId] = useState("");
  const [proposed, setProposed] = useState("");
  const [preview, setPreview] = useState<Preview | null>(null);
  const [approval, setApproval] = useState("");
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [lastApply, setLastApply] = useState<ApplyResult | null>(null);
  const activeFile = useMemo(() => files.find(file => file.relativePath === scan?.files.find(item => item.id === activeFileId)?.relativePath), [files, scan, activeFileId]);

  async function run<T>(label: string, action: () => Promise<T>, success?: (value: T) => void) {
    setBusy(true); setMessage("");
    try { const value = await action(); success?.(value); setMessage(label); return value; }
    catch (error) { setMessage(String(error)); return undefined; }
    finally { setBusy(false); }
  }
  async function chooseProject() {
    await run("Project folder scanned. Select files to inspect.", () =>
      invoke<Scan>("pick_project_folder_and_list_files"), value => {
        setScan(value); setSelected([]); setFiles([]); setAnalysis(null); setPreview(null);
        setApproval(""); setLastApply(null); setActiveFileId("");
      });
  }
  function toggleFile(id: string) {
    setSelected(current => current.includes(id) ? current.filter(item => item !== id) : current.length < 25 ? [...current, id] : current);
  }
  async function inspectProject() {
    await run("Selected files loaded and analyzed locally.", async () => {
      const loaded = await invoke<ProjectFile[]>("read_project_files", { fileIds: selected });
      const result = await invoke<Analysis>("analyze_project_intelligence", { files: loaded });
      return { loaded, result };
    }, value => {
      setFiles(value.loaded); setAnalysis(value.result); setPreview(null); setApproval("");
      const first = scan?.files.find(item => selected.includes(item.id));
      setActiveFileId(first?.id ?? ""); setProposed("");
    });
  }
  async function makePreview() {
    if (!activeFile || !activeFileId) return;
    await run("Change preview created. Review the diff before approving.", () =>
      invoke<Preview>("preview_code_change", { fileId: activeFileId, proposedContent: proposed }), value => {
        setPreview(value); setApproval(""); setLastApply(null);
      });
  }
  async function approveAndApply() {
    if (!preview) return;
    const token = await run("Approval issued for this exact preview.", () =>
      invoke<string>("issue_code_change_approval", { previewId: preview.previewId }));
    if (!token) return;
    setApproval(token);
    const confirmed = window.confirm(
      `Apply the reviewed change to ${preview.relativePath}?\\n\\nFRIDAY will preserve a backup. The original file will be replaced only after its hash is rechecked.`
    );
    if (!confirmed) { setMessage("Approval was not applied. The preview remains available until it expires."); return; }
    const result = await run("Approved change applied and backup saved.", () =>
      invoke<ApplyResult>("apply_code_change", { previewId: preview.previewId, approvalToken: token }, value => {
        setLastApply(value); setPreview(null); setApproval(""); setProposed("");
      }));
    if (result) {
      await run("Project file list refreshed.", () => invoke<Scan>("pick_project_folder_and_list_files"), value => {
        setScan(value); setSelected([]); setFiles([]); setAnalysis(null); setActiveFileId("");
      });
    }
  }
  async function cancelPreview() {
    if (!preview) return;
    await run("Preview cancelled; no project file was changed.", async () => {
      await invoke("cancel_code_change_preview", { previewId: preview.previewId });
      setPreview(null); setApproval("");
    });
  }
  async function undoLastChange() {
    if (!lastApply) return;
    await run("The approved change was undone.", () =>
      invoke<string>("undo_code_change", { undoRecordId: lastApply.undoRecordId }), () => setLastApply(null));
  }

  return <section className="page coding-assistant-page">
    <div className="eyebrow">LOCAL PROJECT WORKSPACE</div>
    <h1>Build with care.<br /><em>Review every change.</em></h1>
    <p className="intro">Choose a project folder, inspect selected source files, review FRIDAY's structural analysis, and approve each file change yourself.</p>
    <div className="coding-safety-note"><strong>Protected workflow</strong><span>FRIDAY reads only files you select. Changes are previewed first, require explicit approval, and keep a recoverable backup. Sandbox code execution is not enabled yet.</span></div>
    <div className="coding-toolbar">
      <button className="primary-button" disabled={busy} onClick={chooseProject}>{busy ? "Working…" : scan ? "Choose another project" : "Choose project folder"}</button>
      {scan && <span className="coding-project-name">{scan.projectName} · {scan.files.length} files</span>}
    </div>
    {scan && <>
      <div className="coding-panel">
        <div className="section-heading"><div><span className="eyebrow">PROJECT FILES</span><h2>Choose files to inspect</h2></div><span className="muted-tag">{selected.length}/25 SELECTED</span></div>
        <p className="panel-copy">Select up to 25 supported source and configuration files. Reading is limited to 180 KB combined.</p>
        {scan.truncated && <p className="coding-warning">The scan reached its safety limit. The list may not include every project file.</p>}
        <div className="coding-file-list">{scan.files.map(file => <label key={file.id} className="coding-file-row">
          <input type="checkbox" checked={selected.includes(file.id)} onChange={() => toggleFile(file.id)} />
          <span><strong>{file.relativePath}</strong><small>{file.language} · {file.sizeBytes.toLocaleString()} bytes</small></span>
        </label>)}</div>
        <button className="primary-button" disabled={busy || selected.length === 0} onClick={inspectProject}>Inspect selected files</button>
      </div>
    </>}
    {analysis && <>
      <div className="coding-panel coding-intelligence">
        <div className="section-heading"><div><span className="eyebrow">LOCAL STRUCTURAL ANALYSIS</span><h2>Project intelligence</h2></div><span className="muted-tag">{analysis.filesAnalyzed} FILES</span></div>
        <div className="coding-metrics"><span><strong>{analysis.symbols.length}</strong> symbols</span><span><strong>{analysis.imports.length}</strong> imports</span><span><strong>{analysis.diagnostics.length}</strong> review hints</span><span><strong>{analysis.dependencies.length}</strong> dependencies</span></div>
        {analysis.entryPoints.length > 0 && <div><strong>Entry points</strong><p>{analysis.entryPoints.join(" · ")}</p></div>}
        {analysis.projectMap.length > 0 && <div><strong>Project map</strong><ul>{analysis.projectMap.map(path => <li key={path}>{path}</li>)}</ul></div>}
        {analysis.diagnostics.length > 0 && <div><strong>Review hints</strong><ul>{analysis.diagnostics.map((item, index) => <li key={item.code + item.filePath + item.line + index}><span className={item.severity === "warning" ? "coding-warning-text" : ""}>{item.severity.toUpperCase()} · {item.code}</span> — {item.filePath}:{item.line} · {item.message}</li>)}</ul></div>}
        {analysis.limitations.map(item => <small key={item}>{item}</small>)}
      </div>
      <div className="coding-panel">
        <div className="section-heading"><div><span className="eyebrow">CONTROLLED FILE EDITING</span><h2>Prepare a change</h2></div></div>
        <label className="coding-field">File to change
          <select value={activeFileId} onChange={event => { setActiveFileId(event.target.value); setPreview(null); setApproval(""); setProposed(""); }}>
            <option value="">Choose an inspected file</option>
            {scan?.files.filter(file => selected.includes(file.id)).map(file => <option key={file.id} value={file.id}>{file.relativePath}</option>)}
          </select>
        </label>
        {activeFile && <>
          <div className="coding-current-file"><strong>Current contents · {activeFile.relativePath}</strong><pre>{activeFile.content}</pre></div>
          <label className="coding-field">Proposed complete file contents
            <textarea value={proposed} onChange={event => { setProposed(event.target.value); setPreview(null); setApproval(""); }} rows={14} maxLength={131072} placeholder="Paste or write the complete proposed file contents here…" />
          </label>
          <button className="primary-button" disabled={busy || !proposed.trim()} onClick={makePreview}>Create reviewable diff</button>
        </>}
      </div>
    </>}
    {preview && <div className="coding-panel coding-change-preview">
      <div className="section-heading"><div><span className="eyebrow">EXPLICIT REVIEW REQUIRED</span><h2>Proposed change</h2></div><span className="planned">EXPIRES IN {preview.expiresInSeconds}s</span></div>
      <strong>{preview.relativePath}</strong>
      <small>Original SHA-256: {preview.originalSha256}</small><small>Proposed SHA-256: {preview.proposedSha256}</small>
      <pre className="coding-diff">{preview.unifiedDiff}</pre>
      <p>Applying replaces this one file only after FRIDAY rechecks the original hash. A backup is retained for guarded undo.</p>
      <div className="coding-actions"><button className="primary-button" disabled={busy} onClick={approveAndApply}>Approve and apply this change</button><button className="secondary-button" disabled={busy} onClick={cancelPreview}>Cancel preview</button></div>
    </div>}
    {lastApply && <div className="coding-panel"><strong>Change saved</strong><p>{lastApply.relativePath} · {lastApply.bytesWritten.toLocaleString()} bytes</p><small>Backup: {lastApply.backupRelativePath}</small><div className="coding-actions"><button className="secondary-button" disabled={busy} onClick={undoLastChange}>Undo this change</button></div></div>}
    {message && <p className="workspace-message" role="status">{message}</p>}
    <p className="security-footnote">Analysis is heuristic, not a compiler or security audit. This interface does not execute project code, run arbitrary shell commands, or enable Windows Sandbox guest execution.</p>
  </section>;
}
