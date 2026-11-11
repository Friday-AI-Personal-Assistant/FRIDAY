import { useState } from "react";

type Page = "Overview" | "Modules" | "Privacy & Security" | "Settings";

const modules = [
  { name: "Email Assistant", detail: "Organise messages and prepare replies for your approval.", icon: "✉", status: "Planned" },
  { name: "Job Hunter", detail: "Find relevant opportunities and prepare applications for your review.", icon: "⌕", status: "Planned" },
  { name: "CV Studio", detail: "Create, improve and export your CV without inventing experience.", icon: "▤", status: "Planned" },
  { name: "Voice Assistant", detail: "Talk to FRIDAY using optional voice input and responses.", icon: "◖", status: "Planned" }
];

function App() {
  const [page, setPage] = useState<Page>("Overview");

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <div className="brand-mark">F</div>
          <div><strong>FRIDAY</strong><span>PERSONAL AI</span></div>
        </div>
        <div className="side-label">WORKSPACE</div>
        <nav>
          {(["Overview", "Modules", "Privacy & Security", "Settings"] as Page[]).map((item) => (
            <button key={item} className={page === item ? "nav-item active" : "nav-item"} onClick={() => setPage(item)}>
              <span className="nav-icon">{item === "Overview" ? "⌂" : item === "Modules" ? "▦" : item === "Privacy & Security" ? "◇" : "⚙"}</span>
              {item}
            </button>
          ))}
        </nav>
        <div className="sidebar-bottom">
          <div className="local-pill"><span /> LOCAL-FIRST DESIGN</div>
          <p>Your workspace is intended to stay on this device.</p>
          <span className="version">EARLY PREVIEW · v0.1.0</span>
        </div>
      </aside>

      <main className="main-content">
        <header className="topbar">
          <div className="breadcrumb">FRIDAY <span>/</span> {page.toUpperCase()}</div>
          <div className="profile-chip"><div className="profile-dot">U</div><span>YOUR WORKSPACE</span></div>
        </header>

        {page === "Overview" && <section className="page">
          <div className="eyebrow"><span className="pulse" /> YOUR PERSONAL ASSISTANT</div>
          <h1>Good to see you.<br /><em>Let's get things done.</em></h1>
          <p className="intro">A private space for your work, ideas and everyday tasks. FRIDAY is being built to put you in control.</p>

          <div className="notice">
            <div className="notice-icon">i</div>
            <div><strong>Early development preview</strong><p>This is the first interface foundation. AI, accounts, email connections and data storage are not active yet.</p></div>
          </div>

          <div className="section-heading"><div><span className="eyebrow">YOUR SETUP</span><h2>Workspace status</h2></div><span className="muted-tag">NOT CONFIGURED</span></div>
          <div className="status-grid">
            <div className="status-card"><span className="card-icon">◈</span><span className="card-label">AI ENGINE</span><strong>Not connected</strong><small>Local model setup comes next</small></div>
            <div className="status-card"><span className="card-icon">▣</span><span className="card-label">PRIVATE WORKSPACE</span><strong>Not initialized</strong><small>Local storage is not active yet</small></div>
            <div className="status-card"><span className="card-icon">⌑</span><span className="card-label">YOUR MODULES</span><strong>4 planned</strong><small>Choose what you use later</small></div>
          </div>

          <div className="section-heading modules-heading"><div><span className="eyebrow">WHAT FRIDAY WILL DO</span><h2>Coming to your workspace</h2></div><button className="text-button" onClick={() => setPage("Modules")}>View modules <span>↗</span></button></div>
          <div className="module-grid">
            {modules.map((module) => <article className="module-card" key={module.name}>
              <div className="module-top"><span className="module-icon">{module.icon}</span><span className="planned">{module.status}</span></div>
              <h3>{module.name}</h3><p>{module.detail}</p>
            </article>)}
          </div>
        </section>}

        {page === "Modules" && <section className="page compact-page">
          <div className="eyebrow">FRIDAY CAPABILITIES</div><h1>Your tools,<br /><em>your choice.</em></h1>
          <p className="intro">The modules below are planned for the application. They are not functional yet; we'll build and test each one before enabling it.</p>
          <div className="module-list">{modules.map((module) => <article className="module-row" key={module.name}><span className="module-icon">{module.icon}</span><div><h3>{module.name}</h3><p>{module.detail}</p></div><span className="planned">PLANNED</span></article>)}</div>
        </section>}

        {page === "Privacy & Security" && <section className="page compact-page">
          <div className="eyebrow">BUILT AROUND YOUR PRIVACY</div><h1>Your data.<br /><em>Your control.</em></h1>
          <p className="intro">FRIDAY's design goal is to keep personal workspace data on the user's own device, separate from account and licence management.</p>
          <div className="privacy-list">
            <div><span>01</span><section><strong>No developer backdoor</strong><p>We will not intentionally build hidden developer access to private workspaces.</p></section></div>
            <div><span>02</span><section><strong>Local-first storage</strong><p>Personal files and conversations are intended to remain on the device by default.</p></section></div>
            <div><span>03</span><section><strong>Clear external connections</strong><p>Email providers, job websites and optional cloud AI may receive data needed for those features.</p></section></div>
            <div><span>04</span><section><strong>Honest security claims</strong><p>These are design goals, not a completed security audit or a guarantee against malware or compromised updates.</p></section></div>
          </div>
          <p className="footnote">Read the project's Privacy and Security principles in the FRIDAY repository.</p>
        </section>}

        {page === "Settings" && <section className="page compact-page">
          <div className="eyebrow">PERSONALISE FRIDAY</div><h1>Made to work<br /><em>your way.</em></h1>
          <p className="intro">Settings will let each person choose their AI engine, modules, language and assistant personality.</p>
          <div className="settings-placeholder"><span>⚙</span><strong>Settings are being prepared</strong><p>No preferences are being saved in this preview. We'll add secure local settings with the workspace foundation.</p></div>
        </section>}

        <footer className="footer"><span>FRIDAY · YOUR PERSONAL AI</span><span>PRIVATE BY DESIGN. USER IN CONTROL.</span></footer>
      </main>
    </div>
  );
}

export default App;