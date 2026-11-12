# FRIDAY Master Development Checklist

> Living project checklist for the `feature/phase-2-skills` development branch. Update this document as features are implemented and verified. Do not mark a feature complete merely because its screen or placeholder exists.

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

Branch: `feature/phase-2-skills`

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
- [x] Add local Ollama model download/pull flow with streamed progress and disk-space/network warnings (implementation committed; Windows verification pending).
- [x] List installed models with local disk sizes (implementation committed; Windows verification pending).
- [x] Remove installed models through Ollama after explicit user confirmation (implementation committed; Windows verification pending).
- [ ] Support configurable local endpoint safely, defaulting to localhost.
- [x] Implement incremental Ollama response streaming from Rust to the chat UI, including chunked NDJSON parsing and visible partial responses.
- [x] Streaming busy/error states and request cancellation are implemented; Windows end-to-end verification remains pending.
- [ ] Test small and larger models on representative hardware.
- [ ] Document optional cloud providers separately; never silently send local data to them.

### 1C. Chat and conversation storage
- [x] Basic user/assistant chat interface.
- [x] Conversation list, create, rename and delete (implemented; Windows verification pending; archive remains a later enhancement).
- [x] Persist conversations locally across restart using authenticated XChaCha20-Poly1305 encryption and the existing Windows Credential Manager key (implementation committed; Windows verification pending).
- [x] Search conversation history by conversation title (implementation committed; Windows verification pending).
- [ ] Export and delete chat data.
- [ ] Context-window management and conversation summarisation.
- [ ] Clear indication of the active model and where processing occurs.

### 1D. Workspace and personal memory
- [~] Encrypted single-note proof of concept.
- [ ] Design and implement a proper local database and migrations.
- [x] Store user-entered, user-approved memory entries separately from chat transcripts in an encrypted local file using the existing Windows Credential Manager key (implementation committed; Windows verification pending).
- [x] Add personal memory review, correction, enable/disable and delete controls (implementation committed; Windows verification pending).
- [x] Include enabled user-approved memory as bounded context in local Ollama chat prompts; memory text is explicitly treated as context, not instructions.
- [ ] Add natural-language memory suggestions and a clear approval step before saving; no automatic extraction or silent saving is enabled.
- [ ] Add memory export and clear-all controls.
- [ ] Add document indexing and retrieval with source references.
- [ ] Add data export, backup/restore and deletion.
- [ ] Document key loss and recovery limitations before promising recovery.

### 1E. Skills platform
- [x] Built-in catalogue with four declarative skill packages.
- [x] Versioned manifests with ID, version, description, instruction entry point, dependencies and declared permissions.
- [x] Install package instruction files to the local app-data skills folder; preserve compatibility with the legacy registry format.
- [x] Verify built-in package identity, compatibility and SHA-256 instruction integrity before enabling/loading.
- [x] Separate installed-on-disk, enabled and loaded-in-session states.
- [x] Explicit load/unload controls; loaded instructions are fetched only when a chat request starts.
- [x] No package scripts are executed and no skills load automatically in the background.
- [x] Safe package path checks and uninstall cleanup for each skill package directory.
- [~] Dependency metadata is supported; current built-in packages have no external dependencies.
- [~] Catalogue remains a Rust built-in list; third-party catalogue/update/signing support is deferred.
- [ ] Add skill-specific settings and data cleanup for future skills.
- [ ] Connect each skill to real permissioned tools and end-to-end workflows.
- [ ] Add signed remote package distribution and safe updates before accepting third-party packages.

### 1F. Tool execution and permissions
- [x] Central allowlisted tool registry with structured JSON arguments and per-tool permission declarations.
- [x] Safe local calculator tool (basic arithmetic parser; no eval, shell or network).
- [x] Desktop destination tool with fixed target allowlist and explicit per-action approval gate.
- [x] Frontend uses the central executor rather than the legacy desktop-action command.
- [x] Encrypted, persistent user-visible tool activity history (last 500 events), with a clear-history control; no telemetry upload.
- [x] Reject unknown tool IDs, unknown desktop targets, unsupported calculator syntax and unapproved desktop actions.
- [x] Permission tiers are represented per tool; only the calculator and fixed desktop-launch tools are currently enabled. File/network permissions remain deliberately unavailable until their dedicated skills are implemented.
- [x] Send the registered calculator and fixed desktop tool schemas to Ollama during streamed chat requests.
- [x] Parse model-proposed tool calls and show their structured arguments in an explicit approval panel; unknown tool IDs are blocked by the frontend catalogue check and backend allowlist.
- [x] Approved tool results are returned to Ollama in a follow-up chat turn so FRIDAY can finish the original request.
- [x] Follow-up tool proposals are supported with a maximum of three approved tool rounds per user request; tool proposal payloads are limited to 8 KB.
- [x] Approved tool results are handed back to Ollama through a bounded app-generated context message; native Ollama tool-role transcript support remains a later compatibility improvement.
- [x] Desktop actions require a short-lived, one-use backend-issued approval token bound to the exact tool ID and JSON arguments; mismatched, expired and replayed tokens are rejected. The UI confirmation remains the user-consent step; the token is not a security boundary against a compromised frontend.
- [x] Tool activity is stored locally in an encrypted file using the existing workspace key, bounded to 500 events, and can be cleared from the UI.

