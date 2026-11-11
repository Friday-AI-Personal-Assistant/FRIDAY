# FRIDAY Master Development Checklist

> Living project checklist for the `feature/v0.2-local-ollama-chat` development branch. Update this document as features are implemented and verified. Do not mark a feature complete merely because its screen or placeholder exists.

## Project direction

Build FRIDAY into a local-first, modular Windows personal AI assistant. Keep the existing React + TypeScript + Tauri 2 + Rust architecture. Use OpenJarvis as a capability and architecture reference, adapting ideas to FRIDAY rather than copying its Python implementation wholesale.

## Status definitions

- [ ] **Pending** — not implemented.
- [~] **In progress / partial** — some code or UI exists, but the complete capability is not ready.
- [x] **Implemented** — implementation exists; this alone does not mean it passed testing.
- [x] **Verified** — the defined test has passed on the target Windows desktop.
- **Blocked** — waiting on an external account, permission, service, hardware, or decision.

Every feature must have both an implementation status and a test status. Keep those separate.

## Baseline audit — current development branch

Branch: `feature/v0.2-local-ollama-chat`

Comparison with `main`: 15 commits ahead, 0 behind at the time of this audit. Continue development on this branch; do not commit feature work to `main` or merge without explicit approval.

| Area | What exists in source | Current assessment |
|---|---|---|
| Desktop shell | Tauri 2 + React + TypeScript + Vite navigation and pages | Implemented; basic navigation was manually tested in an earlier installed build |
| Private workspace | One encrypted test note using XChaCha20-Poly1305; key stored via Windows Credential Manager | Partial foundation; not a full database, backup, export/import or recovery system |
| Ollama | Rust commands to list local models and send non-streaming chat requests to `127.0.0.1:11434`; model selector in chat UI | Early integration present; current branch build and end-to-end test still required |
| Chat experience | User/assistant messages held in React component state; simple allowlisted desktop-action prompts with confirmation | Partial; chat is lost on refresh/restart, no tool-calling framework or conversation management |
| Skills Store | Four hard-coded catalogue entries and local installed/enabled registry | Foundation only; install currently records state, does not download packages or provide working skill tools |
| Email, Job Finder, CV Studio, Voice | Listed as planned items in Overview and Product Plan | Not implemented |
| Persistent memory | No memory/retrieval subsystem found in audited source files | Not implemented |
| Settings and hardware detection | Settings page is a placeholder; no hardware/model recommendation system found | Not implemented |
| Automated verification | No successful current-branch build or CI result was confirmed during this audit | Must verify before release |

**Important wording:** Earlier manual tests of navigation, workspace initialisation, saving a note and persistence after restart were reported as passing on an earlier Windows build. They do not verify the newer Ollama or Skills Store changes.

## Phase 0 — Project control and requirements

- [x] Confirm the development branch exists and is ahead of `main`.
- [x] Audit the current core files and existing Product Plan / Privacy principles.
- [x] Create this master checklist.
- [ ] Update README so its status description matches the current development branch.
- [ ] Record exact supported Windows versions and minimum hardware requirements.
- [ ] Create a repeatable development/build/test procedure.
- [ ] Add automated frontend and Rust checks to the development workflow.
- [ ] Maintain a change log and update this checklist with every feature commit.

## Phase 1 — Reliable core platform (build before many skills)

### 1A. Local AI engine
- [~] Detect whether Ollama is reachable.
- [~] List locally installed Ollama models.
- [~] Select a model in the chat UI.
- [x] Improve connection errors and set separate connection/request timeouts; richer Ollama HTTP error messages are returned. (Implemented; Windows verification pending.)
- [~] Improve model refresh feedback and sort/deduplicate installed model names. Model details (size, family, context) remain pending.
- [ ] Add model download/pull flow with clear disk-space and network warnings.
- [ ] Support configurable local endpoint safely, defaulting to localhost.
- [x] Implement incremental Ollama response streaming from Rust to the chat UI, including chunked NDJSON parsing and visible partial responses.
- [~] Streaming busy/error states are implemented; Windows end-to-end verification and request cancellation remain pending.
- [ ] Test small and larger models on representative hardware.
- [ ] Document optional cloud providers separately; never silently send local data to them.

