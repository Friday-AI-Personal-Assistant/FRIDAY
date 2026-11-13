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
- [x] Undo completion markers are flushed to disk; if a marker cannot be written, FRIDAY detects an already-restored file from its backup and avoids presenting it as undoable. Interrupted transaction states are classified; ambiguous states are deliberately left untouched for manual review.
- [x] Added tests for approved-root validation, undo completion detection without a marker, backup integrity and corrupt-backup refusal, path traversal and backup-directory redirection, missing-file restoration/no-overwrite and permission preservation, transaction ID and semantic journal validation, phase-marker durability, interrupted edit/move classification, rollback-completed edits, ambiguous move states, approval expiry/mismatch/one-use behavior, no-op edit rejection and reserved backup capacity. Run #250 passed the frontend build, Rust unit tests, Windows installer build and artifact upload. Real-world Windows fault-injection/acceptance testing remains in the final release phase.
- [x] Add SHA-256 integrity metadata for newly created undo backups and validate UTF-8/size/hash before restoration; legacy Stage 5 records remain readable with UTF-8/size validation but cannot retroactively verify an original backup hash. Continue reviewing temporary-file cleanup, permissions and audit-history consistency.
- [x] Run final frontend build, Rust tests and Windows installer CI against the complete Stage 6 implementation. Run #250 (commit `b33aa06d7c334b6cd2a910cc7da3e30097ebefff`) passed all steps: frontend build, Rust unit tests, Windows installer build and artifact upload. Keep hands-on Windows user acceptance testing for the final release phase.

Implement each as a real end-to-end capability, including permissions, errors, settings, data handling and Windows tests.

- [~] **PDF Reader** — native Windows file picker, local text extraction with page numbers, extracted-text preview, and local Ollama Q&A with page-citation instructions are implemented. Mixed documents can be queried when some pages contain readable text; pages without selectable text are identified. Encrypted/damaged PDFs return a clear error; file size (25 MB), page count (500), per-page decompression (2 MB) and extracted text (120,000 UTF-8 bytes) are bounded. A real minimal-PDF extraction fixture test has been added. OCR, stronger parser isolation/hardening, latest Rust/TypeScript/Windows build results and end-to-end Windows verification remain pending.
- [~] **Web Research** — multi-source implementation is present: user-initiated DuckDuckGo HTML search; up to four explicitly selected HTTPS sources; bounded downloads and extraction; public-IP/DNS validation with pinned connections; manual redirect revalidation; HTML5 text extraction; BOM/header/meta charset decoding; publisher-declared author/date metadata; local Ollama synthesis with source labels; and visible citation-limit warning. CI Run #137 passed frontend build, Rust unit tests, Windows installer build and artifact upload. Remaining: live provider/page compatibility checks, deeper source-claim review, Windows end-to-end acceptance, and ongoing security review. Citation-label validation does not prove that a cited page supports a claim.
- [~] **File Assistant** — Stages 1–5 implemented on the development branch. CI verification: Stage 2 Run #158, Stage 3 Run #176, Stage 4 Run #183, and Stage 5 Run #191 passed frontend build, Rust unit tests, Windows installer build and artifact upload. Folder scanning is bounded and session-scoped; text reading is limited; edits and moves require previews and explicit one-use approval; originals are backed up; undo history supports approved edits and moves and protects newer changes. Windows desktop acceptance testing remains pending. Stage 6 implementation and CI verification are complete (Run #250); hands-on Windows acceptance and power-loss fault-injection checks remain for the final release phase.
### Coding Assistant — staged implementation

