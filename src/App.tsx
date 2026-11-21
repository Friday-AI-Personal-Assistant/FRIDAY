import { useEffect, useState } from "react";

function readInitialTheme(): "dark" | "light" {
  try {
    return window.localStorage.getItem("friday-theme") === "light" ? "light" : "dark";
  } catch {
    return "dark";
  }
}

import { invoke } from "@tauri-apps/api/core";
import Assistant from "./Assistant";
import SkillsManager from "./SkillsManager";
import CodingAssistant from "./CodingAssistant";
import PermissionsCenter from "./PermissionsCenter";

type Page = "AI Chat" | "Coding Assistant" | "Overview" | "Modules" | "Privacy & Security" | "Permissions & Transparency" | "Settings";

type JourneyStatus = "available" | "in-progress" | "needs-verification" | "planned";

const JOURNEY_MILESTONES: Array<{ id: string; title: string; status: JourneyStatus }> = [
  { id: "ai-chat", title: "AI Chat", status: "available" },
  { id: "coding-assistant", title: "Coding Assistant", status: "in-progress" },
  { id: "skills-store", title: "Skills Store", status: "needs-verification" },
  { id: "midnight-azure", title: "Midnight Azure", status: "needs-verification" },
  { id: "excel-intelligence", title: "Excel Intelligence", status: "planned" },
  { id: "recovery-storage-manager", title: "Recovery Storage Manager", status: "planned" }
];

const JOURNEY_SNAPSHOT_KEY = "friday.developmentJourney.verifiedMilestones.v1";

const modules = [
  { name: "Email Assistant", detail: "Organise messages and prepare replies for your approval.", icon: "✉", status: "Planned" },
  { name: "Job Hunter", detail: "Find relevant opportunities and compare them with your experience.", icon: "⌕", status: "Planned" },
  { name: "CV Studio", detail: "Create and improve an honest, ATS-friendly CV.", icon: "▤", status: "Planned" },
  { name: "Voice Assistant", detail: "Use optional voice input and spoken responses.", icon: "◖", status: "Planned" }
];