### 1B. Chat and conversation storage
- [~] Basic user/assistant chat interface.
- [ ] Conversation list, create, rename, archive and delete.
- [ ] Persist conversations locally across restart.
- [ ] Search conversation history.
- [ ] Export and delete chat data.
- [ ] Context-window management and conversation summarisation.
- [ ] Clear indication of the active model and where processing occurs.

### 1C. Workspace and personal memory
- [~] Encrypted single-note proof of concept.
- [ ] Design and implement a proper local database and migrations.
- [ ] Store preferences and user-approved profile facts separately from chat transcripts.
- [ ] Add explicit memory review, correction, disable and delete controls.
- [ ] Add document indexing and retrieval with source references.
- [ ] Add data export, backup/restore and deletion.
- [ ] Document key loss and recovery limitations before promising recovery.

### 1D. Skills platform
- [~] Built-in catalogue with four initial entries.
- [~] Local installed/enabled registry.
- [ ] Correct UI wording so registry selection is not described as downloading a package.
- [ ] Define a versioned skill manifest (ID, version, description, entry point, dependencies, permissions).
- [ ] Add install source, integrity/signature checks and safe update/uninstall.
- [ ] Separate installed-on-disk from loaded-in-memory states.
- [ ] Load skill instructions and tools only when needed; no background startup by default.
- [ ] Add dependency checks and clear install errors.
- [ ] Add skill-specific settings and data cleanup.
- [ ] Make the catalogue extensible without hard-coding every skill into the UI.

### 1E. Tool execution and permissions
- [~] A small desktop-action allowlist with per-action confirmation.
- [ ] Build a central tool registry and structured tool-calling loop.
- [ ] Permission levels: no access, read selected data, write with approval, external/network access.
- [ ] Ask before sending messages, modifying/deleting files, running commands or submitting forms.
- [ ] Use sandboxing and strict time/resource limits for code execution.
- [ ] Log user-visible action history without silently uploading telemetry.
- [ ] Reject arbitrary shell commands and arbitrary filesystem/network access by default.

## Phase 2 — FRIDAY's original skills

Implement each as a real end-to-end capability, including permissions, errors, settings, data handling and Windows tests.

- [ ] **PDF Reader** — open user-selected PDFs, extract text, summarise, answer questions with page references; handle scanned PDFs via optional OCR.
- [ ] **Web Research** — search and open sources, summarise with citations, distinguish source facts from model inference.
- [ ] **File Assistant** — user-selected folders/files, search and summarise; preview changes; require approval before write, move or delete.
- [ ] **Coding Assistant** — inspect selected project files, explain and propose edits; sandbox tests/commands; show diffs and require approval before applying changes.
- [ ] **Email Management** — official OAuth, least-privilege scopes, inbox summaries, search/labels, draft replies; explicit approval before sending; disconnect and revoke access.
- [ ] **Job Finder** — permitted job sources, saved searches and alerts, location/preferences filters, duplicate detection and application tracking; no unauthorised scraping or mass applications.
- [ ] **CV Studio** — import PDF/DOCX, edit and preview, ATS-friendly templates, export; never invent qualifications or work history.
- [ ] **Voice Assistant** — optional push-to-talk speech input and spoken output, visible microphone state, local processing where practical, clear permission controls.
- [ ] **Personality and Preferences** — tone, language, response length and user-defined preferences, saved locally.

## Phase 3 — OpenJarvis-inspired capability expansion

These are capability areas to adapt, not a promise to reproduce every OpenJarvis integration or backend. Select compatible, maintainable components for FRIDAY's Rust/Tauri architecture.

### Everyday tools
- [ ] Calculator and safe date/time/unit utilities.
- [ ] Controlled web search and webpage reading.
- [ ] Python execution in a restricted sandbox.
- [ ] User-approved file and document tools.
- [ ] Structured data extraction and report generation.

### Knowledge and memory
- [ ] Local document library and indexing.
- [ ] Keyword and semantic retrieval; evaluate lightweight local options.
- [ ] Hybrid retrieval/reranking when useful and affordable.
- [ ] Source citations and traceability for retrieved information.
- [ ] User-controlled long-term memory and profile.