- [x] **Stage 1 — Read-only project understanding (implemented and CI-verified):** select a project folder; scan a bounded set of source/configuration files; exclude hidden/dependency/build folders, credentials, private keys, environment files and binaries; explicitly choose up to 25 files; read at most 128 KB per file and 180 KB total; send selected text only to local Ollama on user request; explain structure and review code without writing or executing anything. Session-scoped grants are revoked when the skill is unloaded, disabled or uninstalled. Run #262 (`a1e450fe5e04e6ff65674b7b020a09967c89de1f`) passed frontend build, Rust unit tests, Windows installer build and artifact upload. Hands-on Windows acceptance remains pending.
- [x] **Stage 2 — Deeper project intelligence (implemented and CI-verified):** deterministic read-only analysis maps selected files, recognises common entry points, extracts common manifest dependencies, lists language-aware symbol declarations, detects imports and heuristic links to selected project files, and flags review hints (TODO/FIXME, Rust unwrap/expect, JS/TS eval) with file/line evidence. Results are shown separately from local-model explanations and include explicit heuristic limitations. Rust fixture tests cover project maps, dependencies, imports/symbols, diagnostics and input bounds. Run #270 (`31b24c8eb573037b47f763a8ebfce59041d2a701`) passed frontend build, Rust unit tests, Windows installer build and artifact upload. Hands-on Windows acceptance and broader parser accuracy remain pending.
- [x] **Stage 3 — Safe code editing (implemented and CI-verified; hands-on Windows acceptance pending):** one-file proposal preview, bounded UTF-8 content, SHA-256 fingerprints, readable diff, 120-second preview expiry, bounded pending previews, project/path/type/size/reparse-point revalidation, one-use explicit approval, verified original backups, transaction journals, atomic replacement, post-write hash verification, guarded Undo that refuses intervening user edits, and recovery reconciliation for interrupted Apply/Undo. Windows DACLs are preserved during normal replacement and captured/restored when recovery must rebuild a missing file; Unix permission bits are restored on recovery. Recovery rejects malformed, missing, mismatched, redirected, oversized or hash-invalid evidence and preserves files/journals for manual review rather than guessing. The UI now exposes explicit Apply confirmation, persistent per-project change history, status/reason reporting, and Undo only when the backend confirms it is safe. Regression coverage includes malformed metadata, missing/corrupt backups, missing targets, unknown target contents, conflicting active/consumed records, symlinked history records, documented ReplaceFileW failure-state snapshots, and five simulated interruption boundaries each for Apply and Undo. Runs #326 (backend safety), #328–#330 (UI and stale-preview fixes), and #333–#335 (persistent history and eight history-safety tests) passed their Windows/Linux CI jobs as applicable. ReplaceFileW failure cases are simulated filesystem snapshots, not actual API fault injection; CI is not physical power-loss or hands-on Windows acceptance. SACL auditing data is not preserved. Final Windows acceptance and real-world interruption testing remain for the release phase.
- [~] **Stage 4 — Sandboxed testing and execution (foundation in progress; execution deliberately disabled):** backend preflight is tied to the explicitly selected, revalidated project root and detects Windows Sandbox without launching it. Added a bounded private project snapshot under FRIDAY app data: allowlisted source/configuration files only, no hidden/dependency/build folders, symlinks/reparse points, likely secret filenames or binary assets; limits of 2,000 files, 1 MB per file, 32 MB total and 12 directory levels; unique snapshot IDs; UI copy-count/size/truncation reporting; and session-owned cleanup that refuses unknown IDs or changed paths. Snapshot creation does not modify the original project and does not execute code. Runs #345–#348 passed Windows/Linux CI. Regression coverage now exercises actual snapshot copying, exclusions, symlink skipping on Unix, oversized files, file-count limits, relative-path validation, and preflight's disabled execution/network flags. Cleanup retains ownership state when validation/removal fails. Added `docs/CODING-ASSISTANT-SANDBOX-DESIGN.md` documenting Microsoft Windows Sandbox constraints and the execution gate. In particular, Windows Sandbox networking defaults to enabled unless explicitly disabled; mapped host folders are a security boundary; `wsb exec` currently does not return process I/O; documented .wsb settings expose memory but not CPU quota or writable-output disk quota; and Windows Sandbox is unavailable on Windows Home. Added a preview-only .wsb configuration builder tied to a current-session-owned snapshot; it validates the canonical snapshot directory, XML-escapes the host path, disables networking/vGPU/clipboard/audio/video/printer redirection, enables ProtectedClient, and maps only the snapshot read-only. The UI displays the generated XML; no .wsb file is written and no Sandbox is launched. New regression tests cover the config settings, read-only mapping, XML escaping and invalid paths (Runs #353–#359: the path-format failure in #353 was corrected in the following commit and verified by passing Linux and Windows Rust test jobs in #354). Windows preflight now reports edition/build, optional-feature state, wsb CLI/version, firmware virtualization reporting, and an existing Sandbox process; the UI displays these diagnostics. These are read-only probes, not a launch test or complete policy audit. Implemented persisted per-snapshot ownership markers, bounded marker parsing, canonical-parent and normal-directory checks, startup recovery of verified FRIDAY-owned partial/stale snapshots, and conservative preservation of missing/malformed/mismatched/redirected entries. Recovery scans only the dedicated sandbox workspace, never the selected project. Regression tests cover valid ownership, malformed/missing/mismatched markers and symlinked snapshot directories. The marker is accidental-ownership evidence, not cryptographic authentication against malicious code running as the same Windows user; future execution must also avoid deleting snapshots mounted by an active FRIDAY-owned Sandbox. Windows and Linux Rust tests plus the Windows release/installer build passed in Run #377 (132 Windows Rust tests and 136 Linux Rust tests; installer artifact uploaded). Runs #371–#376 remain historical failures caused by the Windows canonical-path normalization assertion; the corrected implementation is verified by #377. CI verification update: Runs #425–#440 passed both `build-windows` and `test-linux`. Runs #433–#435 specifically verify the Windows `FILE_FLAG_OPEN_REPARSE_POINT` symlink regression test. Unix `O_NOFOLLOW` and Windows final-component reparse-point handling are CI-verified; parent-directory race resistance remains unresolved and is explicitly tracked in the guest-runner design. A trusted guest-runner contract is now documented in `docs/CODING-ASSISTANT-GUEST-RUNNER-DESIGN.md`: versioned request/result schemas, a deliberately narrow operation allowlist, direct process creation (no shell strings), strict output validation, per-run storage, and fail-closed behavior. The listed operations are design candidates only, not currently executable. Microsoft Job Object research identifies candidate guest-side CPU hard-cap, job memory, process-count and user-mode time controls, but these need a suspended-create/assign-before-resume implementation and Windows verification. Disk capacity still needs a separate enforceable bounded-volume design; I/O limits are not a disk quota. A fixed-capacity per-run VHDX mounted at a dedicated host directory is documented as a prototype candidate for the writable result share. A read-only host capability probe now reports `virtdisk.dll`, whether the current token has `SeManageVolumePrivilege` enabled, optional Hyper-V cmdlet availability, and Windows Sandbox `AllowMappedFolders` / `AllowWriteToMappedFolders` policy states; the UI shows these separately. Policy values are reported as Allowed, Blocked, Not configured, or Unknown without changing registry or Group Policy. Microsoft documents that `AttachVirtualDisk` fails without `SE_MANAGE_VOLUME_PRIVILEGE`. FRIDAY does not silently install optional features, alter token privileges, request elevation, or fall back to a normal writable host folder. Native Virtual Disk API research is documented: fixed VHDX creation uses `CreateVirtualDisk` with `CREATE_VIRTUAL_DISK_FLAG_FULL_PHYSICAL_ALLOCATION`; `CREATE_VIRTUAL_DISK_VERSION_2` requires `VIRTUAL_DISK_ACCESS_NONE`; attach requires `SE_MANAGE_VOLUME_PRIVILEGE`; detach requires the correct handle access and all other handles closed. No disk creation, attachment, partitioning, formatting, mount, or guest sharing has been implemented. Disk initialization must correlate the attached device to the exact run/VHDX identity, not a transient disk number or unreviewed `diskpart` script. Microsoft documents that initializing a disk requires local Administrators or Backup Operators membership. A narrowly scoped, explicitly UAC-approved helper versus a non-elevated result-transfer design remains unresolved; no elevated helper is approved or implemented, and FRIDAY must never silently elevate or run arbitrary elevated PowerShell. Even when the API and privilege are detected, result storage remains unverified until mapped-folder access through the VHDX mount point, guest write identity/ACLs, disk-full behavior, crash recovery and dismount/cleanup pass physical Windows testing. Remaining: verify probe/recovery behavior on supported physical Windows editions, improve process/session ownership detection, adversarial Windows reparse-point tests, a Windows Sandbox-only launcher, reviewed fixed guest toolchain profiles, Job Object enforcement before untrusted code starts, enforceable output-disk quotas, session watchdog/cancellation, validated result collection, and the full adversarial regression suite. No host-execution fallback is permitted. Do not enable execution until all isolation controls and failure paths are implemented and tested.
- [ ] **Stage 5 — Reliability and security review:** malicious repository prompt-injection tests, path/reparse-point protections, interrupted-operation recovery, language fixtures, regression tests and final Windows CI/acceptance testing.

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