## Phase 2 — FRIDAY's original skills

### File Assistant Stage 6 — comprehensive safety, reliability and edge-case review
- [x] Revalidate the selected folder root on every granted operation; reject a root that is no longer the same normal directory or has been redirected after approval.
- [x] Reject Windows reparse points as well as symbolic links for selected files, folders, destination parents and backup locations; add platform-specific symlink checks where the runner permits.
- [x] Revoke folder grants and pending edit/move/undo previews when selecting a different folder, switching to individual-file selection, or disabling, unloading or uninstalling File Assistant; backend file commands now require the skill to be loaded.
- [x] Review crash/interruption recovery for edit, move, backup and undo transactions. Durable forward-operation journals and flushed Undo-intent markers are implemented; interrupted states are classified and ambiguous cases are left untouched. Rust regression tests and Windows CI passed in Run #250; real-world power-loss fault injection remains part of final Windows acceptance testing.
- [x] Allow undo to restore from an intact backup when the edited/moved working copy is missing, provided the destination/original location is safe and unoccupied. The restore path uses a flushed temporary file and an atomic no-overwrite hard-link install; regression tests cover successful restoration, occupied-path refusal and permission preservation. Run #250 passed.
- [x] Add a transaction journal/recovery workflow for crashes between backup creation, file replacement/move, undo and history persistence. Edit/move transactions have durable journal records and phase markers; Undo has a durable intent marker. The File Assistant UI lists recoverable transactions and requires explicit confirmation. Safe states can restore a verified backup or rebuild Undo history; ambiguous move/undo states require manual review. Run #250 passed.
- [x] Bound backup storage growth at 500 backup-folder files or 100 MB; refuse new edits/moves at the limit and never automatically delete recovery backups. Manual review/export and a future user-controlled cleanup workflow remain preferable to silent deletion.
- [x] File edit/move results now distinguish a completed file change from a failure to persist undo history; the UI warns that the original backup remains but one-click Undo may be unavailable.
- [x] Undo completion markers are flushed to disk; if a marker cannot be written, FRIDAY detects an already-restored file from its backup and avoids presenting it as undoable. Full crash-transaction recovery remains pending.
- [x] Added tests for approved-root validation, undo completion detection without a marker, backup integrity and corrupt-backup refusal, path traversal and backup-directory redirection, missing-file restoration/no-overwrite and permission preservation, transaction ID and semantic journal validation, phase-marker durability, interrupted edit/move classification, rollback-completed edits, ambiguous move states, approval expiry/mismatch/one-use behavior, no-op edit rejection and reserved backup capacity. Run #250 passed the frontend build, Rust unit tests, Windows installer build and artifact upload. Real-world Windows fault-injection/acceptance testing remains in the final release phase.
- [x] Add SHA-256 integrity metadata for newly created undo backups and validate UTF-8/size/hash before restoration; legacy Stage 5 records remain readable with UTF-8/size validation but cannot retroactively verify an original backup hash. Continue reviewing temporary-file cleanup, permissions and audit-history consistency.
- [x] Run final frontend build, Rust tests and Windows installer CI against the complete Stage 6 implementation. Run #250 (commit `b33aa06d7c334b6cd2a910cc7da3e30097ebefff`) passed all steps: frontend build, Rust unit tests, Windows installer build and artifact upload. Keep hands-on Windows user acceptance testing for the final release phase.

Implement each as a real end-to-end capability, including permissions, errors, settings, data handling and Windows tests.