function App() {
  const [page, setPage] = useState<Page>("AI Chat");
  const [theme, setTheme] = useState<"dark" | "light">(readInitialTheme);
  const [workspaceReady, setWorkspaceReady] = useState(false);
  const [note, setNote] = useState("");
  const [workspaceMessage, setWorkspaceMessage] = useState("");
  const [milestoneCelebration, setMilestoneCelebration] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const isDesktopApp = "__TAURI_INTERNALS__" in window;

  useEffect(() => {
    const current = Object.fromEntries(JOURNEY_MILESTONES.map(item => [item.id, item.status]));
    try {
      const saved = window.localStorage.getItem(JOURNEY_SNAPSHOT_KEY);
      if (!saved) {
        window.localStorage.setItem(JOURNEY_SNAPSHOT_KEY, JSON.stringify(current));
        return;
      }

      let previous: Record<string, JourneyStatus>;
      try {
        previous = JSON.parse(saved) as Record<string, JourneyStatus>;
      } catch {
        // An unreadable snapshot is a new baseline, never a reason to announce a milestone.
        window.localStorage.setItem(JOURNEY_SNAPSHOT_KEY, JSON.stringify(current));
        return;
      }

      const newlyAvailable = JOURNEY_MILESTONES
        .filter(item => item.status === "available" && previous[item.id] !== "available")
        .map(item => item.title);

      // Persist the new snapshot before showing anything, so a reload cannot replay it.
      window.localStorage.setItem(JOURNEY_SNAPSHOT_KEY, JSON.stringify(current));
      if (newlyAvailable.length) setMilestoneCelebration(newlyAvailable);
    } catch {
      // Without durable local state, suppress celebrations rather than repeat or invent them.
    }
  }, []);

  function updateTheme(next: "dark" | "light") {
    setTheme(next);
    try { window.localStorage.setItem("friday-theme", next); } catch { /* The current session still uses the selected theme. */ }
  }

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
    <div className="app-shell" data-theme={theme}>
      <aside className="sidebar">
        <div className="brand"><div className="brand-mark">F</div><div><strong>FRIDAY</strong><span>PERSONAL AI</span></div></div>
        <div className="side-label">WORKSPACE</div>
        <nav>
          {(["AI Chat", "Coding Assistant", "Overview", "Modules", "Permissions & Transparency", "Privacy & Security", "Settings"] as Page[]).map(item => (
            <button key={item} className={page === item ? "nav-item active" : "nav-item"} onClick={() => setPage(item)}>
              <span className="nav-icon">{item === "AI Chat" ? "✦" : item === "Coding Assistant" ? "⌘" : item === "Overview" ? "⌂" : item === "Modules" ? "▦" : item === "Privacy & Security" ? "◇" : item === "Permissions & Transparency" ? "⛨" : "⚙"}</span>{item}
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
          <button
            className="theme-toggle"
            type="button"
            aria-label={theme === "dark" ? "Switch to light theme" : "Switch to dark theme"}
            aria-pressed={theme === "light"}
            onClick={() => updateTheme(theme === "dark" ? "light" : "dark")}
          >
            <span aria-hidden="true">{theme === "dark" ? "☼" : "☾"}</span>
            {theme === "dark" ? "Light theme" : "Dark theme"}
          </button>
          <div className="profile-chip"><div className="profile-dot">U</div><span>YOUR WORKSPACE</span></div>
        </header>

        {page === "AI Chat" && <Assistant />}
        {page === "Coding Assistant" && <CodingAssistant />}

        {page === "Overview" && <section className="page overview-page">
          <div className="overview-hero">
            <div className="eyebrow"><span className="pulse" /> YOUR PERSONAL ASSISTANT</div>
            <h1>Good to see you.<br /><em>Let's make progress.</em></h1>
            <p className="intro">A private space for your work, ideas and everyday tasks. I’m FRIDAY: here to be useful, clear about what I can do, and careful with what matters to you.</p>
            <div className="overview-hero-footer">
              <span className="overview-local-status"><span className="pulse" /> LOCAL-FIRST WORKSPACE</span>
              <span className="overview-roadmap-label">ON THE ROADMAP</span>
            </div>
            <div className="overview-next-milestone">
              <div><span className="overview-milestone-kicker">NEXT PRODUCT MILESTONE</span><strong>Excel Intelligence</strong><p>Our first practical skill: understand spreadsheets, uncover useful insights and help create reports.</p></div>
              <span className="overview-planned-tag">PLANNED</span>
            </div>
          </div>

          <section className="development-journey" aria-labelledby="development-journey-title">
            <div className="friday-personality-note">
              <span className="friday-personality-mark" aria-hidden="true">F</span>
              <p><strong>A note from FRIDAY</strong><span>I’ll show you what is ready, what still needs work, and what we’re building next. No pretend progress, no surprises.</span></p>
            </div>
            {milestoneCelebration.length > 0 && <div className="milestone-celebration" role="status" aria-live="polite">
              <div><span className="celebration-kicker">A VERIFIED MILESTONE</span><strong>{milestoneCelebration.length === 1 ? milestoneCelebration[0] + " is now available" : milestoneCelebration.join(", ") + " are now available"}</strong><p>This capability has moved to Available in FRIDAY’s verified development journey. Take a look whenever you’re ready.</p></div>
              <button type="button" onClick={() => setMilestoneCelebration([])} aria-label="Dismiss milestone celebration">Dismiss</button>
            </div>}
            <div className="journey-heading">
              <div><span className="eyebrow">THE ROAD AHEAD</span><h2 id="development-journey-title">FRIDAY Development Journey</h2><p>Real capabilities, honest progress, and the next useful things we're building.</p></div>
              <span className="journey-roadmap-tag">ROADMAP-TRACKED</span>
            </div>
            <div className="journey-grid">
              <article className="journey-card journey-available">
                <div className="journey-card-top"><span className="journey-step">01</span><span className="journey-status">AVAILABLE</span></div>
                <h3>AI Chat</h3><p>Chat with a local Ollama model when the model service is running on your device.</p>
                <button type="button" onClick={() => setPage("AI Chat")}>Open AI Chat <span aria-hidden="true">↗</span></button>
              </article>
              <article className="journey-card journey-progress">
                <div className="journey-card-top"><span className="journey-step">02</span><span className="journey-status">IN PROGRESS</span></div>
                <h3>Coding Assistant</h3><p>Project-aware coding help and guarded change proposals are being developed and refined.</p>
                <button type="button" onClick={() => setPage("Coding Assistant")}>Explore workspace <span aria-hidden="true">↗</span></button>
              </article>
              <article className="journey-card journey-verify">
                <div className="journey-card-top"><span className="journey-step">03</span><span className="journey-status">NEEDS VERIFICATION</span></div>
                <h3>Skills Store</h3><p>Imported community skills can be inspected in quarantine; they remain inert and cannot execute.</p>
                <button type="button" onClick={() => setPage("Modules")}>Review skills <span aria-hidden="true">↗</span></button>
              </article>
              <article className="journey-card journey-verify">
                <div className="journey-card-top"><span className="journey-step">04</span><span className="journey-status">NEEDS VERIFICATION</span></div>
                <h3>Midnight Azure</h3><p>The visual system is integrated. Installed-app and narrow-window visual acceptance still remain.</p>
                <button type="button" onClick={() => setPage("Settings")}>View appearance <span aria-hidden="true">↗</span></button>
              </article>
              <article className="journey-card journey-planned">
                <div className="journey-card-top"><span className="journey-step">05</span><span className="journey-status">PLANNED</span></div>
                <h3>Excel Intelligence</h3><p>Our first practical skill: analyze spreadsheets, spot issues and prepare useful summaries with approval before edits.</p>
                <span className="journey-no-action">NEXT PRODUCT CAPABILITY</span>
              </article>
              <article className="journey-card journey-planned">
                <div className="journey-card-top"><span className="journey-step">06</span><span className="journey-status">PLANNED</span></div>
                <h3>Recovery Storage Manager</h3><p>Understand FRIDAY-managed recovery storage and preserve user work through safe, verified controls.</p>
                <span className="journey-no-action">SAFETY-FIRST FEATURE</span>
              </article>
            </div>
            <p className="journey-footnote">Statuses reflect the development roadmap, not autonomous background work. A successful build alone does not make a feature fully accepted.</p>
          </section>

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

        {page === "Permissions & Transparency" && <PermissionsCenter isDesktopApp={isDesktopApp} />}

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

        {page === "Settings" && <section className="page compact-page settings-page">
          <div className="eyebrow">PERSONALISE FRIDAY</div><h1>Made to work<br /><em>your way.</em></h1>
          <p className="intro">Manage the preferences that are available today. FRIDAY will clearly identify settings that are still being developed.</p>

          <div className="settings-section">
            <div className="settings-section-heading"><div><span className="eyebrow">01 / APPEARANCE</span><h2>Choose your theme</h2></div><span className="settings-current">{theme === "dark" ? "MIDNIGHT AZURE" : "LIGHT AZURE"}</span></div>
            <p className="settings-description">Your appearance preference is saved on this device. Midnight Azure remains FRIDAY's default visual identity.</p>
            <div className="settings-theme-options">
              <button type="button" className={theme === "dark" ? "settings-theme-option selected" : "settings-theme-option"} aria-pressed={theme === "dark"} onClick={() => updateTheme("dark")}>
                <span className="theme-preview dark-preview"><i /><i /><i /></span><strong>Midnight Azure</strong><small>Dark · Navy surfaces · Refined blue</small>
              </button>
              <button type="button" className={theme === "light" ? "settings-theme-option selected" : "settings-theme-option"} aria-pressed={theme === "light"} onClick={() => updateTheme("light")}>
                <span className="theme-preview light-preview"><i /><i /><i /></span><strong>Light Azure</strong><small>Light · Soft grey · Deep blue</small>
              </button>
            </div>
          </div>

          <div className="settings-section">
            <div className="settings-section-heading"><div><span className="eyebrow">02 / AI ENGINE</span><h2>Your model provider</h2></div><span className="settings-current">LOCAL-FIRST</span></div>
            <p className="settings-description">AI Chat currently connects to Ollama on this device. Model discovery, installation and selection are managed inside AI Chat; cloud providers and automatic cloud fallback are not enabled by this setting.</p>
            <button type="button" className="settings-action" onClick={() => setPage("AI Chat")}>Open AI Chat <span aria-hidden="true">↗</span></button>
          </div>

          <div className="settings-section">
            <div className="settings-section-heading"><div><span className="eyebrow">03 / PRIVACY & ACCESS</span><h2>Review your permissions</h2></div><span className="settings-current">USER CONTROL</span></div>
            <p className="settings-description">Review active Web Research grants, their expiry, and the permission activity history. You can revoke a grant at any time.</p>
            <button type="button" className="settings-action" onClick={() => setPage("Permissions & Transparency")}>Open Permissions & Transparency <span aria-hidden="true">↗</span></button>
          </div>

          <div className="settings-coming">
            <strong>Still in development</strong>
            <p>Language, voice, startup behavior, notifications, model-provider accounts and cross-device sync are not presented as configurable because their settings and safeguards are not implemented yet.</p>
          </div>
        </section>}

        <footer className="footer"><span>FRIDAY · YOUR PERSONAL AI</span><span>PRIVATE BY DESIGN. USER IN CONTROL.</span></footer>
      </main>
    </div>
  );
}

export default App;
