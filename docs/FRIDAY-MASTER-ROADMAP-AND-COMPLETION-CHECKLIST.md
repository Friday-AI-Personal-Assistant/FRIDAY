# FRIDAY Master Roadmap & Completion Checklist

> **Purpose:** This is the durable, cross-conversation source of truth for what FRIDAY is intended to become, what has been verified as implemented, what remains, the order of work, and the relative effort/risk of each area. A new development conversation must read this file and the Coding Assistant completion checklist before making changes.
>
> **Status baseline:** This document is a planning and status consolidation based on the repository documentation and the Coding Assistant audit baseline `feature/phase-2-skills` at #988 (`108a5573bb3343017658c826f3d682aeb845b3c0`). The Coding Assistant checklist contains the detailed #988 evidence. Other product areas are marked planned/not verified unless repository evidence explicitly says otherwise.
>
> **Do not confuse a plan with implementation.** A checkbox is complete only when code, tests, and (where required) real-device acceptance demonstrate it. Update this document after each meaningful batch with commit, CI run, and evidence.

## 0. How to use this document

- [ ] At the start of every new development conversation, read this document and `docs/CODING-ASSISTANT-COMPLETION-MASTER.md`.
- [ ] Verify the current branch, HEAD commit, working tree, and latest Linux/Windows CI before editing.
- [ ] Find the first unchecked dependency-ready task in the ordered sections below.
- [ ] Work in coherent batches; avoid splitting tightly coupled changes into many tiny runs.
- [ ] Add regression tests with every implementation batch.
- [ ] Run relevant tests plus Linux and Windows CI; record exact run IDs and results.
- [ ] Update status, evidence, blockers, and next task here after each batch.
- [ ] Never mark a feature complete because a screen, mock, design, stub, or plan exists.
- [ ] Never claim physical Windows, microphone, OAuth-provider, installer, or hardware behavior has passed unless it was actually tested in that environment.

### Status vocabulary

| Status | Meaning |
|---|---|
| `DONE — VERIFIED` | Implemented and supported by relevant tests/evidence. |
| `DONE — LIMITED` | Implemented, but intentionally limited; the limitation is stated. |
| `IN PROGRESS` | Work has begun but acceptance criteria are not all met. |
| `PLANNED` | Product requirement accepted; implementation not verified. |
| `BLOCKED` | Cannot safely proceed until a named dependency or decision is resolved. |
| `PHYSICAL / PROVIDER TEST REQUIRED` | Code/CI is insufficient; real OS, device, provider or account acceptance is required. |
| `NOT IN INITIAL RELEASE` | Deliberately deferred, not forgotten. |

## 1. Product vision and non-negotiable principles

FRIDAY is a modular, local-first personal AI desktop assistant. A user should be able to install the core application, add only the capabilities they need, choose their AI engine/model/voice, control permissions, and understand exactly what runs locally or communicates externally.

### Product principles

- [ ] Local-first: local inference is the default where hardware supports it.
- [ ] User owns their files, workspace, settings, model choices, and data.
- [ ] Installing a skill never grants it access to personal files.
- [ ] Asking the AI to do something never grants permission by itself.
- [ ] Every file/folder permission is explicit, narrow, visible, revocable, and enforced in Rust/backend code.
- [ ] No cloud inference or external data transfer without an explicit user choice and clear disclosure.
- [ ] No silent cloud fallback when a local model is unavailable.
- [ ] Offline mode must be technically enforced, not just represented by a UI toggle.
- [ ] No hidden developer access, backdoor, remote file browsing, or central storage of private workspace data.
- [ ] Sensitive writes, sends, submissions, deletes, and external actions require appropriate user approval.
- [ ] Actions should be auditable, recoverable, and reversible where technically possible.
- [ ] Skills and optional dependencies should load on demand and release resources when unused.
- [ ] Security claims must state limitations; never promise absolute security against a compromised OS or malware running as the same user.
- [ ] Initial distribution should be usable without paid cloud services; optional paid services must be clearly labelled.
- [ ] Commercial distribution must respect code, dependency, voice, model, and provider licences.

## 2. Overall status snapshot

| Product area | Current status | Notes |
|---|---|---|
| Coding Assistant | IN PROGRESS | Substantial read/analysis/approved-edit foundations exist. Guest execution remains deliberately disabled; see the dedicated completion checklist. |
| File Assistant | DONE — LIMITED / VERIFY CURRENT TESTS | Backend file operations, previews, approved edits, moves/renames, backups, undo and interrupted transaction recovery were previously audited as implemented. Re-verify current HEAD before release. |
| Skills framework | IN PROGRESS | Skill install/enable/load/unload and package integrity are implemented; tool connections and complete catalogue experience remain. |
| Web Research skill | PLANNED / PARTIAL | Skill instructions exist; integrated research tools, citations and report workflow are not verified complete. |
| PDF Reader skill | PLANNED / PARTIAL | Skill instructions exist; dedicated extraction, page-aware citations and complete UI workflow are not verified complete. |
| Excel Intelligence skill | PLANNED | Explicit product goal; not counted as implemented. |
| Core assistant/chat experience | PLANNED / AUDIT REQUIRED | Audit current implementation before adding or duplicating components. |
| Engine/provider manager | PLANNED | Unified local/cloud engine abstraction and provider lifecycle required. |
| Model catalogue/manager | PLANNED | Install, verify, choose, display requirements/licence/cost and unload models. |
| Voice selection and speech | PLANNED | Local and optional cloud voice choices, with microphone/speech permissions. |
| Privacy and permission centre | PLANNED | Backend-enforced grants, offline/network policy, audit and transparency. |
| Settings and feature controls | PLANNED | Complete settings UX, explanations, defaults, reset/export and validation. |
| Email Assistant | PLANNED | OAuth, minimum scopes, read/draft first; sending only after approval. |
| Job Hunter | PLANNED | Permitted job sources and application preparation; user controls submissions. |
| CV Studio | PLANNED | Import/edit/preview/export; never fabricate user facts. |
| Distribution and updates | PLANNED / AUDIT REQUIRED | Repeatable installers, signing, secure update path, release matrix and documentation. |
| Accounts/licensing | NOT IN INITIAL RELEASE | Separate privacy/security design required before any implementation. |

## 3. Core application foundation

**Difficulty:** Medium to hard. **Time:** foundational work; do early because every module depends on it.