**Privilege policy:** initial guest-runner work will not silently elevate or add an administrator helper. Prioritize a non-elevated result-transfer design with a hard pre-enforced storage cap. A normal writable mapped folder with post-write checks is insufficient. If no non-elevated design can meet the storage, isolation, ownership, recovery and cleanup requirements, keep guest execution disabled. See `docs/CODING-ASSISTANT-GUEST-RUNNER-DESIGN.md`.

## Current next work item

**Phase 2 — Coding Assistant Stage 4:** continue completing the Windows Sandbox execution boundary while keeping execution disabled. Bounded read-only host diagnostics and conservative snapshot recovery are implemented and verified by Run #377. Runs #408–#410 and #411–#414 passed both Windows and Linux CI jobs. The request contract is aligned with the Rust fields (`maxDurationMs`, `maxOutputBytes`), combined UTF-8 diagnostics are capped per run, and a disconnected result-file reader now enforces one exact `result.json` entry, regular-file/path checks, and bounded reads. The parser and file reader remain disconnected from execution. The trusted guest-runner request/result contract and fail-closed operation policy are documented in `docs/CODING-ASSISTANT-GUEST-RUNNER-DESIGN.md`; no runner or launcher exists yet. Next resolve the hard-bounded, non-admin guest result-transfer architecture before implementing any guest runner. Microsoft CLI research confirmed that `wsb exec` cannot return stdout/stderr, and clipboard redirection is a broad bidirectional channel without a documented per-run hard cap; both are rejected as result-transfer mechanisms. Hyper-V sockets require administrator registration for a custom service. The Microsoft MXC TCP bridge is useful protocol research but is not adopted: it enables guest networking, uses a writable rendezvous mapping, relies on guest-side firewall lockdown, starts child code before best-effort Job Object assignment, and invokes `cmd.exe /C`. See the guest-runner design document. See `docs/CODING-ASSISTANT-GUEST-RUNNER-DESIGN.md`. Writable mapped folders, post-write size checks, NTFS per-user quotas, `wsb exec` (which returns only an exit code and has no process-I/O retrieval), and clipboard redirection are not accepted as result-transfer boundaries; VHDX remains blocked until its complete lifecycle is proven without silent elevation. Only after a viable storage boundary exists should fixed runtime profiles and resource controls be implemented and independently reviewed. Do not enable execution until every safety gate and supported-Windows acceptance test passes. Excel Intelligence remains explicitly on the roadmap.

