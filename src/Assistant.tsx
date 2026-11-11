import { useEffect, useState } from "react";
import { Channel, invoke } from "@tauri-apps/api/core";

type Message = { role: "user" | "assistant"; content: string };
type StreamEvent = { event: "token" | "done" | "stopped"; content: string };
type ModelInfo = { name: string; size: number; modified_at: string };
type PullEvent = { status: string; completed: number | null; total: number | null; done: boolean };
type ConversationSummary = { id: string; title: string; created_at: number; updated_at: number; message_count: number };
type Conversation = { id: string; title: string; created_at: number; updated_at: number; messages: Message[] };
const ACTIONS = [
  { id: "notepad", label: "Open Notepad", keywords: /\\b(open|launch|start)\\b.*\\b(notepad|text editor)\\b/i },
  { id: "downloads", label: "Open Downloads", keywords: /\\b(open|show)\\b.*\\bdownloads\\b/i },
  { id: "documents", label: "Open Documents", keywords: /\\b(open|show)\\b.*\\bdocuments\\b/i },
  { id: "desktop", label: "Open Desktop folder", keywords: /\\b(open|show)\\b.*\\bdesktop folder\\b/i },
  { id: "youtube", label: "Open YouTube", keywords: /\\b(open|launch|visit)\\b.*\\byoutube\\b/i },
  { id: "browser", label: "Open browser", keywords: /\\b(open|launch|start)\\b.*\\b(browser|chrome|edge|google)\\b/i },
  { id: "email", label: "Compose email", keywords: /\\b(compose|write|send|open)\\b.*\\b(email|mail)\\b/i }
];
const SYSTEM = "You are FRIDAY, a friendly, helpful and slightly humorous local Windows desktop assistant. Be concise, honest and privacy-conscious. Do not claim you performed computer actions unless the app confirms success. You cannot send emails automatically. You can help draft email text for the user to review.";