- [ ] Audit current app shell, routing, state management, command registration and module boundaries.
- [ ] Define stable module/skill/provider interfaces so future features do not create tight coupling.
- [ ] Define a single Rust backend authority for filesystem access, secrets, permissions, approvals and external actions.
- [ ] Define typed, versioned frontend/backend contracts and consistent error handling.
- [ ] Establish application lifecycle: startup, lazy activation, background work, shutdown and crash recovery.
- [ ] Establish structured, privacy-safe diagnostics that never log document contents, secrets, prompts or tokens by default.
- [ ] Establish a local database and schema migration strategy (if not already adequate).
- [ ] Establish local data directory layout, file ownership, restrictive permissions and backup/restore.
- [ ] Add user-controlled export and deletion of FRIDAY-owned data.
- [ ] Ensure uninstall behavior explains what local data/models are retained or removed.
- [ ] Define resource budgets for core app, skills, engines and models.
- [ ] Add accessibility, keyboard navigation, responsive layouts and usable empty/error/loading states.
- [ ] Add automated tests for module activation, permission boundaries, data migrations and lifecycle.

## 4. Skills platform and Skills Store

**Difficulty:** Medium initially; hard when executable third-party skills and secure updates are introduced. **Time:** high leverage—build the platform once, then reuse it for many capabilities.

### 4.1 Skill package contract and lifecycle

- [ ] Define a versioned skill manifest: ID, name, description, version, author, licence, dependencies, supported OS, entry points, capabilities, resource estimate and network requirements.
- [ ] Distinguish declarative/instruction-only skills from executable skills and external connectors.
- [ ] Define install, verify, enable, activate, unload, disable, update, rollback and uninstall states.
- [ ] Verify package signatures/provenance or trusted publisher metadata; verify hashes and reject tampering.
- [ ] Make installation separate from enabling and enabling separate from granting permissions.
- [ ] Lazy-load instructions, tools and optional dependencies only when a skill is activated.
- [ ] Unload skill-specific resources when idle; close child processes and release handles.
- [ ] Avoid keeping large model/runtime dependencies resident for an inactive skill.
- [ ] Revoke grants, tokens, pending approvals and active sessions when a skill is disabled/uninstalled.
- [ ] Make updates atomic with rollback on failure.
- [ ] Enforce per-skill CPU, memory, storage, process and network policy where applicable.
- [ ] Prevent one skill from directly reading another skill's private state without a grant.
- [ ] Add malicious-package, manifest-tampering, dependency-confusion and rollback tests.

### 4.2 Skills Store UX

- [ ] Build a catalogue with search, categories, descriptions, screenshots where useful, version, publisher and licence.
- [ ] Show whether each skill is installed, enabled, currently loaded, trusted, and what permissions it requests.
- [ ] Show estimated disk use, dependencies, memory/runtime behavior and network requirements.
- [ ] Show free/paid status and any recurring or usage-based cost before activation.
- [ ] Let users install, enable, disable, update, rollback and uninstall independently.
- [ ] Include an explicit permission review before first use.
- [ ] Provide a local/manual installation path with integrity verification.
- [ ] Clearly distinguish official skills from community/third-party skills.
- [ ] Do not auto-install a skill or grant it access just because a task mentions it.

### 4.3 Initial skills catalogue

- [ ] File Assistant — verify existing operations, approval UX, recovery and permission enforcement.
- [ ] Coding Assistant — complete all gates in `CODING-ASSISTANT-COMPLETION-MASTER.md`.
- [ ] Web Research — source discovery, source quality, citations, summaries and report export.
- [ ] PDF Reader — safe PDF parsing, OCR decision, page-aware extraction, summaries and Q&A.
- [ ] Excel Intelligence — workbook/sheet discovery, formulas, tables, charts, analysis, previews and approved edits.
- [ ] Add future skills through the same manifest/permission/lifecycle contract, not custom one-off architecture.

## 5. Engines and provider architecture

**Difficulty:** Hard. **Time:** Medium-to-high; provider differences, streaming, cancellation and model compatibility create integration work. Build a common interface before adding many providers.

- [ ] Define a common inference-engine interface: discover, validate, load/select, generate, stream, cancel, unload, health-check and report resource use.
- [ ] Implement local engine adapters (initial candidate: Ollama; evaluate llama.cpp and other runtimes against licensing, packaging and hardware needs).
- [ ] Keep provider-specific behavior behind adapters rather than inside every skill.
- [ ] Bind local services to loopback only where network transport is needed.
- [ ] Authenticate local engine endpoints where supported and restrict who can invoke them.
- [ ] Verify the actual active endpoint, engine and model; never rely only on a UI selection.
- [ ] Prevent automatic fallback from local to cloud.
- [ ] Add optional cloud provider adapters only behind explicit user opt-in and per-provider disclosure.
- [ ] Make provider selection per task or global setting explicit and visible.
- [ ] Support cancellation, timeout, retry policy and bounded request/response sizes.
- [ ] Handle engine unavailable, model missing, out-of-memory, network failure and malformed response states.
- [ ] Provide a strict offline policy that prevents external inference and external connectors.
- [ ] Test local engine behavior with real installed engines and representative hardware.
- [ ] Document that localhost avoids remote inference but is not a complete boundary against other local processes or same-user malware.

## 6. Models catalogue and model manager

**Difficulty:** Medium to hard. **Time:** Medium; model metadata and download/verification are straightforward compared with compatibility and memory management.

- [ ] Build a model catalogue with model name, publisher, source, version/revision, format, size, context window, licence and supported engine.
- [ ] Label local vs cloud models clearly.
- [ ] Label free/open-weight vs paid/API-billed models accurately; do not call a model free if provider usage costs apply.
- [ ] Display estimated disk space, RAM/VRAM requirements and performance caveats before download.
- [ ] Show model licence and usage restrictions, including commercial-use limits.
- [ ] Download only from configured/trusted sources with checksum/signature verification where available.
- [ ] Support pause, resume, cancellation, disk-space checks and cleanup of incomplete downloads.
- [ ] Allow the user to choose a default model and per-task model.
- [ ] Load models on demand and unload them when no longer needed.
- [ ] Avoid silently loading multiple large models simultaneously.
- [ ] Display actual loaded model, memory use when measurable, and engine health.
- [ ] Provide a safe response when model memory requirements exceed available resources.
- [ ] Support a small/fast default for low-resource devices and allow user override.
- [ ] Keep model download/update network access separate from inference offline mode; ask before downloading.
- [ ] Test model switching, cancellation, corrupted downloads, insufficient disk/RAM and engine restart.

## 7. Hardware-aware resource management

**Difficulty:** Hard to get robust across hardware. **Time:** Medium-to-high; profiling and real-device validation are needed.

