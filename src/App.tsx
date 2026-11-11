import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import Assistant from "./Assistant";
import SkillsManager from "./SkillsManager";

type Page = "AI Chat" | "Overview" | "Modules" | "Privacy & Security" | "Settings";

const modules = [
  { name: "Email Assistant", detail: "Organise messages and prepare replies for your approval.", icon: "✉", status: "Planned" },
  { name: "Job Hunter", detail: "Find relevant opportunities and compare them with your experience.", icon: "⌕", status: "Planned" },
  { name: "CV Studio", detail: "Create and improve an honest, ATS-friendly CV.", icon: "▤", status: "Planned" },
  { name: "Voice Assistant", detail: "Use optional voice input and spoken responses.", icon: "◖", status: "Planned" }
];

function App() {
  const [page, setPage] = useState<Page>("AI Chat");
  const [workspaceReady, setWorkspaceReady] = useState(false);
  const [note, setNote] = useState("");
  const [workspaceMessage, setWorkspaceMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const isDesktopApp = "__TAURI_INTERNALS__" in window;

  async function initializeWorkspace() {
    setBusy(true);
    setWorkspaceMessage("");
    try {
      const message = await invoke<string>("initialize_workspace");
      const savedNote = await invoke<string>("read_private_note");
      setNote(savedNote);
      setWorkspaceReady(true);
      setWorkspaceMessage(message);
    } catch (error) {
      setWorkspaceMessage(String(error));
    } finally {
      setBusy(false);
    }
  }

  async function saveNote() {
    setBusy(true);
    setWorkspaceMessage("");
    try {
      const message = await invoke<string>("save_private_note", { note });
      setWorkspaceMessage(message);
    } catch (error) {
      setWorkspaceMessage(String(error));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand"><div className="brand-mark">F</div><div><strong>FRIDAY</strong><span>PERSONAL AI</span></div></div>
        <div className="side-label">WORKSPACE</div>
        <nav>
          {(["AI Chat", "Overview", "Modules", "Privacy & Security", "Settings"] as Page[]).map(item => (
            <button key={item} className={page === item ? "nav-item active" : "nav-item"} onClick={() => setPage(item)}>
              <span className="nav-icon">{item === "AI Chat" ? "✦" : item === "Overview" ? "⌂" : item === "Modules" ? "▦" : item === "Privacy & Security" ? "◇" : "⚙"}</span>{item}
            </button>
          ))}
        </nav>
        <div className="sidebar-bottom">
          <div className="local-pill"><span /> LOCAL-FIRST DESIGN</div>
          <p>Your workspace is designed to stay on this device.</p>
          <span className="version">LOCAL AI · v0.2.0</span>
        </div>
      </aside>

      <main className="main-content">
        <header className="topbar">
          <div className="breadcrumb">FRIDAY <span>/</span> {page.toUpperCase()}</div>
          <div className="profile-chip"><div className="profile-dot">U</div><span>YOUR WORKSPACE</span></div>
        </header>

        {page === "AI Chat" && <Assistant />}

        {page === "Overview" && <section className="page">
          <div className="eyebrow"><span className="pulse" /> YOUR PERSONAL ASSISTANT</div>
          <h1>Good to see you.<br /><em>Let's get things done.</em></h1>
          <p className="intro">A private space for your work, ideas and everyday tasks. FRIDAY is being built to put you in control.</p>

          <div className="notice">
            <div className="notice-icon">i</div>
            <div><strong>Early development preview</strong><p>The local encrypted-note foundation is now being implemented. AI Chat is available on Windows when Ollama is running. Accounts and full email integrations are not active yet.</p></div>
          </div>

          <div className="section-heading"><div><span className="eyebrow">YOUR SETUP</span><h2>Workspace status</h2></div><span className="muted-tag">{workspaceReady ? "INITIALIZED" : "NOT INITIALIZED"}</span></div>
          <div className="status-grid">
            <div className="status-card"><span className="card-icon">◈</span><span className="card-label">AI ENGINE</span><strong>Local Ollama</strong><small>Open AI Chat to check connection</small></div>
            <div className="status-card"><span className="card-icon">▣</span><span className="card-label">PRIVATE WORKSPACE</span><strong>{workspaceReady ? "Initialized" : "Not initialized"}</strong><small>Encryption key stored by Windows</small></div>
            <div className="status-card"><span className="card-icon">⌑</span><span className="card-label">YOUR MODULES</span><strong>4 planned</strong><small>Choose what you use later</small></div>
          </div>

          <div className="workspace-panel">
            <div className="section-heading"><div><span className="eyebrow">ON-DEVICE STORAGE</span><h2>Private workspace test</h2></div><span className="planned">ENCRYPTED NOTE</span></div>
            <p className="panel-copy">{isDesktopApp ? "Initialize your local workspace, then save a test note. The note is encrypted before it is written to your computer. Its key is kept in Windows Credential Manager." : "You are viewing the browser preview. The private workspace test requires the Windows desktop app, so no notes are stored or encrypted in this browser demo."}</p>
            {!isDesktopApp ? <div className="browser-preview-label">BROWSER DEMO · STORAGE DISABLED</div> : !workspaceReady ? <button className="primary-button" disabled={busy} onClick={initializeWorkspace}>{busy ? "Please wait…" : "Initialize private workspace"}</button> : <>
              <textarea className="private-note" value={note} onChange={event => setNote(event.target.value)} maxLength={20000} placeholder="Write a private test note here…" />
              <button className="primary-button" disabled={busy} onClick={saveNote}>{busy ? "Please wait…" : "Save encrypted note"}</button>
            </>}
            {workspaceMessage && <p className="workspace-message" role="status">{workspaceMessage}</p>}
            <p className="security-footnote">Preview limitation: this test stores one encrypted note only. It is not yet a complete database, backup, account recovery or audited security system. Anyone controlling your unlocked Windows account may still access data available to that account.</p>
          </div>

          <div className="section-heading modules-heading"><div><span className="eyebrow">WHAT FRIDAY WILL DO</span><h2>Coming to your workspace</h2></div><button className="text-button" onClick={() => setPage("Modules")}>View modules <span>↗</span></button></div>
          <div className="module-grid">{modules.map(module => <article className="module-card" key={module.name}>
            <div className="module-top"><span className="module-icon">{module.icon}</span><span className="planned">{module.status}</span></div>
            <h3>{module.name}</h3><p>{module.detail}</p>
          </article>)}</div>
        </section>}

        {page === "Modules" && <section className="page compact-page">
          <div className="eyebrow">FRIDAY CAPABILITIES</div><h1>Your tools,<br /><em>your choice.</em></h1>
          <p className="intro">Install capabilities when you need them. FRIDAY keeps installed skills on disk and does not start them automatically.</p>
          <SkillsManager />
        </section>}

        {page === "Privacy & Security" && <section className="page compact-page">
          <div className="eyebrow">BUILT AROUND YOUR PRIVACY</div><h1>Your data.<br /><em>Your control.</em></h1>
          <p className="intro">FRIDAY separates local personal data from any future account and licence service.</p>
          <div className="privacy-list">
            <div><span>01</span><section><strong>No developer backdoor</strong><p>There is no developer remote-access command in this preview.</p></section></div>
            <div><span>02</span><section><strong>Local encryption foundation</strong><p>The preview uses an encrypted test note and Windows Credential Manager for its key. This is not yet a complete secure workspace.</p></section></div>
            <div><span>03</span><section><strong>Clear external connections</strong><p>Email providers, job websites and optional cloud AI may receive data needed for those features.</p></section></div>
            <div><span>04</span><section><strong>Honest security claims</strong><p>This has not received an independent security audit and cannot protect against malware or an attacker controlling an unlocked device.</p></section></div>
          </div>
          <p className="footnote">See the repository's privacy principles and local workspace design document.</p>
        </section>}

        {page === "Settings" && <section className="page compact-page">
          <div className="eyebrow">PERSONALISE FRIDAY</div><h1>Made to work<br /><em>your way.</em></h1>
          <p className="intro">Settings will let each person choose their AI engine, modules, language and assistant personality.</p>
          <div className="settings-placeholder"><span>⚙</span><strong>Settings are being prepared</strong><p>No preferences are saved in this preview yet. We'll add secure local settings with the workspace foundation.</p></div>
        </section>}

        <footer className="footer"><span>FRIDAY · YOUR PERSONAL AI</span><span>PRIVATE BY DESIGN. USER IN CONTROL.</span></footer>
      </main>
    </div>
  );
}

export default App;