- [~] **PDF Reader** — native Windows file picker, local text extraction with page numbers, extracted-text preview, and local Ollama Q&A with page-citation instructions are implemented. Mixed documents can be queried when some pages contain readable text; pages without selectable text are identified. Encrypted/damaged PDFs return a clear error; file size (25 MB), page count (500), per-page decompression (2 MB) and extracted text (120,000 UTF-8 bytes) are bounded. A real minimal-PDF extraction fixture test has been added. OCR, stronger parser isolation/hardening, latest Rust/TypeScript/Windows build results and end-to-end Windows verification remain pending.
- [~] **Web Research** — multi-source implementation is present: user-initiated DuckDuckGo HTML search; up to four explicitly selected HTTPS sources; bounded downloads and extraction; public-IP/DNS validation with pinned connections; manual redirect revalidation; HTML5 text extraction; BOM/header/meta charset decoding; publisher-declared author/date metadata; local Ollama synthesis with source labels; and visible citation-limit warning. CI Run #137 passed frontend build, Rust unit tests, Windows installer build and artifact upload. Remaining: live provider/page compatibility checks, deeper source-claim review, Windows end-to-end acceptance, and ongoing security review. Citation-label validation does not prove that a cited page supports a claim.
- [~] **File Assistant** — Stages 1–5 implemented on the development branch. CI verification: Stage 2 Run #158, Stage 3 Run #176, Stage 4 Run #183, and Stage 5 Run #191 passed frontend build, Rust unit tests, Windows installer build and artifact upload. Folder scanning is bounded and session-scoped; text reading is limited; edits and moves require previews and explicit one-use approval; originals are backed up; undo history supports approved edits and moves and protects newer changes. Windows desktop acceptance testing remains pending. Stage 6 comprehensive safety, reliability and edge-case review is now in progress.
- [ ] **Coding Assistant** — inspect selected project files, explain and propose edits; sandbox tests/commands with strict time/resource limits; show diffs and require approval before applying changes.
- [ ] **Excel Intelligence (newly requested)** — a dedicated FRIDAY skill for installed Microsoft Excel on Windows. Research open-source GitHub blueprints before implementation; support accounting/finance first, plus general office/administration, sales, inventory, data analysis, reporting, dashboards, formulas and controlled automation. Use actual Excel recalculation where available, independent formula checks, previews, backups, approval and undo. Do not promise 100% accuracy; measure accuracy with a growing workbook evaluation suite.
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

## Current next work item

**Phase 2C — File Assistant Stage 6:** complete the safety, reliability and edge-case review above before moving on to the Coding Assistant. Excel Intelligence is explicitly recorded as a planned FRIDAY skill; its open-source blueprint research and implementation will follow the agreed skill-development order.

## Core platform implementation notes

**Phase 1C — Persistent chat history:** encrypted local conversation storage, conversation list/create/switch/rename/delete, title search and persistence of completed or stopped responses are implemented in the development branch. Rust/TypeScript checks and Windows end-to-end verification remain pending.

**Phase 1F — Central Tool Execution and Permission System:** the allowlisted registry exposes safe local arithmetic and fixed desktop destinations. Streamed Ollama requests include those tool schemas; model-proposed calls are shown for explicit user approval, with a maximum of three approved tool rounds and an 8 KB proposal limit. Desktop execution requires a short-lived, one-use backend-issued token bound to the exact tool and arguments; mismatches, expiry and replay are rejected. The token supplements, but does not replace, the visible user confirmation and is not a security boundary against a compromised frontend. Tool activity is persisted locally in an encrypted, bounded audit file and can be cleared in the UI. The handoff still uses bounded app-generated assistant context rather than a native Ollama tool-role transcript. File read/write tools and sandboxed code execution are intentionally deferred to the File Assistant and Coding Assistant skills, where path grants, diffs, and resource limits can be designed with the actual workflows. Rust/TypeScript checks and Windows verification remain pending.

**Phase 1E — Proper Skills Platform:** four versioned built-in declarative packages can be installed locally, enabled, explicitly loaded/unloaded and integrity-checked. Loaded instructions are added to the next local chat prompt; packages do not execute scripts and do not yet provide their advertised tools. Automated and Windows verification remain pending.

**Phase 1D — Persistent personal memory:** user-entered memory entries are stored separately from transcripts in an encrypted local file. The chat UI supports add, edit, enable/disable and delete; enabled entries are included as bounded context in local Ollama prompts. No memory is silently extracted or saved. Rust/TypeScript checks and Windows end-to-end verification remain pending.