- [ ] Detect OS, CPU architecture, logical cores, RAM, available disk and GPU/VRAM where reliably supported.
- [ ] Use conservative estimates; clearly distinguish detected facts from estimates.
- [ ] Define supported device tiers (including 4 GB RAM minimum target, 8 GB recommended, 16 GB ideal target) without promising identical performance.
- [ ] Define resource budgets for core app, each skill, engine and model.
- [ ] Prevent simultaneous heavy skills/models from exceeding configured budgets where enforceable.
- [ ] Add queueing or graceful refusal instead of starting work that cannot fit.
- [ ] Allow users to choose resource profiles (quiet/balanced/performance) with explanations.
- [ ] Provide unload/stop controls for active engines and skills.
- [ ] Test on real 4 GB, 8 GB and 16 GB devices where possible; record actual latency, memory and limitations.
- [ ] Never claim a model fits solely because the computer meets total-RAM tier; OS and other apps also consume memory.

## 8. Voice Assistant and voice catalogue

**Difficulty:** Medium for local speech; hard for quality, multilingual support, packaging and commercial voice licensing. **Time:** Medium-to-high.

- [ ] Define voice input and text-to-speech provider interfaces.
- [ ] Catalogue local voices with language, locale, quality, disk size, licence and resource requirements.
- [ ] Provide a default free/local voice where a suitable supported voice exists.
- [ ] Label cloud voices and any per-character/minute cost clearly.
- [ ] Make microphone access optional, permission-based and visibly active only while listening.
- [ ] Provide push-to-talk first; make always-listening an explicit separate opt-in if ever supported.
- [ ] Provide voice selection, preview, speed, volume, language and pronunciation settings.
- [ ] Show when speech is processed locally vs sent to a provider.
- [ ] Never upload microphone audio without explicit consent.
- [ ] Add a clear recording/listening indicator and immediate stop control.
- [ ] Test real microphones, speakers, device switching, noise, cancellation and offline behavior.
- [ ] Verify every bundled/downloadable voice's redistribution and commercial licence.

## 9. Privacy modes, network controls and transparency

**Difficulty:** Hard / security-critical. **Time:** High; UI is easy, trustworthy enforcement and verification are not.

### 9.1 Three clear operating modes

- [ ] **Strict Offline Mode:** local inference only; block external AI, web research, cloud voice, email connectors, telemetry and background network activity attributable to FRIDAY workloads.
- [ ] **Local AI + Connected Features:** inference stays local; user may separately enable web, email, updates or other network features.
- [ ] **Cloud AI Mode:** user selects a provider; disclose exactly what categories of data may leave the device, provider, pricing and relevant retention/processing terms.
- [ ] Make mode transitions explicit and show which capabilities become unavailable.
- [ ] Never silently change modes or silently send a prompt to another provider.
- [ ] Separate model downloads and application updates from inference permissions; explain and request network access for each.

### 9.2 Enforced privacy and network policy

- [ ] Centralize network policy in backend services; every skill/engine/connector must consult it.
- [ ] Enforce strict offline mode at the OS/process boundary where possible, not merely through frontend checks.
- [ ] Block cloud fallback, telemetry, analytics, crash uploads and background sync in strict offline mode.
- [ ] Show unknown/unverifiable network state as unknown, never as safely offline.
- [ ] Bind local inference services to loopback and protect local control endpoints.
- [ ] Test attempted outbound connections from skills, subprocesses, engines and update components.
- [ ] Document residual risks from OS compromise, same-user malware and user-installed runtimes.

### 9.3 Transparency Centre

- [ ] Show active mode and whether external network access is allowed.
- [ ] Show the verified active engine, model and local/cloud processing status.
- [ ] Show active/loaded skills and their current grants.
- [ ] Show recent permission requests, approvals, file actions and external transfers.
- [ ] Show what data categories were sent externally, to which provider, and why, without logging sensitive content.
- [ ] Base indicators on backend-observed runtime events, not UI assumptions.
- [ ] Allow users to revoke grants, stop active work and export/clear appropriate audit history.
- [ ] Keep logs bounded and privacy-safe.

## 10. Permission Manager and personal-file protection

**Difficulty:** Hard / security-critical. **Time:** High; must be built into backend architecture, not bolted on at the end.

- [ ] Define grants by skill, resource, path/root, operation, purpose and expiry.
- [ ] Separate list, discover, read, create, edit, rename, move and delete permissions.
- [ ] Default-deny access to Desktop, Documents, Downloads, home folders, removable drives and network shares.
- [ ] Allow selecting one file or one folder rather than granting an entire drive.
- [ ] Make a selected file grant distinct from a directory-wide grant.
- [ ] Require explicit approval for sensitive writes, deletes, external sends and submissions.
- [ ] Bind one-time approvals to exact operation, file identity, content hash and expiry.
- [ ] Enforce every grant in Rust/backend code on every operation; frontend hiding is not security.
- [ ] Protect against symlinks, junctions, reparse points, path traversal and time-of-check/time-of-use races.
- [ ] Revalidate file identity and content before applying writes.
- [ ] Provide grant history, expiry, revoke-all and per-skill revoke.
- [ ] Revoke access and secrets when a skill is disabled or uninstalled.
- [ ] Ensure no skill inherits another skill's grants.
- [ ] Test malicious prompts, forged frontend requests, stale approvals, path swaps, privilege escalation and concurrent access.

## 11. Settings, features and user experience

**Difficulty:** Easy-to-medium for UI; medium-to-hard when settings must enforce policy consistently. **Time:** Medium. Build after underlying controls exist, so the UI represents real state.

### Settings sections

- [ ] **General:** startup, language, appearance, notifications, accessibility and update preferences.
- [ ] **Skills:** installed, enabled, loaded, update, uninstall, permissions and resource usage.
- [ ] **Engines:** local/cloud providers, health, endpoint, availability and network status.
- [ ] **Models:** catalogue, download, selection, licence, cost and hardware requirements.
- [ ] **Voice:** input/output voice, language, speed, microphone permission and processing location.
- [ ] **Features:** enable/disable optional product modules.
- [ ] **Privacy & Security:** modes, permissions, network policy, audit, approvals and data controls.
- [ ] **Resources:** CPU, RAM, GPU, disk budgets and active workloads.
- [ ] **Data & Storage:** workspace location, export, backup, restore and deletion.
- [ ] **Updates:** app, skill, engine and model update policy.
- [ ] **Documentation & About:** versions, licences, privacy explanation, security model and support details.

### Settings quality requirements

- [ ] Every setting explains what it does, why it matters, its default and what changes when toggled.
- [ ] Explain whether a setting sends data externally and whether it costs money.
- [ ] Explain RAM/disk impact and dependencies where relevant.
- [ ] Show whether a setting requires restart or unload/reload.
- [ ] Validate combinations; do not allow contradictory settings to create unsafe states.
- [ ] Provide safe defaults, reset-to-default and export/import settings.
- [ ] Show effective backend state, not only saved preference values.
- [ ] Ensure disabled features do not leave hidden background processes or permissions active.