export default function Assistant() {
  const desktop = "__TAURI_INTERNALS__" in window;
  const [models, setModels] = useState<string[]>([]);
  const [modelDetails, setModelDetails] = useState<ModelInfo[]>([]);
  const [managerOpen, setManagerOpen] = useState(false);
  const [modelToInstall, setModelToInstall] = useState("llama3.2:1b");
  const [pulling, setPulling] = useState(false);
  const [pullStatus, setPullStatus] = useState("");
  const [pullProgress, setPullProgress] = useState<PullEvent | null>(null);
  const [removingModel, setRemovingModel] = useState("");
  const [model, setModel] = useState("llama3.2:1b");
  const [messages, setMessages] = useState<Message[]>([]);
  const [conversations, setConversations] = useState<ConversationSummary[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [activeTitle, setActiveTitle] = useState<string | null>(null);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [historyError, setHistoryError] = useState("");
  const [saving, setSaving] = useState(false);
  const [input, setInput] = useState("");
  const [pending, setPending] = useState(false);
  const [activeRequestId, setActiveRequestId] = useState<string | null>(null);
  const [wasStopped, setWasStopped] = useState(false);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState("");
  const [action, setAction] = useState<(typeof ACTIONS)[number] | null>(null);
  const [actionResult, setActionResult] = useState("");

  async function refreshHistory() {
    if (!desktop) return;
    setHistoryLoading(true);
    try {
      const items = await invoke<ConversationSummary[]>("list_conversations");
      setConversations(items);
      setHistoryError("");
    } catch (e) { setHistoryError(String(e)); }
    finally { setHistoryLoading(false); }
  }

  useEffect(() => { void refreshHistory(); }, []);

  function titleFor(items: Message[]) {
    const first = items.find(m => m.role === "user")?.content.trim() || "New conversation";
    return first.length > 48 ? first.slice(0, 45) + "…" : first;
  }

  async function persist(id: string, items: Message[]) {
    if (!desktop || !id || !items.length) return;
    setSaving(true);
    try {
      await invoke("save_conversation", { id, title: activeTitle || titleFor(items), messages: items });
      await refreshHistory();
      setHistoryError("");
    } catch (e) { setHistoryError("Chat could not be saved: " + String(e)); }
    finally { setSaving(false); }
  }

  async function openConversation(id: string) {
    if (pending || action) return;
    setHistoryLoading(true);
    setHistoryError("");
    try {
      const chat = await invoke<Conversation>("load_conversation", { id });
      setActiveId(chat.id);
      setActiveTitle(chat.title);
      setMessages(chat.messages);
      setError("");
      setActionResult("");
    } catch (e) { setHistoryError(String(e)); }
    finally { setHistoryLoading(false); }
  }

  function newConversation() {
    if (pending) return;
    setActiveId(null);
    setActiveTitle(null);
    setMessages([]);
    setInput("");
    setError("");
    setAction(null);
    setActionResult("");
    setWasStopped(false);
  }

  async function renameConversation(item: ConversationSummary) {
    const title = window.prompt("Choose a name for this conversation:", item.title);
    if (title === null || !title.trim()) return;
    try {
      const chat = await invoke<Conversation>("load_conversation", { id: item.id });
      await invoke("save_conversation", { id: item.id, title: title.trim().slice(0, 160), messages: chat.messages });
      if (activeId === item.id) setActiveTitle(title.trim().slice(0, 160));
      await refreshHistory();
    } catch (e) { setHistoryError(String(e)); }
  }

  async function removeConversation(item: ConversationSummary) {
    if (pending || !window.confirm("Delete this conversation permanently? This cannot be undone.")) return;
    try {
      await invoke("delete_conversation", { id: item.id });
      if (activeId === item.id) newConversation();
      await refreshHistory();
    } catch (e) { setHistoryError(String(e)); }
  }

  async function refresh() {
    if (!desktop || checking) return;
    setChecking(true);
    try {
      const found = await invoke<string[]>("ollama_models");
      setModels(found);
      const details = await invoke<ModelInfo[]>("ollama_model_details");
      setModelDetails(details);
      setModel(current => found.includes(current) ? current : (found[0] || current));
      setError(found.length ? "" : "Ollama is running, but no models are installed. Install a model with Ollama, then press Refresh.");
    } catch (e) { setModels([]); setError(String(e)); }
    finally { setChecking(false); }
  }
  useEffect(() => { void refresh(); }, []);

  async function installModel() {
    const name = modelToInstall.trim();
    if (!name || pulling || !desktop) return;
    setPulling(true); setPullStatus("Connecting to Ollama…"); setPullProgress(null); setError("");
    const channel = new Channel<PullEvent>();
    channel.onmessage = event => { setPullStatus(event.status || "Downloading…"); setPullProgress(event); };
    try {
      await invoke<void>("ollama_pull_model", { name, onEvent: channel });
      setPullStatus("Download completed."); await refresh();
      setModel(name.includes(":") ? name : name + ":latest");
    } catch (e) {
      const message = String(e); setError(message); setPullStatus("Download did not complete.");
      await refresh(); setError(message);
    } finally { setPulling(false); }
  }

  async function removeModel(name: string) {
    if (pulling || pending || removingModel) return;
    if (!window.confirm("Remove " + name + " from Ollama? This deletes its local model files and cannot be undone here.")) return;
    setRemovingModel(name); setError("");
    try { await invoke<void>("ollama_delete_model", { name }); await refresh(); }
    catch (e) { setError(String(e)); }
    finally { setRemovingModel(""); }
  }

  function formatBytes(bytes: number) {
    if (!bytes) return "Size unavailable";
    const units = ["B", "KB", "MB", "GB", "TB"];
    let value = bytes, unit = 0;
    while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit++; }
    return value.toFixed(unit >= 3 ? 1 : 0) + " " + units[unit];
  }

  async function send() {
    const text = input.trim();
    if (!text || pending || action) return;
    const match = ACTIONS.find(a => a.keywords.test(text));
    if (!match && desktop && models.length === 0) {
      setError("FRIDAY cannot start an AI chat yet. Make sure Ollama is running and at least one model is installed, then press Refresh.");
      return;
    }
    setInput(""); setError(""); setActionResult("");
    const next: Message[] = [...messages, { role: "user", content: text }];
    const id = activeId || crypto.randomUUID();
    setActiveId(id); if (!activeTitle) setActiveTitle(titleFor(next)); setMessages(next);
    if (desktop) await persist(id, next);
    if (match) { setAction(match); return; }
    if (!desktop) { setError("AI chat works in the Windows desktop app. This is only the browser preview."); return; }

    setPending(true); setWasStopped(false);
    const requestId = crypto.randomUUID();
    setActiveRequestId(requestId);
    setMessages(prev => [...prev, { role: "assistant", content: "" }]);
    let streamed = "";
    const channel = new Channel<StreamEvent>();
    channel.onmessage = event => {
      if (event.event === "stopped") setWasStopped(true);
      if (event.event === "token") {
        streamed += event.content;
        setMessages(prev => {
          const updated = [...prev], lastIndex = updated.length - 1;
          if (lastIndex >= 0 && updated[lastIndex].role === "assistant") updated[lastIndex] = { ...updated[lastIndex], content: streamed };
          return updated;
        });
      }
    };
    try {
      await invoke<void>("ollama_chat_stream", {
        requestId, model,
        messages: [{ role: "system", content: SYSTEM }, ...next.slice(-18)],
        onEvent: channel
      });
      const finalMessages = [...next, { role: "assistant" as const, content: streamed || "(Empty response)" }];
      setMessages(finalMessages);
      await persist(id, finalMessages);
    } catch (e) {
      setError(String(e));
      const finalMessages = streamed ? [...next, { role: "assistant" as const, content: streamed }] : next;
      setMessages(finalMessages);
      await persist(id, finalMessages);
    } finally { setPending(false); setActiveRequestId(null); }
  }

  async function stopGeneration() {
    if (!activeRequestId) return;
    try { await invoke<boolean>("cancel_ollama_chat", { requestId: activeRequestId }); }
    catch (e) { setError(`Could not stop generation: ${String(e)}`); }
  }

  async function confirmAction() {
    if (!action) return;
    const selected = action; setAction(null);
    if (!desktop) { setError("Desktop actions require the installed Windows app."); return; }
    try {
      const result = await invoke<string>("desktop_action", { action: selected.id });
      setActionResult(result);
      const updated = [...messages, { role: "assistant" as const, content: result }];
      setMessages(updated);
      if (activeId) await persist(activeId, updated);
    } catch (e) { setError(String(e)); }
  }

  return <section className="assistant-page">
    <div className="assistant-head"><div><div className="eyebrow">YOUR LOCAL AI ASSISTANT</div><h1>Talk to <em>FRIDAY.</em></h1><p className="intro">Your conversations are saved privately on this computer.</p></div><button className="secondary-button" onClick={newConversation} disabled={pending}>+ New chat</button></div>
    <div className="chat-layout">
      <aside className="conversation-sidebar" aria-label="Conversation history">
        <div className="conversation-sidebar-head"><strong>CHAT HISTORY</strong><button onClick={() => void refreshHistory()} disabled={historyLoading} aria-label="Refresh chat history">↻</button></div>
        {historyLoading && <small className="history-hint">Loading…</small>}
        {!conversations.length && !historyLoading && <p className="history-hint">Your saved chats will appear here.</p>}
        <div className="conversation-list">{conversations.map(item => <div className={activeId === item.id ? "conversation-item active" : "conversation-item"} key={item.id}>
          <button className="conversation-open" onClick={() => void openConversation(item.id)} disabled={pending}><strong>{item.title}</strong><small>{item.message_count} messages</small></button>
          <div className="conversation-actions"><button onClick={() => void renameConversation(item)} title="Rename chat" aria-label={"Rename " + item.title}>✎</button><button onClick={() => void removeConversation(item)} title="Delete chat" aria-label={"Delete " + item.title}>×</button></div>
        </div>)}</div>
        {historyError && <p className="history-error" role="alert">{historyError}</p>}
      </aside>
      <div className="chat-panel">
        <div className="chat-top"><div className="chat-avatar">F</div><div><strong>FRIDAY AI</strong><small>{checking ? "◌ Checking Ollama…" : models.length ? "● Ollama connected" : "○ Ollama offline or no model installed"}</small></div><select aria-label="Ollama model" value={model} onChange={e => setModel(e.target.value)} disabled={!models.length}>{models.length ? models.map(m => <option key={m}>{m}</option>) : <option value={model}>{model}</option>}</select><button className="secondary-button" onClick={() => setManagerOpen(open => !open)} disabled={!desktop || pulling}>{managerOpen ? "Close manager" : "Manage models"}</button><button className="secondary-button" onClick={() => void refresh()} disabled={checking || pulling}>{checking ? "Checking…" : "Refresh"}</button></div>
        {managerOpen && <section className="model-manager" aria-label="Ollama model manager">
          <div className="model-manager-heading"><div><strong>Local model manager</strong><p>Models are stored by Ollama on this computer. Downloads may be several gigabytes.</p></div></div>
          <div className="model-install-row"><input aria-label="Model name to install" value={modelToInstall} onChange={e => setModelToInstall(e.target.value)} placeholder="e.g. llama3.2:1b" disabled={pulling} /><button className="primary-button" onClick={() => void installModel()} disabled={pulling || !modelToInstall.trim()}>{pulling ? "Downloading…" : "Download model"}</button></div>
          <p className="model-safety-note">Use an official Ollama model name. Check available disk space and your computer's RAM before downloading. FRIDAY sends the model name to your local Ollama service only.</p>
          {pullStatus && <div className="model-download-status" role="status"><div><strong>{pullStatus}</strong>{pullProgress?.total ? <span>{Math.round(((pullProgress.completed || 0) / pullProgress.total) * 100)}%</span> : null}</div>{pullProgress?.total ? <progress max={pullProgress.total} value={pullProgress.completed || 0}/> : <div className="model-progress-indeterminate"/>}{pullProgress?.total ? <small>{formatBytes(pullProgress.completed || 0)} of {formatBytes(pullProgress.total)}</small> : null}</div>}
          <div className="model-list-heading"><strong>Installed models</strong><span>{modelDetails.length}</span></div>
          {modelDetails.length ? <div className="installed-model-list">{modelDetails.map(item => <div className="installed-model" key={item.name}><div><strong>{item.name}</strong><small>{formatBytes(item.size)} · {item.name === model ? "Selected for chat" : "Installed locally"}</small></div><button className="model-remove-button" onClick={() => void removeModel(item.name)} disabled={pulling || pending || !!removingModel}>{removingModel === item.name ? "Removing…" : "Remove"}</button></div>)}</div> : <p className="model-empty">No installed models found. Download one above to start chatting.</p>}
        </section>}
        <div className="chat-history" role="log" aria-live="polite">
          {!messages.length && <div className="chat-welcome"><div className="chat-welcome-icon">✦</div><h2>How can I help?</h2><p>Ask me anything, or try “Open Notepad”, “Open Downloads”, or “Compose email”. I'll ask before touching your desktop.</p></div>}
          {messages.map((m, i) => <div key={i} className={`chat-bubble ${m.role}`}><span>{m.content || (pending && m.role === "assistant" ? "FRIDAY is thinking…" : "")}</span></div>)}
        </div>
        {action && <div className="action-confirm" role="alertdialog" aria-label="Confirm desktop action"><strong>Allow FRIDAY to {action.label.toLowerCase()}?</strong><p>Nothing will happen until you approve. Email will open a draft, not send it.</p><div><button className="primary-button" onClick={() => void confirmAction()}>Allow once</button><button className="secondary-button" onClick={() => setAction(null)}>Cancel</button></div></div>}
        {error && <div className="chat-error" role="alert">{error}</div>}
        {wasStopped && <div className="chat-success" role="status">Response stopped.</div>}
        {actionResult && <div className="chat-success" role="status">{actionResult}</div>}
        <div className="chat-compose"><textarea aria-label="Message FRIDAY" placeholder="Message FRIDAY…" value={input} onChange={e => setInput(e.target.value)} onKeyDown={e => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); void send(); } }} rows={2} maxLength={20000}/>{pending ? <button className="secondary-button stop-generation" onClick={() => void stopGeneration()} aria-label="Stop response">■ Stop</button> : <button className="primary-button" disabled={!input.trim() || !!action} onClick={() => void send()} aria-label="Send message">↑ Send</button>}</div>
        <p className="chat-footnote">Local AI · Encrypted chat history · Desktop actions require confirmation · {saving ? "Saving…" : "Saved on this device"} · Emails are never sent automatically</p>
      </div>
    </div>
  </section>;
}