### Agents and workflows
- [ ] Reliable task planner with visible steps and progress.
- [ ] ReAct-style tool-use loop with approval gates.
- [ ] Research workflow that gathers and cites sources.
- [ ] Multi-step workflows with checkpoints, cancellation and recovery.
- [ ] Multi-agent coordination only where it provides measurable value.
- [ ] Scheduled tasks and reminders, opt-in and visible.
- [ ] Proactive monitoring only when explicitly configured.

### Integrations
- [ ] Define a secure connector framework and per-connector permissions.
- [ ] Calendar and contacts.
- [ ] Cloud drives and document providers.
- [ ] Messaging platforms (evaluate WhatsApp, Telegram, Discord, Slack, Teams, Signal individually).
- [ ] Additional email providers and productivity services.
- [ ] Make each connector optional, revocable and transparent about data leaving the device.

### Evaluation and observability
- [ ] User-visible activity history and diagnostics.
- [ ] Local evaluation tests for tool correctness and model quality.
- [ ] Performance measurements (latency, memory, CPU/GPU) with user consent.
- [ ] No silent analytics or diagnostic uploads.

## Phase 4 — Advanced assistant and optimisation

- [ ] Hardware detection and model recommendations.
- [ ] Model profiles for low-memory, balanced and high-performance devices.
- [ ] Unload idle models/skills where supported; avoid unnecessary background services.
- [ ] Resource limits and cancellation for long-running tasks.
- [ ] Optional model routing based on task and user settings.
- [ ] Reliable workflow resume/retry and clear failure recovery.
- [ ] Optional cloud models only with explicit provider and data-flow disclosure.

## Phase 5 — Security, privacy and release readiness

- [ ] Threat model covering local malware, unlocked Windows account, malicious documents, prompt injection and compromised skill packages.
- [ ] Review every tool's filesystem, process and network permissions.
- [ ] Protect OAuth tokens and secrets with OS credential storage.
- [ ] Verify package integrity and trusted update/signing process.
- [ ] Test prompt-injection resistance for webpages, PDFs, email and other untrusted content.
- [ ] Test permission bypass attempts and destructive-action confirmation.
- [ ] Verify data export, deletion, backup and restore.
- [ ] Review privacy notices and document all external data flows.
- [ ] No claims of absolute security; obtain independent review before making strong security claims.

## Phase 6 — Verification and Windows release

- [ ] TypeScript build and lint/checks pass for the latest changes.
- [ ] Rust formatting, new Ollama request-validation unit tests, and build pass for the latest changes.
- [ ] Tauri Windows installer build passes on the development branch.
- [ ] Install on a clean supported Windows environment.
- [ ] Test Ollama missing, running, stopped and model-not-installed cases.
- [ ] Test chat, history, memory and restart persistence.
- [ ] Test each installed skill's success, failure, permissions and uninstall.
- [ ] Test external integrations with test accounts and revoke access afterward.
- [ ] Test offline operation for features advertised as local.
- [ ] Test low-resource hardware and long-running task cancellation.
- [ ] Verify upgrade and uninstall behaviour without losing user data unexpectedly.
- [ ] Brother/friend completes the real-world acceptance checklist.
- [ ] Fix all release-blocking issues and document known limitations.
- [ ] Build and publish the final approved Windows installer.
- [ ] Only merge to `main` after explicit approval.

## OpenJarvis reference map

Use the upstream project as a design reference and verify current upstream documentation before implementing a specific capability:

- Repository: https://github.com/open-jarvis/OpenJarvis
- Architecture overview: https://github.com/open-jarvis/OpenJarvis/blob/main/docs/architecture/overview.md
- Skills architecture: https://github.com/open-jarvis/OpenJarvis/blob/main/docs/architecture/skills.md
- Skills guide: https://github.com/open-jarvis/OpenJarvis/blob/main/docs/user-guide/skills.md
- Security guide: https://github.com/open-jarvis/OpenJarvis/blob/main/docs/user-guide/security.md

Do not copy code or dependencies without checking their licences and compatibility. Preserve required notices. Prefer adapting concepts to FRIDAY's native stack.

## Immediate next work item

**Phase 1A — complete Ollama foundation:** run frontend/Rust checks and verify streaming on Windows with Ollama running, stopped and without an installed model. Add request cancellation and a safe model download flow. After that, proceed through chat persistence, memory, skills architecture and permissioned tool execution before implementing the larger skill catalogue.