## 12. Core assistant and task orchestration

**Difficulty:** Hard. **Time:** High; central to product quality and integration with every skill.

- [ ] Audit current chat/assistant implementation before duplicating it.
- [ ] Define task routing from user intent to skill/provider with explainable selection.
- [ ] Discover installed skills without loading all their heavy dependencies.
- [ ] Ask for missing permissions only when a task truly requires them.
- [ ] Separate planning/reasoning from privileged execution.
- [ ] Treat model output, files, websites, emails and skill content as untrusted input.
- [ ] Use typed tool contracts and backend validation for every action.
- [ ] Support cancellation, progress, bounded retries and clear failure messages.
- [ ] Require confirmation before high-impact or external actions.
- [ ] Keep task history useful without retaining sensitive content unnecessarily.
- [ ] Test prompt injection, conflicting skills, unavailable engines, malformed tool arguments and permission denial.

## 13. Initial product modules

### 13.1 File Assistant

- [ ] Re-audit existing backend and UI at current HEAD.
- [ ] Verify file discovery and selection are scoped to user-approved roots.
- [ ] Verify previews, backups, hashes, approved edits, rename/move and undo.
- [ ] Verify interrupted transactions recover safely.
- [ ] Add clear operation summaries and permission history.
- [ ] Add real-world Windows filesystem adversarial tests.
- [ ] Keep destructive actions approval-gated.

### 13.2 PDF Reader

- [ ] Safe PDF open/import with bounded file size, page count and parsing resources.
- [ ] Extract text with page numbers and preserve document order.
- [ ] Detect scanned/image-only pages and offer optional local OCR.
- [ ] Summarize whole document and selected pages.
- [ ] Answer questions with page-specific references to extracted content.
- [ ] Preview pages and extracted text before sending content to any model.
- [ ] Keep PDF processing local by default; clearly disclose cloud use if selected.
- [ ] Defend against malformed PDFs, parser exploits, huge files and decompression/resource exhaustion.
- [ ] Test digitally generated, scanned, mixed, encrypted and malformed PDFs.

### 13.3 Excel Intelligence

- [ ] Safe workbook import for supported formats (initially define XLSX/CSV; assess XLS separately).
- [ ] Enumerate sheets, tables, named ranges, formulas, dimensions and hidden content.
- [ ] Preview selected sheets/ranges before analysis.
- [ ] Explain formulas and detect common errors/inconsistencies.
- [ ] Perform summaries, filtering, comparisons, trends and statistical analysis.
- [ ] Generate charts and pivot-style summaries where supported.
- [ ] Preserve formulas, styles, merged cells, references and workbook integrity when editing.
- [ ] Propose edits as a preview/diff and require approval before writing.
- [ ] Keep original file backed up and provide verified undo/recovery.
- [ ] Never execute workbook macros or external links automatically.
- [ ] Defend against zip bombs, huge worksheets, malicious formulas, malformed XML and resource exhaustion.
- [ ] Test with representative real workbooks and round-trip fidelity checks.

### 13.4 Web Research

- [ ] Search only when connected mode and required network permissions are enabled.
- [ ] Show sources, dates, source quality and citations.
- [ ] Distinguish sourced facts from inference and uncertainty.
- [ ] Treat webpages as untrusted data, not instructions.
- [ ] Support concise answers and structured research reports.
- [ ] Export reports locally.
- [ ] Test stale sources, conflicting sources, prompt injection and unavailable networks.

### 13.5 CV Studio

- [ ] Import PDF/DOCX and extract content with user review.
- [ ] Store user-approved career facts as editable structured data.
- [ ] Create, edit and format CVs using templates.
- [ ] Offer ATS-friendly, modern and classic layouts.
- [ ] Preview and export PDF/DOCX.
- [ ] Never invent skills, qualifications, employers, dates or achievements.
- [ ] Clearly flag suggested wording that needs user verification.
- [ ] Test layout fidelity, multilingual text and malformed documents.

### 13.6 Email Assistant

- [ ] Use official OAuth flows; never ask users to paste passwords.
- [ ] Request minimum scopes and explain each scope.
- [ ] Start with read/search/summarize and draft-only workflows.
- [ ] Keep tokens in OS-protected secret storage.
- [ ] Show recipient, subject, body and attachments before sending.
- [ ] Require explicit per-send approval; no unattended sending by default.
- [ ] Support disconnect/revoke and token deletion.
- [ ] Clearly disclose provider/API/network processing.
- [ ] Test expired tokens, revoked access, phishing content and prompt injection in email bodies.

### 13.7 Job Hunter

- [ ] Use permitted job sources, official APIs/alerts and user-provided links.
- [ ] Avoid LinkedIn scraping, automated login and mass-application behavior.
- [ ] Compare job requirements with user-approved CV facts and preferences.
- [ ] Explain match reasoning and missing qualifications honestly.
- [ ] Prepare tailored CV/cover-letter/application drafts.
- [ ] Require user review and explicit approval for every submission.
- [ ] Track application status locally with export/delete controls.
- [ ] Disclose external services and data sent to job platforms.

### 13.8 Voice and Personality

- [ ] Support user-selected tone/personality presets and custom preferences.
- [ ] Support language and response-style preferences.
- [ ] Keep preferences local and editable/exportable.
- [ ] Ensure personality cannot override security policy or user permissions.
- [ ] Integrate voice only through the permissioned voice architecture in section 8.

## 14. Data, secrets and local storage

**Difficulty:** Hard / security-critical. **Time:** Medium-to-high.

- [ ] Define what data FRIDAY stores and why.
- [ ] Keep private workspace content on-device by default.
- [ ] Encrypt sensitive tokens/secrets using OS-protected storage.
- [ ] Do not store provider passwords in plaintext or application logs.
- [ ] Minimize retention of prompts, document content, extracted text and audit details.
- [ ] Give users clear retention, export, backup and delete controls.
- [ ] Protect local databases and files with appropriate OS permissions.
- [ ] Handle corruption, interrupted writes, migrations and restore.
- [ ] Avoid central server storage of user documents, emails, CVs or keys.
- [ ] Test backup/restore, deletion, permission isolation and secret leakage.

## 15. Distribution, updates and commercial readiness

**Difficulty:** Hard. **Time:** High near release; signing, update trust, licences and support are often underestimated.