## Core platform implementation notes

**Phase 1C — Persistent chat history:** encrypted local conversation storage, conversation list/create/switch/rename/delete, title search and persistence of completed or stopped responses are implemented in the development branch. Rust/TypeScript checks and Windows end-to-end verification remain pending.

**Phase 1F — Central Tool Execution and Permission System:** the allowlisted registry exposes safe local arithmetic and fixed desktop destinations. Streamed Ollama requests include those tool schemas; model-proposed calls are shown for explicit user approval, with a maximum of three approved tool rounds and an 8 KB proposal limit. Desktop execution requires a short-lived, one-use backend-issued token bound to the exact tool and arguments; mismatches, expiry and replay are rejected. The token supplements, but does not replace, the visible user confirmation and is not a security boundary against a compromised frontend. Tool activity is persisted locally in an encrypted, bounded audit file and can be cleared in the UI. The handoff still uses bounded app-generated assistant context rather than a native Ollama tool-role transcript. File read/write tools and sandboxed code execution are intentionally deferred to the File Assistant and Coding Assistant skills, where path grants, diffs, and resource limits can be designed with the actual workflows. Rust/TypeScript checks and Windows verification remain pending.

**Phase 1E — Proper Skills Platform:** four versioned built-in declarative packages can be installed locally, enabled, explicitly loaded/unloaded and integrity-checked. Loaded instructions are added to the next local chat prompt; packages do not execute scripts and do not yet provide their advertised tools. Automated and Windows verification remain pending.

