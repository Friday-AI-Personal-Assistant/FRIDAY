import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type Message = { role: "user" | "assistant"; content: string };
const ACTIONS = [
  { id: "notepad", label: "Open Notepad", keywords: /\b(open|launch|start)\b.*\b(notepad|text editor)\b/i },
  { id: "downloads", label: "Open Downloads", keywords: /\b(open|show)\b.*\bdownloads\b/i },
  { id: "documents", label: "Open Documents", keywords: /\b(open|show)\b.*\bdocuments\b/i },
  { id: "desktop", label: "Open Desktop folder", keywords: /\b(open|show)\b.*\bdesktop folder\b/i },
  { id: "youtube", label: "Open YouTube", keywords: /\b(open|launch|visit)\b.*\byoutube\b/i },
  { id: "browser", label: "Open browser", keywords: /\b(open|launch|start)\b.*\b(browser|chrome|edge|google)\b/i },
  { id: "email", label: "Compose email", keywords: /\b(compose|write|send|open)\b.*\b(email|mail)\b/i }
];
const SYSTEM = "You are FRIDAY, a friendly, helpful and slightly humorous local Windows desktop assistant. Be concise, honest and privacy-conscious. Do not claim you performed computer actions unless the app confirms success. You cannot send emails automatically. You can help draft email text for the user to review.";

export default function Assistant() {
  const desktop = "__TAURI_INTERNALS__" in window;
  const [models, setModels] = useState<string[]>([]);
  const [model, setModel] = useState("llama3.2:1b");
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [pending, setPending] = useState(false);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState("");
  const [action, setAction] = useState<(typeof ACTIONS)[number] | null>(null);
  const [actionResult, setActionResult] = useState("");

  async function refresh() {
    if (!desktop || checking) return;
    setChecking(true);
    try {
      const found = await invoke<string[]>("ollama_models");
      setModels(found);
      setModel(current => found.includes(current) ? current : (found[0] || current));
      setError(found.length ? "" : "Ollama is running, but no models are installed. Install a model with Ollama, then press Refresh.");
    } catch (e) {
      setModels([]);
      setError(String(e));
    } finally {
      setChecking(false);
    }
  }

  useEffect(() => { void refresh(); }, []);

  async function send() {
    const text = input.trim();
    if (!text || pending || action) return;

    const match = ACTIONS.find(a => a.keywords.test(text));
    if (!match && desktop && models.length === 0) {
      setError("FRIDAY cannot start an AI chat yet. Make sure Ollama is running and at least one model is installed, then press Refresh.");
      return;
    }

    setInput("");
    setError("");
    setActionResult("");
    const next: Message[] = [...messages, { role: "user", content: text }];
    setMessages(next);

    if (match) { setAction(match); return; }
    if (!desktop) { setError("AI chat works in the Windows desktop app. This is only the browser preview."); return; }

    setPending(true);
    try {
      const answer = await invoke<string>("ollama_chat", {
        model,
        messages: [{ role: "system", content: SYSTEM }, ...next.slice(-18)]
      });
      setMessages(prev => [...prev, { role: "assistant", content: answer || "(Empty response)" }]);
    } catch (e) {
      setError(String(e));
    } finally {
      setPending(false);
    }
  }

  async function confirmAction() {
    if (!action) return;
    const selected = action;
    setAction(null);
    if (!desktop) { setError("Desktop actions require the installed Windows app."); return; }
    try {
      const result = await invoke<string>("desktop_action", { action: selected.id });
      setActionResult(result);
      setMessages(prev => [...prev, { role: "assistant", content: result }]);
    } catch (e) {
      setError(String(e));
    }
  }

  return <section className="assistant-page">
    <div className="assistant-head"><div><div className="eyebrow">YOUR LOCAL AI ASSISTANT</div><h1>Talk to <em>FRIDAY.</em></h1><p className="intro">Private conversations and simple desktop actions, powered by Ollama.</p></div><button className="secondary-button" onClick={() => { setMessages([]); setError(""); setAction(null); }}>+ New chat</button></div>
    <div className="chat-panel">
      <div className="chat-top"><div className="chat-avatar">F</div><div><strong>FRIDAY AI</strong><small>{checking ? "◌ Checking Ollama…" : models.length ? "● Ollama connected" : "○ Ollama offline or no model installed"}</small></div><select aria-label="Ollama model" value={model} onChange={e => setModel(e.target.value)} disabled={!models.length}>{models.length ? models.map(m => <option key={m}>{m}</option>) : <option value={model}>{model}</option>}</select><button className="secondary-button" onClick={() => void refresh()} disabled={checking}>{checking ? "Checking…" : "Refresh"}</button></div>
      <div className="chat-history" role="log" aria-live="polite">
        {!messages.length && <div className="chat-welcome"><div className="chat-welcome-icon">✦</div><h2>How can I help?</h2><p>Ask me anything, or try “Open Notepad”, “Open Downloads”, or “Compose email”. I'll ask before touching your desktop.</p></div>}
        {messages.map((m, i) => <div key={i} className={`chat-bubble ${m.role}`}><span>{m.content}</span></div>)}
        {pending && <div className="chat-bubble assistant">FRIDAY is thinking…</div>}
      </div>
      {action && <div className="action-confirm" role="alertdialog" aria-label="Confirm desktop action"><strong>Allow FRIDAY to {action.label.toLowerCase()}?</strong><p>Nothing will happen until you approve. Email will open a draft, not send it.</p><div><button className="primary-button" onClick={() => void confirmAction()}>Allow once</button><button className="secondary-button" onClick={() => { setAction(null); setActionResult("Action cancelled."); }}>Cancel</button></div></div>}
      {error && <div className="chat-error" role="alert">{error}</div>}
      {actionResult && <div className="chat-success" role="status">{actionResult}</div>}
      <div className="chat-compose"><textarea aria-label="Message FRIDAY" placeholder="Message FRIDAY…" value={input} onChange={e => setInput(e.target.value)} onKeyDown={e => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); void send(); } }} rows={2} maxLength={20000}/><button className="primary-button" disabled={!input.trim() || pending || !!action} onClick={() => void send()} aria-label="Send message">↑ Send</button></div>
      <p className="chat-footnote">Local AI · Chat is held in memory for this session · Desktop actions require confirmation · Emails are never sent automatically</p>
    </div>
  </section>;
}