- [ ] Define supported Windows versions and minimum hardware.
- [ ] Produce reproducible Windows installer builds.
- [ ] Sign installers and update artifacts where feasible; document certificate requirements.
- [ ] Secure update metadata and artifact integrity; prevent downgrade/tampering where practical.
- [ ] Separate core app updates from skill, engine and model updates.
- [ ] Support staged rollout, rollback and recovery from interrupted updates.
- [ ] Test clean install, upgrade, downgrade policy, repair, uninstall and reinstall.
- [ ] Document offline installation and update behavior.
- [ ] Generate a software bill of materials (SBOM).
- [ ] Audit third-party code, fonts, icons, voices, models and bundled assets for licence compatibility.
- [ ] Document open-source notices and attribution obligations.
- [ ] Define free vs paid features without making local basic use dependent on subscriptions.
- [ ] Keep any future account/licence service separate from private local workspace data.
- [ ] Do not implement accounts/payments until a separate privacy and threat-model review is approved.
- [ ] Prepare user documentation, troubleshooting, privacy policy, security disclosure and release notes.

## 16. Security and quality assurance

- [ ] Maintain a threat model covering malicious files, prompt injection, malicious skills, local malware, compromised providers, network threats and OS compromise.
- [ ] Define security invariants and negative tests for every privileged operation.
- [ ] Run dependency and supply-chain checks.
- [ ] Fuzz parsers for PDFs, workbooks, manifests, JSON, paths and IPC inputs.
- [ ] Test forged frontend commands and direct backend invocation.
- [ ] Test permission revocation during active work.
- [ ] Test race conditions, crash recovery and concurrent operations.
- [ ] Test memory, CPU, disk and output exhaustion.
- [ ] Test offline mode with network attempts from every component.
- [ ] Run Linux and Windows CI for every relevant batch.
- [ ] Run physical Windows acceptance for Sandbox, filesystem, installer, microphone and OS-specific behavior.
- [ ] Maintain a known-limitations list and do not market unverified controls as guarantees.

## 17. Difficulty, time and dependency guide

These are **relative engineering estimates**, not promises or calendar deadlines. They assume one primary developer using AI assistance, an existing React/Tauri/Rust codebase, coherent batches, and no major upstream/provider blockers. Physical-device testing, provider approvals, packaging/signing and security review can extend them.

| Work area | Relative difficulty | Rough effort | Why |
|---|---|---:|---|
| Settings screens and explanatory documentation | Easy–Medium | 1–3 weeks | Mostly UI once real backend settings exist. |
| Basic Skills Store catalogue UI | Easy–Medium | 1–3 weeks | Listing/search/status is simpler than secure package execution. |
| Basic voice selection UI | Easy–Medium | 1–2 weeks | UI is simple; actual speech engines and licences are separate. |
| Core app UX polish/accessibility | Medium | 2–4 weeks | Many small integration and regression details. |
| PDF text extraction and page-aware reading | Medium | 2–5 weeks | Mature parsers help; malformed-file safety and OCR add work. |
| Excel read/analysis MVP | Medium–Hard | 3–7 weeks | Workbook formats, formulas, fidelity and resource limits. |
| CV Studio MVP | Medium | 3–6 weeks | Import, templates, editing and export fidelity. |
| Basic local engine adapter | Medium | 2–4 weeks | Depends on existing Ollama integration and common interface. |
| Model catalogue/download/selection | Medium–Hard | 3–6 weeks | Verification, licences, disk/RAM and lifecycle. |
| Web Research workflow | Medium–Hard | 3–6 weeks | Search, source quality, citations and injection resistance. |
| Skill manifest/lifecycle/store security | Hard | 4–8 weeks | Trust, isolation, update/rollback and revocation. |
| Permission Manager and file trust boundary | Hard | 5–10+ weeks | Backend enforcement, race resistance and adversarial tests. |
| Privacy modes with real network enforcement | Hard | 4–8+ weeks | OS-level enforcement and trustworthy state are difficult. |
| Voice input/output with local options | Medium–Hard | 3–7 weeks | Engine packaging, language quality, hardware and licences. |
| Email OAuth and approval workflow | Medium–Hard | 3–6 weeks | Provider scopes, token security and real-account testing. |
| Job Hunter | Medium–Hard | 3–6 weeks | Permitted sources, changing APIs and accurate matching. |
| Coding Assistant Stage 4 | Very Hard | 8–16+ weeks | Windows Sandbox, filesystem race resistance, transfer, resource enforcement and physical acceptance. |
| Secure installer/update/release pipeline | Hard | 4–8+ weeks | Signing, rollback, SBOM, OS matrix and release validation. |
| Commercial/licensing/account readiness | Hard | 4–10+ weeks | Legal/licence review, operational support and privacy design. |

**Important:** Effort ranges overlap when shared infrastructure is built once. Do not add every row as if each were independent. A minimal useful FRIDAY can ship before every optional module is complete. A secure, polished, commercial-grade product takes substantially longer than a functional prototype.

## 18. Time-saving implementation strategy

The best way to save time is to build shared foundations once and then deliver narrow, complete user-visible capabilities on top of them.

### Recommended order

1. **Finish the current Coding Assistant security track without bypassing gates.** Its filesystem boundary and Windows Sandbox work is already underway; do not abandon or destabilize it.
2. **Audit and consolidate shared foundations** (settings, storage, permissions, skills, engine interface) before creating parallel implementations.
3. **Complete the permission and data-access contract** so every future skill uses the same backend grants.
4. **Complete the skill lifecycle and resource-aware loading** so modules can be installed independently and do not all remain resident.
5. **Create the common engine/model manager** and verify local-only behavior before adding many model/provider options.
6. **Ship read-only local document capabilities first:** PDF Reader, then Excel analysis. Add approved editing only after read/write safety is shared and tested.
7. **Deliver Web Research as an optional connected skill** with explicit network grants and citations.
8. **Add settings and Transparency Centre over verified backend state**, not placeholder toggles.
9. **Add voice using the shared provider/licence/permission model.**
10. **Add CV Studio, then Email Assistant, then Job Hunter** using common document, secret, OAuth and approval services.
11. **Harden distribution, updates, licensing and release documentation** before broad release.
12. **Consider accounts/payments only after the product works locally and privacy review is complete.**

### Time-saving rules

- [ ] Do not build separate permission systems for each skill.
- [ ] Do not build separate model/provider selectors inside each module.
- [ ] Do not duplicate file preview/backup/undo logic in PDF, Excel, CV and File Assistant.
- [ ] Build common document import/export and safe preview components.
- [ ] Build one shared approval service for file writes, email sends and job submissions, with operation-specific policies.
- [ ] Build one common settings schema and validation layer.
- [ ] Use lazy loading; don't install every optional runtime with the core app.
- [ ] Prefer an MVP that reads and explains before adding risky write/execute behavior.
- [ ] Avoid adding multiple cloud providers until one provider adapter and disclosure workflow are correct.
- [ ] Avoid building account/payment infrastructure before product-market and privacy requirements are clear.
- [ ] Keep CI fast with focused tests per batch and full Linux/Windows checks at integration milestones.
- [ ] Use feature flags only for incomplete UI exposure; never use them as a substitute for backend security enforcement.