**Phase 1D — Persistent personal memory:** user-entered memory entries are stored separately from transcripts in an encrypted local file. The chat UI supports add, edit, enable/disable and delete; enabled entries are included as bounded context in local Ollama prompts. No memory is silently extracted or saved. Rust/TypeScript checks and Windows end-to-end verification remain pending.


- [ ] Coding Assistant Stage 3: startup/session recovery for interrupted Apply and Undo transactions (implemented on `feature/phase-2-skills`; CI verification pending).


- [x] Coding Assistant Stage 4: read-only Windows Sandbox capability diagnostics include a bounded 15-second probe, bounded output draining, and conservative unknown-on-failure handling (implementation committed; CI verification pending).


- [x] Coding Assistant Stage 4: per-run guest stdout/stderr UTF-8 output budget is enforced by the host result validator; Runs #413 and #414 passed both Windows and Linux CI jobs.
- [x] Coding Assistant Stage 4: disconnected guest result-file boundary validator accepts only one exact `result.json` regular file, rejects unexpected entries and link/reparse-point paths, and bounds the read; regression tests added. Runs #421–#423 passed both Windows and Linux CI jobs.


- [x] Coding Assistant Stage 4: investigated NTFS per-user quotas as a non-admin result-storage option; rejected for the initial design because quotas are volume/user scoped rather than per-run/per-folder and mapped-folder host ownership cannot be assumed. No host quota settings changed. See `docs/CODING-ASSISTANT-GUEST-RUNNER-DESIGN.md`.


- [x] Coding Assistant Stage 4: Unix regression tests covering symlinked result directories and non-text filenames passed in Runs #421–#423 (both Windows and Linux CI jobs).


