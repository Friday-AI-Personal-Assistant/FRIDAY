import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import Agent from './Agent';

type Page = 'Assistant' | 'Private workspace' | 'Privacy';

export default function App() {
  const [page, setPage] = useState<Page>('Assistant');
  const [ready, setReady] = useState(false);
  const [note, setNote] = useState('');
  const [message, setMessage] = useState('');
  const [busy, setBusy] = useState(false);
  const desktop = '__TAURI_INTERNALS__' in window;
  async function initialize() {
    setBusy(true);
    try {
      setMessage(await invoke<string>('initialize_workspace'));
      setNote(await invoke<string>('read_private_note'));
      setReady(true);
    } catch (e) { setMessage(String(e)); } finally { setBusy(false); }
  }
  async function save() {
    setBusy(true);
    try { setMessage(await invoke<string>('save_private_note', { note })); }
    catch (e) { setMessage(String(e)); } finally { setBusy(false); }
  }
  return <div className="app-shell">
    <aside className="sidebar">
      <div className="brand"><div className="brand-mark">F</div><div><strong>FRIDAY</strong><span>PERSONAL AI</span></div></div>
      <div className="side-label">YOUR WORKSPACE</div>
      <nav aria-label="Main navigation">{(['Assistant', 'Private workspace', 'Privacy'] as Page[]).map((name, i) => <button key={name} className={'nav-item' + (page === name ? ' active' : '')} onClick={() => setPage(name)} aria-label={name}><span className="nav-icon">{['◈', '▣', '◇'][i]}</span>{name}</button>)}</nav>
      <div className="sidebar-bottom"><div className="local-pill"><span/> LOCAL AI ONLY</div><p>Ollama on your computer. No paid APIs or cloud AI.</p><span className="version">DEVELOPMENT BUILD · v0.1.0</span></div>
    </aside>
    <main className="main-content">
      <header className="topbar"><div className="breadcrumb">FRIDAY <span>/</span> {page.toUpperCase()}</div><div className="profile-chip"><div className="profile-dot">F</div><span>FREE & LOCAL</span></div></header>
      {/* Keep the assistant mounted so navigation cannot silently discard a running task or unsaved conversation. */}
      <div hidden={page !== 'Assistant'}><Agent/></div>
      {page === 'Private workspace' && <section className="page compact-page">
        <div className="eyebrow">ON-DEVICE STORAGE</div><h1>Your private<br/><em>workspace.</em></h1>
        <p className="intro">The original encrypted note is preserved independently of your assistant session. Both use the per-user encryption key protected by Windows Credential Manager.</p>
        <div className="workspace-panel"><h2>Encrypted note</h2>
          {!desktop ? <p>Use the Windows desktop app to access encrypted storage.</p> : !ready ? <button className="primary-button" disabled={busy} onClick={initialize}>Initialize / load workspace</button> : <><textarea aria-label="Encrypted private note" className="private-note" maxLength={20000} value={note} onChange={e => setNote(e.target.value)}/><button className="primary-button" disabled={busy} onClick={save}>Save encrypted note</button></>}
          <p role="status" className="workspace-message">{message}</p>
        </div>
        <p className="security-footnote">This is a single encrypted note, not a complete encrypted database or backup/recovery system. Use the assistant’s separate encrypted session controls for conversation and approved memory.</p>
      </section>}
      {page === 'Privacy' && <section className="page compact-page"><div className="eyebrow">USER IN CONTROL</div><h1>Your data.<br/><em>Your decisions.</em></h1><div className="privacy-list">
        <div><span>01</span><section><strong>Local Ollama only</strong><p>The worker connects to 127.0.0.1:11434, disables proxies and redirects, and rejects remote/cloud models. No paid APIs, subscriptions or telemetry are integrated.</p></section></div>
        <div><span>02</span><section><strong>Review actions</strong><p>The model proposes tools; each proposal requires individual review. Desktop input can have real consequences. Check the foreground application before approving.</p></section></div>
        <div><span>03</span><section><strong>Know what is stored</strong><p>Session, preferences, approved memory and history are encrypted when explicitly saved. Files and document backups are ordinary plaintext files in the per-user documents workspace. Screenshots are not included in saved sessions.</p></section></div>
        <div><span>04</span><section><strong>Honest limits</strong><p>This development build is not independently audited. An attacker controlling your unlocked account can access data available to that account. Prompt injection cannot be guaranteed to be detected. Advanced modules remain unfinished; see Availability and the repository checklist.</p></section></div>
      </div></section>}
      <footer className="footer"><span>FRIDAY · FREE LOCAL DEVELOPMENT BUILD</span><span>REVIEW ACTIONS. VERIFY RESULTS.</span></footer>
    </main>
  </div>;
}