## 19. Release gates

FRIDAY is not ready for broad release until all applicable gates pass:

- [ ] Core app installs, launches, upgrades, uninstalls and recovers cleanly on supported Windows systems.
- [ ] Local AI works with no cloud account and no silent external fallback.
- [ ] Privacy modes accurately reflect and enforce actual network behavior.
- [ ] Personal files remain inaccessible until explicitly granted.
- [ ] Revocation takes effect for active and future operations.
- [ ] Skills can be installed/disabled/unloaded/uninstalled without residual permissions or processes.
- [ ] Model/voice/provider licences and costs are accurately disclosed.
- [ ] Every high-impact action has clear review and approval.
- [ ] Data export, backup, deletion and recovery work.
- [ ] Core skills pass functional, adversarial and resource-limit tests.
- [ ] Coding Assistant guest execution remains disabled unless its separate master checklist and physical acceptance gates are all complete.
- [ ] Security, privacy, licence and release documentation is published.
- [ ] Known limitations are visible and honestly described.

## 20. Handoff template for every future batch

Append or update a short record here after each meaningful batch:

| Field | Value |
|---|---|
| Batch / task | |
| Section and checklist item | |
| Branch | |
| Base commit | |
| Result commit | |
| CI workflow run | |
| Linux result | |
| Windows result | |
| Tests added/changed | |
| Physical/provider acceptance | |
| Checklist status changes | |
| Known limitations | |
| Next dependency-ready task | |

## 21. Current next steps

- [ ] Verify the current branch HEAD and CI; do not assume the #988 audit baseline is still current.
- [ ] Continue with the first unchecked dependency-ready task in `docs/CODING-ASSISTANT-COMPLETION-MASTER.md`: audit and harden handle-relative filesystem traversal and parent identity checks, keeping guest execution disabled.
- [ ] Do not start parallel implementation of new product modules until the active security batch is complete or a deliberate roadmap reprioritization is recorded.
- [ ] After the current Coding Assistant safety track, begin the shared permission/skill/engine foundations in the recommended order in section 18.

---

**Maintainer rule:** This document records the whole product plan; `CODING-ASSISTANT-COMPLETION-MASTER.md` remains the detailed source of truth for Coding Assistant implementation status. When the two overlap, keep this document as the product-level index and the dedicated checklist as the detailed engineering record.

## 22. Confirmed product decisions and OpenJarvis reuse policy

**Decision status:** Product direction accepted by the product owner on 2026-11-19. These are requirements and architecture decisions, not claims that the features are already implemented.

### 22.1 Product identity and visual direction

- [ ] FRIDAY is an independently branded desktop assistant and product; OpenJarvis is a candidate upstream foundation, not the product identity.
- [ ] Adopt a dark navy and electric-blue visual direction, with restrained cyan accents; support accessible contrast and a light theme where practical.
- [ ] Build the **FRIDAY Control Centre** as the central place to manage features, skills, engines, models, permissions, privacy, memory, voice, appearance, security and updates.
- [ ] Include a dedicated **Features** page so users can inspect and enable/disable available capabilities without confusing a feature toggle with a security permission.
- [ ] Do not copy an upstream visual design merely because its underlying functionality is reused.

### 22.2 Reuse-first engineering policy for OpenJarvis and other upstream projects

- [x] Complete an initial source/docs audit of OpenJarvis architecture, current main revision, platform/dependency declarations, root licence and documented security model. Evidence: [OPENJARVIS-INTEGRATION-AUDIT-AND-MIGRATION-PLAN.md](OPENJARVIS-INTEGRATION-AUDIT-AND-MIGRATION-PLAN.md). A pinned-release, transitive-dependency and full Rust-crate compatibility audit is still pending.
- [ ] Complete the feature-by-feature source mapping for every FRIDAY requirement, including API/test evidence and a reuse/adapt/wrap/retain/build/defer decision.
- [ ] Prefer reusing, adapting or wrapping a suitable upstream capability over independently rebuilding equivalent functionality.
- [ ] Do not duplicate an upstream feature without documenting why it fails FRIDAY's requirements (for example: security, privacy, reliability, platform support, resource use, licence or maintainability).
- [ ] Do not adopt an upstream capability merely because it exists: review its implementation, permissions, data flows, dependencies and failure behavior first.
- [ ] Preserve valuable FRIDAY code where it meets or exceeds the required behavior; do not perform a wholesale rewrite for cosmetic or architectural preference alone.
- [ ] Keep integration boundaries replaceable so FRIDAY can maintain or replace an upstream component if it becomes unsuitable.
- [ ] Track upstream origin, version/commit, modifications, licence, required notices and third-party dependency obligations in a software bill of materials and third-party notices.
- [ ] Keep FRIDAY's branding, product experience, policies and original features independently maintained. Submit generally useful improvements upstream when appropriate, but do not depend on upstream accepting them.

### 22.3 User security, permissions, recovery and revocation

- [ ] FRIDAY must not receive unrestricted access to a user's desktop or personal files by default.
- [ ] Installing a skill, enabling a feature, selecting a model or asking FRIDAY to perform a task must not silently grant filesystem, application, account or network permissions.
- [ ] Use least-privilege, narrowly scoped, purpose-specific grants enforced by trusted backend code.
- [ ] Users can inspect and revoke a skill's permissions at any time from the FRIDAY Control Centre.
- [ ] Revocation must prevent new operations and cancel or safely terminate active operations where possible; revoke associated tokens, sessions and pending approvals. Clearly disclose any operation that cannot be safely interrupted immediately.
- [ ] Sensitive writes, deletes, sends, purchases, job applications, account changes and financial actions require a suitable preview and explicit approval.
- [ ] Use backups, transaction journals, verification and undo/restore for supported changes. Explain actions that are inherently irreversible or cannot be fully restored.
- [ ] Maintain a privacy-safe audit history showing what skill/provider acted, what permission was used, what category of data was accessed or transmitted, and the outcome—without unnecessarily recording sensitive content.
- [ ] Never describe a third-party service as safe merely because it is free, popular or open source; communicate known limitations and unknowns honestly.

### 22.4 Skills Store, modular installation and resource behavior

- [ ] Provide a **Skills Store** where users can discover, inspect, install, update, disable, roll back and uninstall optional skills independently of the core application.
- [ ] Clearly distinguish official FRIDAY skills, upstream integrations and community/third-party skills, including publisher, version, licence, provenance and requested capabilities.
- [ ] Installation must be separate from enabling; enabling must be separate from granting permissions.
- [ ] Skills should be lazy-loaded and unloaded when inactive where technically safe. An installed skill may use disk space, but inactive skills should not keep unnecessary runtimes, models, processes or substantial memory resident.
- [ ] Show estimated disk requirements, dependencies, resource behavior, network access and costs before installation/activation.
- [ ] Updates must be integrity-checked, atomic where possible, and recoverable through rollback.
- [ ] Disabling or uninstalling a skill must revoke its grants and clean up its owned sessions/processes without touching unrelated user resources.