- [x] Coding Assistant Stage 4 security hardening: Unix `open_source_file` uses `O_NOFOLLOW`, and Windows `open_source_file` uses `FILE_FLAG_OPEN_REPARSE_POINT`; final-component symlink/reparse-point regression tests pass in CI (Runs #430–#435). Parent-directory race resistance and physical Windows junction/reparse-point acceptance remain separate review items.


- [x] Runs #425–#431 passed both `build-windows` and `test-linux` jobs. This verifies compilation/tests for the current changes, not physical Windows Sandbox execution or full filesystem race resistance.
- [~] Stage 4 filesystem prototype: separate test-only cap-primitives 4.0.3 handle-relative directory walker in `src-tauri/src/coding_sandbox_fs.rs` (compiled only under `cfg(test)`; dependencies are dev-only). It enumerates relative to a held root handle, validates single components and uses `open_dir_nofollow` for child directories. A Windows `NtCreateFile` helper opens regular files relative to a held parent with no-reparse flags and synchronous I/O; it remains test-only and is not wired into snapshot copying. Runs #452, #454 and #458 passed Windows and Linux CI. Run #453 exposed a missing synchronous-I/O option (Windows error 87), corrected and verified in #454. Runs #455–#457 exposed a quoting/path-syntax bug in the `mklink /J` test setup; the test now passes in #458. A test-only root acquisition prototype now compares directory identity before/opened/after (Unix device+inode; Windows volume serial+file index), requires an absolute path, and fails closed on reparse points or identity mismatch. Production snapshot traversal remains path-based; deterministic root replacement tests, ancestor-path/reparse review, broader reparse-tag coverage, and physical Windows acceptance remain open. `cap-primitives` is pinned to patched 4.0.3 because earlier releases are affected by RUSTSEC-2026-0482.
- [ ] Stage 4 next: prove a non-elevated, hard-bounded, host-isolated guest result-transfer channel. No accepted mechanism has been identified yet; keep guest execution disabled.
- [ ] Stage 4 next: implement exact FRIDAY-owned Sandbox lifecycle, fixed runtime profiles, pre-resume Job Object enforcement, hard storage/output limits, watchdog/cancellation, and validated result collection only after the transfer boundary is proven.
- [ ] Stage 5: malicious repository/prompt-injection tests, filesystem and recovery adversarial tests, language fixtures, full regression suite, and physical Windows acceptance.


- [~] Windows reparse-point policy review documented in `docs/CODING-ASSISTANT-GUEST-RUNNER-DESIGN.md`: reject all entries carrying FILE_ATTRIBUTE_REPARSE_POINT rather than allowlisting tags, covering symlinks, junctions/mount points, cloud placeholders, WCI, AppExecLink, ProjFS, DFS and unknown third-party tags. Existing junction regression is verified by #458; deterministic ancestor replacement regression passed in #467. A real volume-mount-point test and physical Windows acceptance remain open. This review does not change production snapshot traversal or enable guest execution.

- [~] Coding Assistant Stage 4 root acquisition prototype now walks from the filesystem root/local Windows drive root using held parent directory handles and no-follow child opens; it verifies root identity before/opened/after. Canonical local-drive and explicit UNC rejection tests were added. Runs #467–#472 passed both Windows and Linux CI. A new Unix regression replaces an ancestor after its handle is held and verifies final root acquisition fails closed; this latest regression is awaiting CI. Windows UNC/device namespaces remain unsupported, and physical volume-mount-point acceptance remains open. Prototype is not connected to production snapshot creation.

- [~] Stage 4 filesystem prototype: Unix regular-file opening now uses `openat` relative to a held parent handle with `O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK`, then verifies the opened handle is a regular file. Tests cover regular reads, symlink/traversal rejection, and parent-path replacement after the handle is held. Windows uses the existing `NtCreateFile` no-reparse helper. The new Unix file-open tests and held-ancestor root regression are awaiting CI in #473–#480. Neither helper is integrated into production snapshot copying.


- [~] **Stage 4 — bounded handle-relative source capture prototype:** `coding_sandbox_fs.rs` now combines verified root acquisition, handle-relative directory traversal and no-follow file opening into a bounded in-memory capture. It enforces depth/file-count/per-file/total-byte limits and adds nested-copy, limit, symlink-swap and held-parent replacement tests. Runs #475–#478 and #480–#482 passed both Windows and Linux CI; #473–#474 and #479 were corrected by later commits. This is still test-only: it does not write destination files, is not wired into production snapshot creation, and the post-read metadata check is not an atomic snapshot. Next: review safe destination-side creation/cleanup, then integrate only after adversarial tests and independent review. Production snapshot copier remains path-based; guest execution remains disabled. See `docs/CODING-ASSISTANT-GUEST-RUNNER-DESIGN.md`.


- [~] **Stage 4 — handle-relative snapshot destination creation prototype:** a separate test-only writer now creates an exclusive snapshot directory and nested files below an already-verified workspace handle. Unix uses `mkdirat`/`openat(O_EXCL | O_NOFOLLOW)`; Windows uses handle-relative `NtCreateFile(FILE_CREATE, OBJ_DONT_REPARSE)`. It revalidates path and byte limits, reserves the ownership-marker name, flushes each file, and writes the ownership marker last so partial snapshots are not falsely marked complete. Tests cover nested writes, pre-existing destination preservation, traversal rejection, and unmarked partial failure. Awaiting CI for the new commits. Cleanup is intentionally not implemented; partial unmarked directories are preserved. The helper is test-only, production copying remains path-based, and guest execution remains disabled. See `docs/CODING-ASSISTANT-GUEST-RUNNER-DESIGN.md`.
- [~] **Stage 4 — snapshot cleanup/recovery security review:** Runs #488–#495 passed both Windows and Linux CI, verifying the test-only destination writer prototype. Reviewed the existing production recovery path and identified that marker validation followed by `remove_dir_all` is not a race-resistant deletion boundary; schema-1 markers also lack a payload inventory. Documented a fail-closed cleanup design: versioned inventory, exact handle-relative tree preflight, unknown-entry preservation, handle-based deletion, and interruption-safe retry. No cleanup implementation or production integration yet. Next: implement and test manifest validation and handle-relative cleanup in the test-only filesystem prototype; preserve legacy/unmarked snapshots. See `docs/CODING-ASSISTANT-GUEST-RUNNER-DESIGN.md`.