### 22.5 Pricing, local capability and paid-provider transparency

- [ ] Product policy: FRIDAY's core application and first-party FRIDAY skills are intended to be usable without an additional FRIDAY feature subscription or per-skill fee.
- [ ] Local models and compatible local processing should be available without cloud inference charges, subject to hardware, model licence and download requirements.
- [ ] Optional paid cloud models/APIs may incur charges from their provider. Display the provider, pricing/billing basis where known, and whether the user pays the provider directly or through another disclosed arrangement.
- [ ] Do not imply that a paid model/API is included for free merely because its connector is included in FRIDAY.
- [ ] Disclose third-party services whether free or paid. Before connection or data transfer, explain the provider's identity, purpose, data categories that may be sent/accessed, permissions/scopes, relevant retention/training terms where verifiable, costs and how to disconnect.
- [ ] Show which engine/provider handled each request and whether processing was local or external.
- [ ] Provide a clear way to disconnect a provider, revoke credentials/scopes and return to local-only operation.
- [ ] Never silently fall back from local inference to a cloud provider.
- [ ] Clearly label any optional third-party costs (job-board services, email/API services, data feeds, exchange fees or other external charges). Do not promise that every external service is free.
- [ ] Treat “free” and “private/safe” as separate properties; make no blanket safety or privacy guarantee for a third-party service.

### 22.6 Transparency Centre and privacy controls

- [ ] Provide a **Transparency Centre** within the FRIDAY Control Centre.
- [ ] Show connected providers and third-party services, their connection status, permissions/scopes, last use and the skills/features that can invoke them.
- [ ] Show what categories of information may leave the device for each provider, when a transfer occurs, and the purpose.
- [ ] Show actual runtime-observed engine/provider and local-versus-cloud processing status rather than relying only on settings labels.
- [ ] Provide clear local-only and strict-offline controls, with backend/OS-level enforcement appropriate to the platform; a UI toggle alone is not sufficient.
- [ ] Explain unknown or unverifiable provider data practices as unknown; link to current provider terms/privacy information where available.
- [ ] Let users disconnect a provider and revoke its permissions without uninstalling unrelated skills.

### 22.7 Feature priorities confirmed by the product owner

- [ ] **Excel Intelligence is a must-have first-party skill:** workbook understanding, data cleaning, formula assistance, analysis, charts, reporting and carefully approved workbook edits with backup/undo. Prefer local processing where feasible.
- [ ] **Job Hunter is a major planned skill:** discover and compare relevant opportunities, explain source and fit, prepare tailored application materials and track applications. Use only permitted platform access and respect each job platform's terms; submitting an application requires user approval.
- [ ] **Crypto Intelligence / Trading is a later, separate, optional skill:** begin with research, backtesting and paper trading. Live trading must be disabled by default, use narrowly scoped exchange credentials, have explicit user-configured risk limits and approvals, and never require withdrawal permissions. No profit or performance guarantees.
- [ ] Continue the broader planned catalogue (File Assistant, PDF Reader, Web Research, Coding Assistant, CV Studio, Email Assistant and Voice) through the same skill, permission, provider and transparency contracts.
- [ ] New first-party skills should be deliverable through independently verified Skills Store updates without requiring a full application reinstall where architecture permits.

### 22.8 Low-cost development and honest acceptance

- [ ] Prefer GitHub-hosted automation, reproducible tests, static analysis and available free development infrastructure while project funding and local hardware are unavailable.
- [ ] Do not make paid cloud infrastructure or a personal test computer a prerequisite for work that can be safely implemented and verified through repository-based tests.
- [ ] Keep physical-device, supported-Windows Sandbox, real hardware, provider-account and other environment-specific acceptance explicitly marked pending until actually performed.
- [ ] Do not claim compatibility with a hardware tier or OS configuration based solely on mocks or hosted CI. Record tested configurations and limitations.
- [ ] Prepare safe, self-contained acceptance kits for volunteer testers where needed; no tester should have to grant AI remote access or expose personal files/accounts. Tests must operate only on explicitly created disposable test resources.

### 22.9 Architecture decision gate before migration

- [ ] Produce a feature-by-feature OpenJarvis-versus-FRIDAY comparison: capability, source location, licence, security review, resource behavior, platform support, test evidence, integration effort and recommendation (reuse/adapt/wrap/retain/build/defer).
- [ ] Confirm the exact upstream repository and licence at the revision selected for integration; re-check all bundled and transitive dependency licences.
- [ ] Decide whether to integrate OpenJarvis as a library/service, selectively port components, or fork it only after the comparison. Do not assume a wholesale fork is automatically the fastest or safest route.
- [ ] Preserve the existing FRIDAY branch/history and Coding Assistant security work. Any migration must be incremental, reviewable and reversible.
- [ ] Update this roadmap and the relevant subsystem checklists with the chosen architecture, migration sequence, compatibility strategy and rollback plan before broad code migration.

**Non-negotiable product principles reaffirmed:** FRIDAY must not compromise user security for convenience; core FRIDAY functionality and first-party skills should not require extra FRIDAY fees; users decide what FRIDAY can do, what it can access and which AI engines/providers it may use.

## 23. OpenJarvis audit result and deliberate roadmap reprioritization

**Audit record:** [OpenJarvis Integration Audit & FRIDAY Migration Plan](OPENJARVIS-INTEGRATION-AUDIT-AND-MIGRATION-PLAN.md)

**Audit baseline:** FRIDAY `feature/phase-2-skills` at `6cba2052157861aef56f550764db27a284d80921`; OpenJarvis `main` inspected at `427c2dd11d180ac254a03e92c5fb01189fb4b23b` on 2026-11-19.

### Initial findings

- OpenJarvis is a broad local-AI framework with Python modules, Rust crates, a React frontend and Tauri desktop packaging. FRIDAY already has React/Tauri/Rust, so a whole second desktop application/backend stack is not automatically beneficial.
- Valuable candidates include engine/model abstractions, agent orchestration, tool registry, skills catalogue/import, memory, research, speech, scheduling, telemetry and connectors. The Rust engine crate is the strongest narrow integration candidate: it exposes Ollama and OpenAI-compatible adapters, but its 0.1.0 API, blocking generation path, dependency footprint and tool-call boundary still need a compile spike.
- OpenJarvis is marked Alpha in its package metadata and requires Python >=3.11,<3.14. It also has a Rust workspace (Rust 1.89 minimum), React frontend and Tauri 2 shell. Its core dependencies and optional extras are substantial; adding the entire stack to FRIDAY's core installer would conflict with low-resource and modular-install goals.
- Its documented system-access model includes powerful shell and file tools. The upstream system-access guide documents arbitrary shell execution as the current user, directory restrictions that do not constrain shell/patch/code-interpreter tools, and desktop/server confirmation behavior that differs from interactive CLI behavior. Both Python and Rust capability policies default to allow unless explicitly configured deny-by-default. The inspected Tauri capability file also grants shell execute/spawn/stdin/kill permissions.
- The inspected Rust hardware detector uses Unix `which`, `/proc/cpuinfo` and `/proc/meminfo` paths and does not yet provide reliable native Windows CPU/RAM/GPU detection. Keep FRIDAY's native Windows resource queries for Windows. The upstream root licence is Apache-2.0, while the desktop Tauri crate manifest declares MIT; this needs provenance/maintainer clarification before reusing that crate. Therefore upstream security/tool defaults must not become FRIDAY's permission authority unchanged. FRIDAY Rust backend remains the authority for user grants, exact approvals, revocation, file safety and recovery.
- FRIDAY's current skill catalogue is compile-time and first-party; package loading currently loads declarative instructions and explicitly reports that skill tools are not yet connected. This is a useful starting point, not a complete third-party Skills Store.
- Preserve the existing FRIDAY file-edit and Coding Assistant security work. Coding Assistant guest execution remains disabled.

### Roadmap reprioritization and current next steps

The product owner directed us to investigate reuse before building duplicate capabilities. This remains a deliberate reprioritization, not a claim that Coding Assistant is complete.

- [x] Preserve the existing Coding Assistant security work and keep guest execution disabled.
- [x] Complete the initial OpenJarvis source/API/platform inspection at a pinned upstream commit.
- [x] Build and test the isolated Rust engine compatibility spike on Linux and Windows.
- [x] Record the spike result and a conditional integration decision in the audit document.
- [x] Complete the pinned OpenJarvis Rust engine/API/dependency review and document the final production decision.
- [x] Run the isolated Rust engine compatibility spike on Linux and Windows (workflow [44540769399](https://github.com/Friday-AI-Personal-Assistant/FRIDAY/actions/runs/44540769399), both PASS).
- [x] Decide **NO-GO** on adopting `openjarvis-engine` in FRIDAY production: the existing FRIDAY Ollama path is already async, bounded and fixed to loopback; the upstream generation API is blocking and brings additional dependency/policy work. See the audit's section 11.3.
- [ ] Complete the broader feature-by-feature OpenJarvis-versus-FRIDAY capability matrix for other candidate subsystems only where reuse may provide a concrete net benefit.
- [ ] Resume Coding Assistant Stage 4 at section 6A: continue hardening handle-relative snapshot traversal, parent identity and TOCTOU/reparse-point defenses. Keep guest execution disabled.
- [ ] Continue the product roadmap after these security foundations: enforceable skill grants/revocation and provenance, Control Centre/Transparency Centre, then Excel Intelligence and Job Hunter.

**Compatibility experiment:** branch `experiment/openjarvis-engine-compat`; spike commit `3eec6b4d96cd6aafb5367789b561b1adb903dab6`; upstream pin `427c2dd11d180ac254a03e92c5fb01189fb4b23b`. Workflow [44540769399](https://github.com/Friday-AI-Personal-Assistant/FRIDAY/actions/runs/44540769399) completed successfully on both `ubuntu-latest` and `windows-latest`. This proves compile/API compatibility and basic trait properties only.

**Final integration decision:** do not add `openjarvis-engine` or its transitive dependency graph to the production application and do not replace FRIDAY's existing Ollama path. The upstream desktop crate's MIT metadata versus repository-root Apache-2.0 discrepancy remains unresolved and blocks copying that crate. Other OpenJarvis components remain candidates only if a feature-specific review demonstrates a measurable advantage without weakening FRIDAY's security, privacy or low-resource goals.

**Recent security batch:** commit `30db64f15165d41db0eb1ade19be2f32206cd530` adds deterministic source/workspace root replacement tests and revalidates both path-to-handle identities immediately before snapshot publication. Workflow run [44552742730](https://github.com/Friday-AI-Personal-Assistant/FRIDAY/actions/runs/44552742730): Linux PASS; Windows still running at the time of this update.

**Coding Assistant progress:** the user reports runs #991–#999 and #1000–#1002 passed. These are user-confirmed run outcomes; exact workflow URLs/commit SHAs have not been independently associated with those run numbers in this update. Do not infer that they close Stage 4 gates. The dedicated Coding Assistant checklist remains authoritative; physical Windows acceptance and guest execution remain pending.


### Selective OpenJarvis reuse — approved direction, not yet implemented

The audit is complete, but production component reuse has **not** started. FRIDAY will not wholesale-fork or replace its existing desktop/backend stack. Once the current Coding Assistant filesystem trust-boundary batch is safely advanced, perform feature-by-feature reuse reviews and integrate only components with a demonstrable net benefit.

- [ ] Build the broader capability matrix for specific candidates (document/PDF processing, speech, model catalogue/discovery, research, scheduling, memory and orchestration) rather than auditing or importing the entire repository indiscriminately.
- [ ] For each candidate, pin the upstream commit and verify source/package licences, transitive dependencies, maintenance status, Linux/Windows compatibility, install size, idle/active RAM and CPU behavior.
- [ ] Prefer a small isolated adapter or selective port over a second always-running service or mandatory Python stack.
- [ ] Route every model/tool action through FRIDAY's own Rust permission gateway, explicit user grants, revocation, approval and Transparency Centre; upstream trust tiers or confirmation prompts never substitute for these controls.
- [ ] Prove local-only mode has no cloud fallback; any optional provider must be disclosed, explicitly connected, bounded and revocable.
- [ ] Add focused tests proving reused components cannot access files, execute commands, use credentials or make network requests beyond FRIDAY-issued capabilities.
- [ ] Integrate one reviewed capability at a time with a rollback path; keep the existing FRIDAY implementation until the replacement passes parity and security checks.
- [ ] Build Excel Intelligence and Job Hunter as FRIDAY product capabilities on the shared, permissioned skill/tool architecture; do not assume OpenJarvis already satisfies those product requirements.

**Ordering:** continue Coding Assistant Stage 4A filesystem hardening first. Low-risk OpenJarvis research and capability comparison can proceed as a separate planning track, but no upstream code is imported merely to save time. The NO-GO decision for `openjarvis-engine` remains unchanged.