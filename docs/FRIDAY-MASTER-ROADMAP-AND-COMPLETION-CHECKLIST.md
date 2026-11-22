# FRIDAY Master Roadmap & Completion Checklist

> **Purpose:** This is the durable, cross-conversation source of truth for what FRIDAY is intended to become, what has been verified as implemented, what remains, the order of work, and the relative effort/risk of each area. A new development conversation must read this file and the Coding Assistant completion checklist before making changes.
>
> **Status baseline (2026-11-21):** Current development branch is `foundation/evaluation`; the first read-only Excel Intelligence workspace/backend passed Linux and Windows CI in #1199 and #1200. The follow-on Excel Data Quality Intelligence batch passed Linux and Windows CI in #1202–#1205; #1201 was an earlier failed iteration superseded by the corrected implementation. The roadmap update passed in #1206 (run `44930624372`). The bounded local natural-language Excel question-answering batch passed in #1211–#1215 (user-confirmed); earlier iterations #1207 and #1210 failed and were superseded. The next major product-engineering milestone is the reusable FRIDAY Domain Intelligence & Formula Engine, beginning with a strictly validated, read-only calculation core; Accounting Intelligence is its first specialist domain. Charts/reports and the permissioned first-party Excel Analyst skill remain future work. Coding Assistant remains in progress; guest execution remains disabled. Recovery cleanup is not authorized for production deletion. Community skills remain quarantined and inert; provenance/trust metadata and the Skills Store review UI are now CI-covered, but no skill execution bridge is enabled. Midnight Azure tokens and initial application-wide styling exist; integrated physical-device visual acceptance remains open. The current product focus is richer read-only Excel Analyst workflows; the next stages are workbook summaries/charts/reports and the permissioned first-party skill/tool bridge. Other product areas remain planned/partial unless repository evidence explicitly says otherwise.
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

### 1.1. The FRIDAY Four Non-Negotiable Product Guarantees

These four guarantees are permanent product requirements. They apply to every feature, skill, model/provider, integration, installer, update, automation and release. They must survive new chats, handoffs, roadmap changes and architecture migrations. This section is authoritative even when a conversation is unavailable or corrupted.

**Decision priority (highest to lowest): Safety → User control → Cost transparency → Reliability.** Time saved is a core product outcome and engineering objective, but it never overrides these four guarantees.

A feature must not be shipped merely because it is impressive, convenient or technically possible. If it fails any applicable guarantee, keep it disabled, redesign it, or defer it. A green CI run alone does not prove these product guarantees.

#### A. Safety — protect users, their devices and their data

- [ ] Apply least privilege and deny-by-default to every tool, skill, agent, connector and execution path.
- [ ] Enforce permissions in trusted backend code, not only in the UI, prompt, manifest or model instructions.
- [ ] Treat imported skills, repositories, documents, webpages, model output and tool results as untrusted input.
- [ ] Keep shell execution, arbitrary code execution, computer control, destructive file operations and external actions disabled until their isolation, authorization and recovery boundaries are independently tested.
- [ ] Require explicit confirmation for consequential actions such as sending, publishing, submitting, purchasing, deleting, overwriting, moving sensitive data or initiating financial transactions.
- [ ] Validate paths, identities, destinations, arguments, resource limits and trust boundaries at the point of use; defend against symlinks, traversal, race conditions, injection and confused-deputy behavior.
- [ ] Protect credentials and tokens using OS-appropriate secure storage; never expose secrets in logs, prompts, telemetry, crash reports or child-process environments unnecessarily.
- [ ] Use secure dependency practices: pinned/reproducible dependencies, provenance checks, vulnerability review, SBOM and license inventory.
- [ ] Fail closed for protected operations if policy, authorization, signature, isolation or security checks are unavailable or fail.
- [ ] State security limitations honestly. Do not claim absolute protection against a compromised operating system or malware running with the same user privileges.

#### B. User control — the user is always in charge

- [ ] Installing a feature, skill, model or connector must not silently enable it or grant it access.
- [ ] Every permission must be understandable, purpose-specific, narrowly scoped, time/session/task bounded where practical, visible, revocable and enforced by the backend.
- [ ] Show what resource is accessed, by which component, for what purpose, and whether data leaves the device.
- [ ] Provide clear approve, deny, cancel and stop controls. Cancellation must stop work where technically possible and report honestly if an operation cannot be interrupted.
- [ ] Let users inspect and manage active grants, connected accounts, scheduled jobs, background activity and pending approvals.
- [ ] Make local/offline mode technically enforce network restrictions; do not treat a visual toggle as enforcement.
- [ ] Never include hidden developer access, backdoors, remote file browsing, covert monitoring or central collection of private workspace contents.
- [ ] Provide user control over local data: inspect, export, correct and delete FRIDAY-managed data, with clear retention and backup behavior.
- [ ] Do not let an agent, skill, model, provider or remote service grant itself additional permissions or broaden an existing grant.

#### C. Cost transparency — no hidden fees or surprise bills

- [ ] The core FRIDAY experience must remain usable without paid cloud services wherever technically feasible.
- [ ] Clearly distinguish free/local functionality from optional paid providers, subscriptions, usage-based APIs, hosted tools and third-party services.
- [ ] Before enabling a paid service or making a billable request, identify the provider, what data is sent, the pricing basis and any known usage/cost limits.
- [ ] Obtain informed user consent before the first paid use; provide a way to disable the provider and avoid accidental repeat charges.
- [ ] Never silently fall back from a local model/provider to a cloud or paid provider.
- [ ] Never hide API usage, subscriptions, metered storage, voice charges, data-transfer charges or other recurring costs.
- [ ] Where exact costs cannot be known in advance, explain the uncertainty, show available estimates/usage limits and let the user set a budget or disable usage.
- [ ] Disclose third-party licensing or commercial-use restrictions for code, models, voices, datasets, assets and services. An open-source code licence does not automatically make bundled models or services free.

#### D. Reliability — predictable, recoverable and honest behavior

- [ ] Define acceptance criteria and failure behavior before implementing each consequential capability.
- [ ] Add automated unit, integration, regression and adversarial tests at the relevant trust boundaries.
- [ ] Test supported Windows and Linux paths in CI where applicable; separately perform real-device/provider acceptance for behavior CI cannot establish.
- [ ] Use bounded timeouts, resource limits, cancellation, retries with safe semantics and clear error reporting.
- [ ] Make updates, writes, migrations and multi-step actions atomic or recoverable where possible; preserve backups and transaction evidence.
- [ ] Make operations idempotent where practical and prevent duplicate submissions, charges or external actions after retries.
- [ ] Never report success unless the operation's outcome was verified. Distinguish planned, implemented, CI-passed and physically accepted behavior.
- [ ] Provide diagnostics that protect private data, plus a safe recovery path for failed updates, interrupted work and corrupted state.
- [ ] Pin and review dependencies, maintain compatibility and document known limitations and support status.

#### Mandatory release gate

Before any feature is marked DONE or enabled for users, record evidence for all four guarantees:

- [ ] Safety review completed; threat model, abuse cases and negative tests recorded.
- [ ] User permissions, scopes, revocation, cancellation and backend enforcement tested.
- [ ] Pricing, provider, network/data-transfer and licensing disclosures verified; paid use cannot occur without informed consent.
- [ ] Reliability tests, failure/recovery behavior and supported-platform acceptance recorded.
- [ ] Remaining limitations, residual risks and required user actions documented.
- [ ] If any applicable gate is not met, mark the feature BLOCKED, LIMITED or DISABLED — never DONE.

#### Reuse-first and time-saving engineering rule

FRIDAY should save the user's time and avoid unnecessary development overhead. Prefer mature, maintained, well-tested open-source projects and libraries over rebuilding equivalent functionality. Before adopting anything, verify its source, maintenance, architecture, security model, platform support, dependencies, licences, assets, provider terms and hidden costs. Reuse components selectively; do not blindly replace FRIDAY or merge entire assistant applications. Preserve existing FRIDAY work, use reversible branches/migrations, and build only the gaps that are genuinely specific to FRIDAY. A faster implementation is unacceptable if it weakens any of the four guarantees.

When choosing between features or implementation approaches, prioritize: (1) safety, (2) user control, (3) cost transparency, (4) reliability, then (5) usefulness and time saved. If life, device limitations or engineering capacity require scope reduction, reduce scope rather than these guarantees.

## 1.1. Coding Assistant filesystem-boundary progress (through #1055)

- #1009 added a shared scan-entry budget to source snapshot collection; #1010 fixed Rust test closure annotations and Linux/Windows CI passed.
- #1011 added a shared entry budget to the candidate-file walker; Linux and Windows CI passed.
- #1012 extended shared entry limits to exact snapshot inventory and cleanup-progress preflight; subsequent batches hardened recovery, cleanup, identity-aware traversal, project reads and approved-edit rename behavior.
- #1054 added Unix handle-relative exclusive creation for approved-edit backups, journals, replacement temporaries and Undo records; #1055 fixed a Linux compile ambiguity and passed Linux/Windows CI (run [44759117667](https://github.com/Friday-AI-Personal-Assistant/FRIDAY/actions/runs/44759117667)). Windows equivalents and later transaction verification/rollback/recovery remain open.
- Guest execution remains disabled. CI does not establish physical Windows Sandbox, VHDX/NTFS, or end-to-end result-transfer acceptance.

## 2. Overall status snapshot

| Product area | Current status | Notes |
|---|---|---|
| Coding Assistant | IN PROGRESS | Substantial read/analysis/approved-edit foundations exist. Guest execution remains deliberately disabled; see the dedicated completion checklist. |
| File Assistant | DONE — LIMITED / VERIFY CURRENT TESTS | Backend file operations, previews, approved edits, moves/renames, backups, undo and interrupted transaction recovery were previously audited as implemented. Re-verify current HEAD before release. |
| Skills framework | IN PROGRESS | Skill install/enable/load/unload and package integrity are implemented; tool connections and complete catalogue experience remain. |
| Web Research skill | PLANNED / PARTIAL | Skill instructions exist; integrated research tools, citations and report workflow are not verified complete. |
| PDF Reader skill | PLANNED / PARTIAL | Skill instructions exist; dedicated extraction, page-aware citations and complete UI workflow are not verified complete. |
| Excel Intelligence skill | IN PROGRESS — READ-ONLY WORKSPACE | XLSX/CSV preview, bounded summaries and data-quality hints are CI-verified; natural-language analysis, charts/reports and permissioned skill integration remain. |
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

**Initial delivery slice (DONE — VERIFIED, CI #1199/#1200):** a read-only local XLSX/CSV analyzer with explicit file picking, workbook/sheet overview, bounded preview, missing-cell counts, numeric column statistics and formula counts. It does not modify workbooks, recalculate formulas, execute macros or follow external links. This is the first Excel Intelligence workspace—not yet the complete Excel Analyst skill or the full product scope.

**Data Quality Intelligence (implemented; CI pending):** bounded local checks for blank and duplicate headers, mixed data types, exact duplicate data rows, and potential numeric outliers using the 1.5× IQR rule. Findings are capped, include limited sample row/column references, and are explicitly presented as review hints—not confirmed errors. No data is changed or removed.

- [x] Safe bounded import for `.xlsx` and `.csv` files. XLSX input is ZIP-signature checked and package entries, expanded size, per-entry size, compression ratio, macro payloads, worksheet count and total analyzed cells are bounded. CSV parsing is bounded; legacy `.xls` is not supported in this first slice. [CI #1199/#1200 passed Linux and Windows.]
- [x] Show bounded sheet preview, missing-value counts, formula counts and numeric column summaries. [CI #1199/#1200 passed Linux and Windows.]
- [x] Detect blank and duplicate headers, mixed data types, repeated non-empty rows and potential IQR numeric outliers. Findings and sample references are bounded; outliers are review suggestions, not errors. [Current Data Quality batch; CI pending.]
- [ ] Enumerate tables, named ranges, dimensions and hidden content.
- [ ] Preview selected ranges before analysis.
- [ ] Explain formulas and detect common formula errors/inconsistencies.
- [ ] Perform richer summaries, filtering, comparisons, trends and statistical analysis.
- [ ] Generate charts and pivot-style summaries where supported.
- [ ] Export useful analysis reports.
- [ ] Preserve formulas, styles, merged cells, references and workbook integrity when editing.
- [ ] Propose edits as a preview/diff and require approval before writing.
- [ ] Keep original file backed up and provide verified undo/recovery.
- [x] Never execute workbook macros or external links automatically. Macro-enabled workbooks are rejected; external links are not followed. [CI #1199/#1200 passed Linux and Windows.]
- [x] Add initial XLSX package and worksheet resource bounds to reduce zip-bomb and oversized-workbook risk; formulas remain data and are never executed. [CI #1199/#1200 passed Linux and Windows.] Continue adversarial parser testing before expanding supported formats.
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
### Coding Assistant Stage 4A — bounded enumeration checkpoint

- [ ] Enforce a separate maximum count of inspected directory entries, counting ignored files and excluded directories as well as accepted files.
- [ ] Ensure entry-budget exhaustion marks the collection incomplete and blocks snapshot publication.
- [ ] Preserve exact-boundary behavior and add fanout regression tests on Linux and Windows CI.
- [ ] Continue the source traversal, parent identity, reparse-point and cleanup-boundary review; guest execution remains disabled.

Latest verified checkpoint: #1008 passed Linux and Windows CI (commit `e49eb5160d5ddc713f31ca14844db55a6e421184`, run [44557584318](https://github.com/Friday-AI-Personal-Assistant/FRIDAY/actions/runs/44557584318)).

### Native capability policy — first increment (foundation/evaluation)

- [x] Preserve the existing one-use, exact-tool-and-arguments approval tokens for consequential built-in tool actions.
- [x] Add a Rust-owned capability policy mapping each currently registered built-in tool to an explicit capability.
- [x] Deny unknown tool IDs, capability mismatches and approval-policy weakening by default.
- [x] Add regression tests for allowed local computation, approval-required desktop launch, unknown-tool denial and declaration tampering.
- [ ] Verify the new policy increment on both Windows and Linux CI before marking it complete.
- [ ] Extend backend authorization to the file and Coding Assistant command families without bypassing their existing scoped grants, previews and transaction approvals.
- [ ] Add durable, inspectable permission grants with narrow scopes, expiry, revocation and audit before introducing third-party executable skills or broad integrations.
- [ ] Build the user-facing Permissions/Transparency Centre only on top of backend-enforced grants; UI visibility is not authorization.

**Scope note:** the first increment covers the existing built-in tool dispatcher only. It is not yet a universal Tauri-command permission gateway or persistent permission manager. Keep shell, unrestricted plugin execution, unattended computer control and paid cloud fallback disabled until their separate gates pass.


### File and Coding Assistant backend capability enforcement

- [x] Assign explicit backend capability IDs to all registered File Assistant command entry points.
- [x] Assign explicit backend capability IDs to all registered Coding Assistant command entry points.
- [x] Add a separate capability for local Coding Assistant structural analysis.
- [x] Deny unknown command IDs and capability substitutions in the central Rust policy.
- [x] Preserve existing folder/project-scoped grants, per-operation previews, approvals, hash checks, backups and undo protections.
- [x] Verify this increment with Windows and Linux CI (#1076, [workflow 44796838718](https://github.com/Friday-AI-Personal-Assistant/FRIDAY/actions/runs/44796838718)).
- [x] Extend the backend capability audit to sandbox lifecycle and guest-execution approval entry points while guest execution remains disabled.
- [ ] Design and implement durable user-visible permission grants, revocation, expiry and audit before enabling broader integrations or executable skills.

**Scope:** this is backend command classification/enforcement, not a persistent permission centre or OS-level sandbox. The UI cannot grant itself authority; feature-specific grants and operation tokens remain required where currently implemented.

### Sandbox lifecycle capability enforcement — implementation pending CI

- [x] Map preflight, snapshot preparation, recovery, configuration preview and snapshot discard to distinct backend capabilities.
- [x] Enforce each capability at the start of its Rust command handler.
- [x] Give guest-run approval receipt issuance, consumption and revocation separate narrow backend capabilities.
- [x] Confirm approval receipt endpoints do not launch guest processes; no guest process-launch command is registered.
- [x] Add regression tests for lifecycle mappings, capability substitution and guest-approval capability separation.
- [ ] Verify this increment with Windows and Linux CI.
- [ ] Complete physical Windows Sandbox launch/isolation acceptance before considering any guest execution enablement.
- [ ] Keep guest process launch, arbitrary commands, host fallback and network-enabled execution disabled.

**Scope:** this is a static backend command allowlist. Guest approval endpoints manage one-use receipts only; they do not execute code. No guest process-launch command is registered or enabled. This is not a persistent user-grant manager, universal Tauri IPC gateway or proof of OS isolation. Existing snapshot ownership and filesystem protections remain mandatory.


### Backend command coverage — workspace, history, memory and skills

- [x] Gate encrypted workspace initialization and private-note read/write operations with distinct Rust-owned capabilities.
- [x] Gate conversation list/read/write/delete operations independently.
- [x] Gate personal-memory list/add/update/enable-disable/delete operations independently.
- [x] Gate skill catalogue and install/enable/load/read-instructions/unload/uninstall operations independently.
- [x] Add tests rejecting unknown capability substitutions and cross-operation privilege substitution.
- [ ] Confirm Linux and Windows CI pass before closing this batch.
- [ ] Build durable user grants, expiry, revocation and audit separately; static command capabilities are not user consent.

**Important:** installing, enabling or loading a declarative skill does not grant it access to files, tools, accounts, network integrations or executable code.


### Backend command coverage — AI, research, PDF and tool activity (#1080)

- [x] Protect installed-model discovery/details, model installation/removal and local chat/Coding Assistant model requests with distinct backend capability IDs.
- [x] Protect PDF selection/extraction and web search/source-reading entry points.
- [x] Protect tool catalogue/history, history clearing, approval issuance and execution entry points.
- [x] Preserve exact-action one-use approvals for consequential built-in tools; command classification does not replace user approval.
- [x] Verify Linux and Windows CI: #1080 exposed a return-type mismatch in `list_tools`; #1081 corrected it, and both Linux and Windows CI passed.
- [ ] Add durable user grants, explicit consent, expiry, revocation and audit before broader integrations or executable skills.

**Important:** these are static Rust command gates, not persistent user permissions. Model downloads/removal, external web requests and other consequential operations still need clear user-facing disclosure and controls. No silent cloud fallback, shell execution or guest process execution is enabled.


### Streaming response cancellation registration and authorization (#1083)

- [x] Register `cancel_ollama_chat`, which the frontend already calls when the user presses Stop.
- [x] Protect cancellation with a distinct `ai.chat.cancel` backend capability.
- [x] Reuse strict bounded request-ID validation for both stream start and cancellation.
- [x] Add regression tests for invalid IDs and capability substitution.
- [ ] Verify Linux and Windows CI.

**User impact:** the existing Stop action can reach the Rust cancellation endpoint and target only its own active request.


### Registered backend command coverage — #1084

- [x] Inventory every currently registered Tauri IPC command (72 commands).
- [x] Add a source-backed manifest that must exactly match `tauri::generate_handler!`.
- [x] Test that every registered command enforces its own exact backend capability at the Rust handler boundary.
- [x] Remove the stale, unregistered legacy desktop-action command; retain the centrally authorized tool-dispatch route.
- [ ] Verify #1084 on both Linux and Windows CI.
- [ ] Implement durable user grants separately: explicit consent, narrow resource/action scopes, expiry, revocation and audit history.
- [ ] Do not treat static command capabilities, skill installation, enablement or loading as user permission grants.

**Four guarantees remain mandatory:** Safety, User control, Cost transparency and Reliability. Command-coverage tests do not substitute for consent, OS isolation, cost disclosure or physical-device acceptance.


### Persistent user permission engine and Web Research enforcement — #1085–#1087

- [x] Versioned, bounded, fail-closed permission store using atomic replacement.
- [x] Exact capability/resource grants with secure random IDs and expiry capped at one year.
- [x] Native OS confirmation required before grant persistence; cancellation means denial.
- [x] Bounded grant/revoke/expiry audit history and revocation API.
- [x] Start grant expiry after native confirmation; persist expired-grant transitions and cover expiry/revocation persistence and replay denial with tests.
- [x] Static backend policy entries and IPC inventory coverage for permission management commands.
- [x] Tests for expiry, exact scope, malformed storage, duplicate IDs and unknown capability denial.
- [ ] Verify both Linux and Windows CI.
- [ ] Connect grants to actual resource-owning handlers; no grant may bypass a separate one-use approval.
- [x] Add Midnight Azure Permissions & Transparency Centre UI with grant listing, native-confirmed exact-domain grants, revocation and audit history.
- [x] Enforce exact Web Research domain grants in Rust for DuckDuckGo search and selected page reads; re-check redirect destinations.
- [ ] Extend persistent grants to other feature handlers only when exact resource identity and safe consent UX are implemented.
- [ ] Ensure skill install/enable/load never silently grants permissions.

The four guarantees remain mandatory: Safety, User control, Cost transparency and Reliability.

## Midnight Azure design system — first application-wide implementation

**Design direction:** FRIDAY — Midnight Azure. The dark theme uses Midnight Navy `#050E1D`, Deep Slate Blue `#0E1A2D`, Muted Steel Blue `#192840`, Refined Electric Blue `#2874D8`, and Soft Ice Grey `#A9BBD2`. The light theme is a deliberate companion palette, not a simple inversion.

- [x] Add shared design tokens for the app shell, surfaces, borders, typography, muted text and primary actions.
- [x] Apply Midnight Azure to the global navigation, workspace shell, AI Chat, Coding Assistant, Modules/Skills, Memory, Settings and Permissions Centre.
- [x] Add an explicit dark/light theme switch with a saved user preference; dark remains the default.
- [x] Add visible keyboard focus treatment and respect reduced-motion preferences for theme transitions.
- [ ] Verify the visual result at desktop and narrow mobile widths, including contrast and overflow, on physical Windows hardware.
- [ ] Continue component-by-component refinement after the first integrated visual acceptance; do not claim independent accessibility certification.

**Scope note:** this batch is the first shared application-wide styling pass. It does not redesign every screen's information architecture, and browser/desktop previews are not a substitute for physical-device visual acceptance. The four guarantees remain Safety, User control, Cost transparency and Reliability.

## Settings & user preference controls — first functional settings surface

- [x] Replace the non-functional Settings placeholder with a real Appearance control for Midnight Azure and Light Azure.
- [x] Keep one shared theme update path for the top-bar toggle and Settings; save the selected theme locally on the device.
- [x] Add truthful navigation to existing AI Chat model controls and the Permissions & Transparency Centre.
- [x] Clearly label language, voice, startup behavior, notifications, cloud-provider accounts and cross-device sync as not yet configurable.
- [x] Add responsive theme-selection cards and selected-state accessibility semantics.
- [ ] Validate visual contrast, keyboard operation and narrow-window layout on physical Windows hardware.
- [ ] Future settings must use explicit, typed preferences with safe defaults; paid-provider, network and background settings require the four-guarantee release gate.

This batch makes only the documented appearance preference functional. It does not imply that unsupported settings or provider integrations exist.


### Saved-memory prompt controls and transparency — #1091

- [x] Add a device-persisted user control to include or exclude saved memories from future local AI chat prompts.
- [x] Preserve saved memory entries when prompt inclusion is disabled; disabling context is not deletion.
- [x] Show a preview of the exact saved-memory text that will be included, using the same 30-entry and 12,000-character limits as chat.
- [x] Keep the preference explicit and visible in Personal Memory; disable changes while a response is in flight so the current request cannot be misunderstood.
- [ ] Verify Linux and Windows CI.
- [ ] Physically validate the memory panel, preview overflow and preference persistence in the Windows desktop app.

**User impact:** FRIDAY no longer requires the user to accept saved-memory context on every future chat. The user can keep memories stored locally while opting out of sending them to the local model. The control does not delete old conversation history or alter a request already in progress. No remote model provider is enabled by this change.

The four guarantees remain mandatory: Safety, User control, Cost transparency and Reliability.


### Chat history discovery and usability — #1092

- [x] Include a bounded preview of each conversation's latest message in the encrypted-history listing response.
- [x] Let users search saved conversations by title or latest-message preview without loading every full conversation into the frontend.
- [x] Show message count and last-updated date in each history item; keep full conversation content loaded only when the user opens that chat.
- [x] Keep previews bounded to 180 Unicode characters and reuse the existing encrypted local store; no new network access or data export.
- [ ] Verify Linux and Windows CI.
- [ ] Physically validate history readability, truncation and narrow-window layout in the Windows desktop app.

**User impact:** users can recognise and find saved chats more easily without opening each one. Search intentionally covers the title and latest message preview, not every historical message. Full messages remain encrypted at rest and are fetched only when a conversation is opened. The four guarantees remain mandatory: Safety, User control, Cost transparency and Reliability.


### Conversation portability — #1093

- [x] Add a clearly labelled Export chat action to the Chat toolbar.
- [x] Export the currently open conversation as a readable Markdown file, preserving message order and distinguishing the user from FRIDAY.
- [x] Generate a safe, bounded filename from the conversation title; export is explicit and initiated by the user.
- [x] Keep export local to the device/browser session: no upload, external service, or automatic background export.
- [x] Disable export when there are no messages or while a response/save operation is in progress.
- [ ] Verify Linux and Windows CI.
- [ ] Physically validate that the Windows desktop WebView presents the Markdown download/save flow clearly; test long conversations and Unicode content.

**User impact:** users can keep a portable copy of their own conversations, read them in any Markdown editor, or move them to another system. Export includes only the currently open conversation and does not delete or modify FRIDAY's encrypted local history. Exported files are ordinary unencrypted files; users should store them somewhere appropriate. The four guarantees remain mandatory: Safety, User control, Cost transparency and Reliability.


### Conversation organisation — pinning and reversible archive (#1096)

- [x] Extend encrypted conversation records with pinned and archived metadata while defaulting missing fields to false so existing history remains readable.
- [x] Add narrowly authorised backend commands for pin/unpin and archive/restore; preserve metadata when an existing conversation is renamed or saved again.
- [x] Sort pinned conversations first, then by most recently updated; expose metadata in bounded history summaries.
- [x] Add Active and Archived history views, pinned/recent grouping, and one-click pin/unpin and archive/restore controls.
- [x] Keep archive reversible and separate from permanent deletion; retain the existing explicit confirmation for permanent deletion.
- [x] Keep all metadata inside the existing encrypted local conversation store; no network access or new permissions are introduced.
- [ ] Verify Linux and Windows CI.
- [ ] Physically validate history layout, pin/archive actions, legacy-history loading and archive restoration in the Windows desktop app.

**User impact:** important chats stay at the top, old chats can be moved out of the active list without being deleted, and archived conversations can be restored at any time. Existing encrypted chat files remain compatible. Permanent deletion remains a separate, explicitly confirmed action. The four guarantees remain mandatory: Safety, User control, Cost transparency and Reliability.


### Full-text local conversation search (#1097)

- [x] Search conversation titles and every saved user/FRIDAY message, case-insensitively, within the encrypted local store.
- [x] Keep search local-only: no cloud service, network request, third-party index or new external permission.
- [x] Scope results to the currently selected Active or Archived history view; retain pinned-first and recent-first ordering.
- [x] Debounce interactive search and bound queries to 1–200 characters and results to 100 conversations.
- [x] Return bounded matching-message previews and add unit coverage for older-message and title matches.
- [ ] Verify Linux and Windows CI.
- [ ] Physically validate search speed, Unicode matching, archive filtering and large-history behaviour in the Windows desktop app.

**User impact:** people can find older discussions and answers without opening conversations individually. Search reads the existing encrypted local history and never uploads message content. Search remains limited to the 500-conversation local history store and returns at most 100 matching conversations per query. The four guarantees remain mandatory: Safety, User control, Cost transparency and Reliability.


### Chat message control and correction workflow — #1100

- [x] Add an explicit Copy action to each populated user and FRIDAY message; report clipboard failures without claiming success.
- [x] Let users edit an earlier user message and resend from that point, replacing the later messages in the active conversation.
- [x] Keep the existing saved conversation unchanged until the user actually sends the edited message.
- [x] Clearly warn that sending an edit removes subsequent replies from the active conversation; provide a cancel path before sending.
- [x] Disable editing while generation or persistence is in progress; retain the existing local encrypted conversation persistence path.
- [ ] Verify Linux and Windows CI.
- [ ] Physically validate clipboard access, keyboard flow, long messages and the edit/cancel/resend experience in the Windows desktop WebView.

**User impact:** users can copy any message and correct an earlier prompt without manually rebuilding the conversation. Editing is non-destructive until Send is pressed; after sending, the conversation continues from the edited message and later turns are replaced. No message is sent to a remote service by these controls. The four guarantees remain mandatory: Safety, User control, Cost transparency and Reliability.

### Saved-memory opt-out enforcement across tool continuations — #1104

- [x] Audit every AI Chat model-prompt construction path, including the follow-up generation after a user approves a tool proposal.
- [x] Route initial chat and tool-continuation prompts through one shared system-context builder.
- [x] Enforce the saved-memory inclusion preference inside the shared builder, so a disabled preference produces no saved-memory text on either path.
- [x] Keep the existing enabled-entry, 30-entry and 12,000-character limits when inclusion is enabled; do not change stored memories or skill guidance.
- [ ] Verify Linux and Windows CI and inspect the built frontend output.
- [ ] Physically validate: disable memory inclusion, trigger a tool proposal, approve it, and confirm the continuation does not receive saved-memory context; repeat with inclusion enabled.

**Security finding and correction:** the initial chat request respected the saved-memory opt-out, but the model continuation after an approved tool call rebuilt memory context without checking that preference. This could include saved memories in a later local-model prompt even while the UI said inclusion was disabled. Both paths now use the same preference-enforcing builder. No saved memory is deleted, no new network access is introduced, and the local Ollama model remains the only configured chat inference destination. This is a privacy hardening correction; CI and physical acceptance remain pending.

The four guarantees remain mandatory: Safety, User control, Cost transparency and Reliability.


### Safe response retry workflow — #1105

- [x] Add a Retry action to the latest assistant response; load the previous user prompt back into the composer for review before sending.
- [x] Preserve the previous answer and conversation history until the user explicitly presses Retry.
- [x] Reuse the existing edit-and-resend path so a retry replaces only the selected turn and later replies after explicit submission.
- [x] Hide retry for turns that used an approved model tool, avoiding accidental repetition of actions or side effects.
- [x] Keep retry local to the currently configured Ollama model; do not introduce automatic retries, provider fallback, or additional network access.
- [ ] Verify Linux and Windows CI.
- [ ] Physically validate prompt review, cancel, retry, interrupted response, tool-used turn suppression and narrow-window layout in the Windows desktop app.

**User impact:** when an answer is incomplete or unsatisfactory, users can bring the exact last prompt back, review or change it, and explicitly request a new answer. The previous answer remains in the saved conversation until the user sends the retry. Turns that already executed an approved tool do not expose Retry, because repeating a request could repeat an external or consequential action. No background retry or extra model/provider cost occurs without the user's Send action. The four guarantees remain mandatory: Safety, User control, Cost transparency and Reliability.


### Search result excerpts — #1106

- [x] Show a bounded excerpt centred on the matching phrase inside the matching message, rather than always showing only the message beginning.
- [x] Preserve case-insensitive matching and slice the original UTF-8 text by character boundaries.
- [x] Retain the existing 180-character excerpt bound (plus optional ellipsis markers), 100-result limit, local-only search and encrypted history storage.
- [x] Add regression tests for late-message matches and Unicode text.
- [ ] Verify Linux and Windows CI.
- [ ] Physically validate search excerpts with long messages, Unicode and narrow history panels in the Windows desktop app.

**User impact:** search results now reveal the part of a long message that actually contains the search phrase, making older conversations easier to identify. Search remains local to FRIDAY's encrypted history; no message content is uploaded and no new permission or paid service is introduced. The four guarantees remain mandatory: Safety, User control, Cost transparency and Reliability.


### Export all saved conversations — #1107

- [x] Add a user-initiated export for every saved conversation, including active, archived and pinned history.
- [x] Preserve conversation IDs, titles, timestamps, pin/archive state, message roles and full message content in a versioned JSON document.
- [x] Add a dedicated backend IPC command with its own narrow `conversations.export` capability and inventory coverage.
- [x] Ask for confirmation before export and clearly warn that the downloaded JSON is unencrypted and may contain private messages.
- [x] Keep export local: no upload, cloud storage, automatic backup, history mutation or new external permission.
- [ ] Verify Linux and Windows CI.
- [ ] Physically validate the Windows save/download flow with empty history, large history and Unicode messages.

**User impact:** users can take a portable copy of all their FRIDAY conversations rather than exporting chats one at a time. Active and archived chats are included, and the existing encrypted on-device history is not changed. The exported JSON is ordinary unencrypted data; FRIDAY must not describe it as an encrypted backup or automatic restore point. The four guarantees remain mandatory: Safety, User control, Cost transparency and Reliability.


## 24. Conversation backup and restore — batch #1108

**Status:** IN PROGRESS — implementation committed; Linux/Windows CI and real Windows file-picker acceptance pending.

- [x] Add a user-initiated import action beside conversation export in Chat History.
- [x] Accept only FRIDAY JSON export format version 1; reject empty, unsupported and malformed backups.
- [x] Validate imported IDs, duplicate IDs, timestamps, titles, message roles, per-message and per-conversation sizes, and aggregate import bounds in Rust before writing.
- [x] Merge imports into existing encrypted local history; never overwrite existing conversations. Remap ID collisions so existing chats remain untouched.
- [x] Preserve imported pinned/archived state and message order.
- [x] Save through the existing encrypted store and atomic replacement path; no network transfer, AI invocation or automatic action.
- [ ] Verify Linux and Windows CI; test valid export, malformed JSON, unsupported version, duplicate IDs, size limits and 500-chat capacity.
- [ ] Physically validate Windows file selection and imported history display. CI does not prove the native file picker works on a user's machine.
- [ ] Keep imported content untrusted: it is displayed as text only and does not grant tools, permissions or execute instructions.
- [ ] Document that the JSON export itself is unencrypted and users should protect it.


## 25. Coding Assistant approved-edit verification — batches #1109–#1111

**Status:** COMPLETE IN CI — Linux tests and Windows build/tests passed in #1111. Physical Windows desktop acceptance remains outstanding.

- [x] Verify the installed Unix replacement by reopening it relative to the held parent directory handle.
- [x] Bind post-install verification to the exact temporary-file identity captured before the no-replace rename.
- [x] Read and hash installed content through the verified handle; reject type, identity, size, read and content mismatches.
- [x] On verification failure, move only the exact installed replacement to a unique failed name and restore the displaced original using handle-relative no-replace renames.
- [x] Preserve substituted entries and displaced originals when safe automatic restoration cannot be proven.
- [x] Add regression tests for content mismatch and Unix symlink substitution without reading outside target contents.
- [x] Fix the missing `std::io::Read` import in #1111; Linux tests and Windows build/tests passed.
- [x] Keep guest execution disabled; CI does not imply physical Sandbox acceptance.

## 26. Windows installed-file verification and rollback — batch #1112

**Status:** IMPLEMENTED — Linux/Windows CI pending; physical Windows acceptance not performed.

- [x] Reuse verified parent handles and exact file identities on Windows as well as Unix for installed-file verification.
- [x] Use handle-relative no-replace rename operations for Windows installation, displaced-original staging, failed-replacement preservation and rollback.
- [x] Verify installed bytes through the reopened no-follow file handle and compare both file identity and SHA-256.
- [x] Run the identity-bound verification regression on both Windows and Unix; retain a Unix-only symlink-substitution regression.
- [ ] Verify Linux tests and Windows build/tests.
- [ ] Continue Windows handle-relative backup/temp/journal/Undo creation, Undo, crash recovery and cleanup.
- [ ] Continue Unix displaced-file cleanup, Undo/recovery hardening and the documented same-user rename/unlink limitation.
- [ ] Complete physical Windows desktop acceptance; CI does not prove native desktop behavior.
- [ ] Guest execution remains disabled; no physical Sandbox acceptance is implied.


## 27. Coding Assistant transaction and Undo recovery hardening — batches #1116–#1124

**Status:** IN PROGRESS — #1116–#1122, #1125, #1127, #1128 and #1132 passed Linux/Windows CI. #1123–#1124 and #1126 required follow-up corrections; deeper path/handle hardening remains open.

- [x] #1116: Restore original Windows DACL metadata after successful Undo replacement and when recovery finds the original content already installed.
- [x] #1117–#1118: Add bounded, identity-checked reads relative to verified directory handles; route recovery-file reads through the verified parent-directory capability.
- [x] #1119–#1120: Correct Windows replacement-file access rights so replacement files can preserve/apply required attributes; #1120 passed Linux/Windows CI.
- [x] #1121: Create Undo recovery journal/temp entries through verified directory handles and verify staged original bytes.
- [x] #1122: Rework normal on-disk Undo around verified directory handles, bounded reads, identity-bound no-replace renames and post-install verification.
- [ ] #1123–#1124: Repair interrupted Undo recovery and its crash/ReplaceFileW failure-state tests. Preserve the active Undo record when recovery has not completed; archive the transaction journal without overwriting; retain only artifacts that actually exist and are needed as evidence.
- [ ] Remove remaining unsafe path-based cleanup from the Apply/Undo recovery paths. Do not replace it with a filename-only deletion fallback.
- [x] Move permission restoration to verified-handle operations; #1133 additionally binds metadata mutation to the identity captured from the original open handle. #1132–#1134 passed Linux/Windows CI.
- [ ] Define and test the recovery artifact state machine for every transaction boundary and documented Windows replacement outcome.
- [ ] Verify Linux and Windows CI and record the exact run IDs.
- [ ] Perform physical Windows desktop acceptance separately; CI is not physical-device validation.

**Known limitations:** some recovery copies are intentionally retained because the current cross-platform cleanup primitive cannot yet prove safe deletion under replacement races. Retention has no automatic expiry today. Do not describe these artifacts as automatically temporary or promise a cleanup interval.

### Handle-bound permission restoration — #1129

**Status:** COMPLETE IN CI — #1132 passed Linux tests and Windows build/tests.

- [x] Open the exact recovery child relative to the already-verified parent directory handle with no-reparse semantics before changing metadata.
- [x] Restore Unix mode bits using the opened file handle rather than path-based permission mutation.
- [x] Restore Windows DACL and inheritance protection through SetSecurityInfo on the opened file handle, not SetFileSecurityW on a path.
- [x] Request READ_CONTROL and WRITE_DAC only in the dedicated security-restoration open path; ordinary file reads retain narrower access.
- [x] Convert interrupted Undo target, interrupted Apply restoration temporary, and normal Undo prepared temporary call sites to parent-handle + child-name operations.
- [x] Verify Linux tests and Windows build/tests in #1132; Windows DACL restoration passed after requesting READ_CONTROL alongside WRITE_DAC in the dedicated security handle.
- [ ] Audit remaining path-based transaction cleanup and recovery-target validation before enabling Recovery Storage Manager cleanup.

**User impact:** FRIDAY applies saved file permissions to the file it actually opened inside the approved directory, rather than reopening a pathname that could have been swapped to point at another file. Windows DACL restoration uses the verified file handle and fails closed if required access cannot be obtained. No broader project access is granted and no recovery evidence is deleted.

### Identity-bound permission restoration — #1133–#1134

**Status:** COMPLETE IN CI — #1133–#1134 passed Linux/Windows CI.

- [x] Capture stable file identity from the original open handle before closing newly created recovery/Undo temporary files.
- [x] Require the expected identity when reopening a child for permission restoration; compare the opened handle before changing Unix mode bits or Windows DACLs.
- [x] Bind interrupted-Undo target permission restoration to a verified identity and fail closed if the opened object differs.
- [x] Add cross-platform regression coverage for identity captured from an open handle and same-name replacement detection.
- [x] Verify Linux tests and Windows build/tests in #1133–#1134.
- [x] Continue auditing path-based transaction cleanup and recovery-target validation; this audit is recorded in the next section. This batch does not authorize deletion or enable Recovery Storage Manager cleanup.

**User impact:** FRIDAY carries the original file identity across the close/reopen boundary and refuses to change permissions if the named file was replaced. Recovery evidence remains preserved.

### Handle-bound Undo state inspection — #1128

**Status:** COMPLETE IN CI — #1128 passed Linux tests and Windows build/tests.

- [x] Inspect active and consumed Undo records through the already-open verified recovery-directory handle, using bounded no-follow reads instead of path-based `symlink_metadata` followed by a separate read.
- [x] Treat missing, unsafe, malformed, duplicate active/consumed, and mismatched records as distinct fail-closed outcomes; preserve recovery evidence on uncertainty.
- [x] Compare both Unix mode and Windows DACL metadata as part of the journal/state-record match.
- [x] Route every interrupted-Undo recovery state decision through the handle-bound inspection.
- [ ] Verify Linux tests and Windows build/tests; add adversarial state-record replacement coverage.
- [ ] Continue converting permission restoration, remaining path-based recovery helpers and transaction cleanup to verified-handle operations.

**User impact:** interrupted Undo recovery no longer decides whether an operation is active or consumed by checking a pathname and then reopening that pathname separately. Both state records are read from the same verified recovery folder, reducing the chance that a swapped file changes the recovery decision. No recovery evidence is deleted.



### Identity-bound recovery cleanup primitive and failure-path preservation — #1137–#1141

**Status:** #1137–#1139 introduced the Windows identity-bound recovery-file deletion gate and adversarial identity/hard-link tests, but all three CI runs failed because two existing Windows call sites had not been updated for the helper's new fifth argument. #1140 fixed those call sites and passed both Windows and Linux CI (commit `f39259e11c9867a7e0a2abe0efafeb22fcb46417`, run [44904582789](https://github.com/Friday-AI-Personal-Assistant/FRIDAY/actions/runs/44904582789)). #1141 is the next preservation-hardening increment; its CI result is pending.

- [x] Add a recovery-specific deletion wrapper requiring the expected file identity; Windows checks the opened handle identity, regular-file state, single-link condition, bounded size and SHA-256 before applying handle-bound delete disposition.
- [x] Keep Unix recovery deletion fail-closed; no name-based unlink fallback was introduced.
- [x] Add Windows regression coverage for mismatched identity, successful identity-bound deletion and hard-link rejection; add Unix coverage proving unsupported cleanup preserves the file.
- [x] Fix the two stale Windows helper call sites in #1140; both supported CI jobs passed.
- [ ] #1141 removes pathname-based deletion from unverified Apply/Undo temporary failure paths and disables the unused path-based recovery-copy deletion helper. Partially written or hash-mismatched artifacts are preserved for inspection; verify Linux and Windows CI before marking complete.
- [ ] Do not yet wire deletion into Apply/Undo journal or displaced-copy lifecycle. These remain recovery evidence until transaction ownership, retention eligibility and exact original-handle identity are carried through the complete state machine.
- [ ] Next: add lifecycle-bound inventory/identity records for eligible transient artifacts, then integrate Windows-only identity-bound cleanup behind state and ownership checks. Unix remains preserve-only until a reviewed race-safe primitive exists.
- [ ] Add/retain adversarial coverage for locked files, reparse/symlink substitution, parent replacement, same-name substitution and partial cleanup before any production cleanup call site is enabled.

**User impact:** FRIDAY now has a tested Windows-only deletion primitive, but it is not yet authorized to delete Coding Assistant recovery evidence. Unverified or partially written temporary files are retained rather than removed by pathname. This intentionally favors recoverability over disk-space reclamation.

### Recovery cleanup and target-validation audit — #1136

**Status:** AUDIT COMPLETE — unsafe cleanup call sites identified; safe deletion implementation remains blocked pending platform-specific guarantees and adversarial tests.

**Confirmed findings in `src-tauri/src/coding_assistant.rs`:**

- [ ] Apply still uses pathname-based `fs::remove_file` for the displaced original copy and resolved transaction journal after successful installation. The displaced copy is valuable recovery evidence and should not be deleted merely because the Undo record was written.
- [ ] Normal Undo still removes its resolved journal by pathname after marking Undo consumed; the displaced edited copy is intentionally retained.
- [ ] Apply/Undo error paths attempt pathname-based removal of newly-created temporary files. These paths must either preserve the artifact or use a deletion operation bound to the exact identity captured from the original open handle.
- [ ] `remove_recovery_copy` performs a separate path-based hash/read and then pathname deletion. It currently has no production call sites, but must not become a cleanup fallback.
- [x] `safe_recovery_target` validates the record, granted root, allowlisted file type and parent path; recovery flows additionally open verified parent handles and use no-follow, identity-checked child operations. Continue auditing the boundary between path validation and the later handle-bound operation.
- [x] Confirmed `coding_sandbox_fs::remove_verified_runtime_file` is intentionally unsupported on Unix because unlink-by-name retains a same-user replacement race. Windows has a handle-bound delete-disposition implementation, but it must be extended/reviewed for recovery-specific expected identity, ownership and transaction-state checks before reuse.

**Engineering decision:**

- Do not wire the snapshot prototype's Unix `unlinkat` helper into recovery cleanup; its own documentation explicitly says the final name-based unlink race remains.
- Do not use a path-based delete after a successful hash check, a filename-only check, or a fresh identity lookup that is not bound to the deletion handle.
- Preserve resolved journals and displaced copies until the platform can prove safe cleanup. Prefer a verified no-replace archive/retention state over deletion when that is possible.
- Implement and test the Windows identity-bound delete path independently. Keep Unix cleanup fail-closed unless a reviewed primitive and threat model can close the same-user race; preserving storage is preferable to unsafe reclamation.
- The Recovery Storage Manager remains read-only/informational until cleanup eligibility, exact artifact identity, transaction ownership and state-machine gates are enforced by the backend.

**User impact:** FRIDAY will not reclaim disk space by risking a user's project file or the only copy needed to recover an edit. Some artifacts may remain on disk until a platform-safe cleanup implementation is available; this is an intentional reliability trade-off, not an automatic retention policy.


## 28. Recovery Storage Manager — accepted product requirement

**Status:** PLANNED — product direction accepted; implementation must begin only after the underlying transaction recovery and safe-cleanup primitives are verified.

**Purpose:** Give users clear visibility and safe control over FRIDAY-managed Coding Assistant recovery storage, without risking project files or deleting evidence needed to restore work.

### User-facing capabilities

- [ ] Add a Recovery Storage view showing total storage used, item count, and bounded per-item details.
- [ ] Show each artifact's type, owning transaction, project association, creation time when trustworthy, size, current recovery state, and why FRIDAY is retaining it.
- [ ] Distinguish active transaction data, recoverable backups, completed-transaction evidence, archived journals, and uncertain/corrupt artifacts.
- [ ] Show the exact storage location and explain whether an item is inside a project recovery folder or FRIDAY-managed application storage.
- [ ] Provide a user-initiated inspect/reveal-location action without exposing file contents unnecessarily.
- [ ] Provide manual cleanup only for artifacts the backend independently proves are eligible; explain what will be removed before confirmation.
- [ ] Provide configurable retention preferences for eligible completed evidence. Proposed default: retain successfully recovered evidence for 7 days; this is a future policy, not current behavior.
- [ ] Show when cleanup is blocked and explain that unfinished, active, mismatched, corrupted, or uncertain transaction evidence is preserved.
- [ ] Report cleanup results and reclaimed space honestly; never claim success when deletion could not be verified.

### Safety and reliability requirements

- [ ] Build one cross-platform safe-cleanup primitive using verified directory handles and exact file identity, with platform-specific implementations and adversarial race tests.
- [ ] Never delete by path/name alone after a separate pre-check; prevent symlink/reparse-point redirection, hard-link surprises, parent-directory substitution and replacement races.
- [ ] Revalidate transaction state, artifact ownership, expected identity, type, size and (where applicable) SHA-256 immediately before cleanup.
- [ ] Never automatically delete an artifact belonging to an unfinished transaction or an uncertain/corrupted recovery state.
- [ ] Keep backup restoration possible until the user-visible retention policy and transaction lifecycle explicitly allow cleanup.
- [ ] Make cleanup idempotent, bounded, auditable and cancellable where practical; handle locked files and permission failures without weakening safety.
- [ ] Keep project recovery data local; no upload, telemetry of file contents, cloud storage or paid service.
- [ ] Keep cleanup separate from install/enable permissions and never silently broaden the Coding Assistant's project access.
- [ ] Test Windows and Linux, including identity substitution, symlink/reparse-point, hard-link, parent replacement, locked-file and partial-cleanup scenarios.
- [ ] Document exact retention semantics, disk-space accounting, uninstall behavior and user recovery options before release.

**Retention policy proposal (not implemented):** unfinished transactions are never auto-deleted; successfully recovered evidence is retained for 7 days by default; user-pinned/user-requested recovery is retained until the user removes it; uncertain or corrupted artifacts are preserved until resolved. A timer alone is never sufficient authorization to delete an artifact.

**Release gate:** the manager remains informational/read-only until the safe-cleanup primitive and state machine pass Linux/Windows adversarial tests. Manual and automatic deletion must both use the same backend safety gate. The FRIDAY Four Guarantees apply: Safety, User control, Cost transparency and Reliability.


## 29. OpenJarvis skill-system selective reuse — audit and adoption decision

**Status:** SOURCE AUDIT COMPLETE; STRICT IMPORTER, HANDLE-BOUND QUARANTINE INVENTORY, IDENTITY HARDENING, PROVENANCE/TRUST METADATA AND SKILLS STORE REVIEW UI PASSED LINUX/WINDOWS CI THROUGH #1175. #1173/#1174 exposed a frontend JSX closing-brace error; #1175 corrected it and passed both jobs. No upstream runtime code has been copied or enabled.

- [x] Verify current FRIDAY branch/HEAD and latest Linux/Windows CI: `foundation/evaluation`, `82cef5bae69a70a9c02dcd4d9361b1371ca0d53c`, #1175 run [44922923873](https://github.com/Friday-AI-Personal-Assistant/FRIDAY/actions/runs/44922923873); Linux tests and Windows build/tests passed.
- [x] Pin and inspect current OpenJarvis `main`: `5a9b5268f2432f7ea4fe402e384ecb6b64a649bb`.
- [x] Compare `skills/loader.py`, `skills/types.py`, `skills/manager.py`, `skills/executor.py`, `skills/tool_adapter.py`, `tools/_stubs.py` and `workflow/engine.py` with FRIDAY's Rust skills, persistent permissions, backend command policy and existing Skills Store UI.
- [x] Record the feature-level reuse decisions, upstream limitations, licence obligations, FRIDAY package contract, tool-execution boundary and staged acceptance criteria in [OPENJARVIS-SKILL-SYSTEM-REUSE-AUDIT.md](OPENJARVIS-SKILL-SYSTEM-REUSE-AUDIT.md).
- [x] Decision: reuse the compatible `SKILL.md` package convention and the upstream separation of discovery, manifest, manager, adapter, executor and event concepts. Keep FRIDAY's Tauri/React shell and Rust backend as the sole authority for permissions and actual tool execution.
- [x] Decision: do not port OpenJarvis's permissive markdown fallback, optional signature/scanner behavior, arbitrary argument-template execution, Python worker pool or general synchronous workflow engine into the first FRIDAY implementation.
- [x] Implement the first strict, bounded, instructions-only `SKILL.md` parser and local quarantine command in Rust. It accepts exactly one `SKILL.md`, bounds content/frontmatter, rejects unsupported metadata and extra package files, and never installs, enables, loads, executes or grants access. Linux/Windows CI passed in #1154; filesystem race hardening, provenance collection and physical Windows acceptance remain open.
- [x] Add handle-bound quarantine reinspection for an explicitly selected content-addressed item: read `SKILL.md` and `inspection.json` through a pinned directory handle and no-follow child opens; enforce size/type/read-stability bounds; recompute SHA-256; compare parsed metadata; and reject altered enabled/executable/permission state. Add regression coverage for content tampering and metadata tampering. Expose the backend command; this is integrity verification, not publisher authentication.
- [x] Add a bounded backend quarantine inventory (maximum 200 entries) that reports verified items and explicitly labels invalid, tampered or unreadable entries as blocked; it never silently drops uncertain entries. Frontend inventory presentation and trust review are now being wired.
- [x] Harden community-skill source inspection and quarantine inventory to enumerate through pinned directory handles, retain each entry's filesystem identity, reject reparse/symlink entries, and re-open each candidate relative to the held quarantine root before reading its bounded child files. Add Linux/Windows regression coverage for directory identity mismatch and symlinked entries; #1171 passed.
- [x] Extend provenance metadata with source type, import time, optional source URL/publisher/revision fields, self-declared licence and explicit trust findings. Current picker records only the user-selected local-folder source and local import time; publisher/source authentication is not implemented. Legacy metadata remains readable and is labelled unknown; CI passed in #1172 and the integrated frontend correction passed in #1175.
- [x] Add parser regression tests for duplicate/unknown fields, unsupported nested YAML/anchors, invalid IDs, missing delimiters, empty body and oversized content. [ ] Add adversarial filesystem tests for path traversal, symlinks/reparse points, unsupported extra files/scripts and quarantine tampering; physical Windows acceptance remains outstanding.
- [x] Add a Skills Store import action and unverified quarantine preview. No install transition exists yet; inspected content remains inert and is not added to session context. [ ] Build trust review and explicit install-disabled transition after source/provenance hardening.
- [ ] After importer acceptance, build a Rust-native typed tool registry and permission-checked skill-to-tool bridge. Authorize every invocation against backend-owned tool policy and current exact user grants; unknown tools fail closed.
- [ ] Connect one bounded first-party skill/tool end-to-end, then prioritize Excel Intelligence on that shared foundation.
- [ ] Keep executable third-party extensions blocked until OS-enforced process isolation, resource limits, cancellation and adversarial physical-platform acceptance are independently proven.

**Key distinction:** OpenJarvis provides reusable architecture and package-format conventions, not a ready-made FRIDAY security boundary. Its source-level review does not establish that imported packages are benign. FRIDAY's Safety, User control, Cost transparency and Reliability guarantees remain mandatory.

**Licence:** OpenJarvis's inspected repository-root licence is Apache-2.0. If any upstream source is copied/adapted, retain required licence/copyright/NOTICE material, mark modified files and record exact source commit/file provenance. The first importer should be a clean Rust implementation unless copying code has a clear measured advantage.


## 30. FRIDAY development personality and visible growth journey — accepted product requirement

**Status:** INITIAL OVERVIEW JOURNEY AND FIRST PERSONALITY/MILESTONE UI IMPLEMENTED; #1180–#1182 and personality/milestone batch #1183–#1186 passed Linux and Windows. The first Excel Intelligence read-only workspace is now implemented and awaiting its own CI. The first Midnight Azure workspace integration batch passed #1179 (run `44924968988`); installed-app visual acceptance remains outstanding. This is a user-facing product experience, not a claim that FRIDAY is conscious or independently self-aware.

**Purpose:** Make FRIDAY's development feel visible, understandable and motivating. The user should be able to open the application and see what FRIDAY can genuinely do today, what is being built next, and which milestones have actually been completed. Development should feel like watching a useful assistant grow rather than watching invisible engineering activity.

### Personality and experience requirements

- [x] Add a concise FRIDAY voice treatment to Overview: useful, honest, careful, and clear about what is ready versus still being built. Keep the tone natural and avoid forced jokes, excessive flattery or manipulative emotional language. [Current UI batch; verify CI.]
- [ ] Create a clear "FRIDAY Development Journey" or equivalent home/dashboard experience showing current focus, completed milestones and the next planned capability.
- [ ] Show visible milestone unlocks only when the corresponding feature is implemented and verified; distinguish **Available**, **In progress**, **Planned** and **Needs verification**.
- [x] Add a dismissible milestone celebration when a roadmap milestone transitions from a previously recorded non-available status to `Available`. Persist the local status snapshot before displaying the message so a reload does not replay it; establish a silent baseline on first launch or when stored state is unreadable. If local persistence fails, suppress the celebration rather than repeat or invent one. Add subtle motion only when reduced motion is not requested. [Current UI batch; verify CI.]
- [ ] Explain technical progress in plain language and show what the user can now try, rather than exposing commit/test jargon as the primary experience.
- [ ] Keep the journey tied to the Master Roadmap so status survives conversation loss and does not depend on an assistant's memory or an unverified estimate.
- [ ] Let the user explore completed capabilities and launch the relevant feature directly from the journey.
- [ ] Keep progress honest: CI success alone does not mean a user-facing feature is complete; require working integration and applicable platform/device acceptance.
- [ ] Do not fabricate progress percentages, background work, autonomous development, awareness, emotions or capabilities. FRIDAY must clearly identify planned or simulated content.
- [ ] Personality and milestone celebration must never override permissions, safety checks, cost disclosure, user consent or the FRIDAY Four Guarantees.
- [ ] Keep the experience lightweight, accessible, responsive and consistent with the official Midnight Azure design system and companion light theme.

- [x] Add the initial roadmap-backed Development Journey to Overview: AI Chat (Available), Coding Assistant (In progress), Skills Store and Midnight Azure (Needs verification), Excel Intelligence (In progress) and Recovery Storage Manager (Planned). Keep the journey explicit that CI success is not equivalent to full user-facing acceptance; #1180–#1182 passed Linux/Windows (user-confirmed).

### Initial delivery sequence

1. [x] Complete the first Midnight Azure visual-refinement and integration pass across existing workspaces; #1179 passed Linux and Windows. Installed-app and narrow-window visual acceptance remains open.
2. [x] Add the initial FRIDAY Development Journey to Overview with explicit, truthful milestone statuses and navigation to available workspaces. #1180–#1182 passed Linux/Windows (user-confirmed).
3. [x] Add FRIDAY's concise welcome/assistant personality treatment and small verified milestone celebrations. Celebration is gated on a persisted status transition to Available, shown once per transition, dismissible, and motion-aware. Verify this implementation in CI before marking this step accepted.
4. [In progress] Make Excel Intelligence the first prominent capability milestone. The first local read-only XLSX/CSV analysis workspace is now implemented; continue with workbook-quality insights, report/chart output, and connect the first usable Excel Analyst skill through the permissioned skill/tool bridge.
5. Validate the experience in the installed Windows application at normal and narrow window sizes; record screenshots/acceptance evidence before marking visual acceptance complete.

**Product principle:** The user should regularly have something visible, testable or enjoyable to interact with. Avoid long stretches of invisible foundation-only work unless a concrete safety or reliability blocker requires it.

**Excel first-slice boundary:** current support is `.xlsx` and `.csv` read-only analysis under strict input, ZIP package, worksheet, row, column and cell bounds. The preview treats row one as headers and summarizes numeric values from cached cells. It does not write files, preserve or edit workbook formatting, generate charts, interpret named ranges/tables, or provide a full natural-language analyst skill. Legacy `.xls` remains unsupported.\n\n**Milestone celebration boundary:** the UI only observes the app's versioned local roadmap-status snapshot. It does not inspect GitHub, run CI, perform background development, or independently verify feature quality. The developer must update a milestone to `Available` only after its implementation and applicable acceptance checks are complete. A clean first-run baseline is intentionally silent; no historical milestone is celebrated as if it had just shipped.


## 31. Excel Data Quality Intelligence — first analyst-quality batch

**Status:** DONE — CI VERIFIED (Linux and Windows). The initial implementation run #1201 failed; the corrected backend and integrated frontend/CSS/roadmap runs #1202, #1203, #1204 and #1205 passed. The final combined state is represented by #1205. This completes the bounded data-quality review slice, not the full Excel Analyst skill.

- [x] Detect blank column headers and duplicate headers (case-insensitive after trimming).
- [x] Flag columns containing mixed inferred value types.
- [x] Identify repeated non-empty rows using bounded, normalized row comparisons.
- [x] Suggest potential numeric outliers using the 1.5× IQR rule when enough numeric values exist; findings remain review hints, not confirmed errors.
- [x] Bound the number of findings and sample row references returned to the UI.
- [x] Display severity, category, explanation, occurrence count and sample row references in the Excel workspace.
- [x] Keep all analysis local and read-only; never modify, delete, recalculate or upload workbook data.
- [x] Verify the corrected implementation through Linux and Windows CI in #1202–#1205.

**Next batch:** Natural-language Excel analysis over the currently selected, bounded workbook/sheet context. Prefer deterministic local query handling for supported aggregates, rankings and simple comparisons; show the evidence/columns used, and ask for clarification or explain unsupported questions rather than guessing. Do not send full workbook contents to a cloud model. Keep the workflow read-only and disclose any future provider/network use before it can occur.


## 32. Natural-language Excel question answering — bounded local first release

**Status:** DONE — CI VERIFIED (Linux and Windows). The final integrated implementation passed in #1211, #1212, #1213, #1214 and #1215 (user-confirmed). Earlier iterations #1207 and #1210 failed and were superseded by the corrected commits. This is a deterministic, local question layer over the selected worksheet's already-computed bounded summaries. It is not a general-purpose AI analyst and does not send workbook content to a model or network service.

- [x] Add bounded per-column frequency summaries (top five values) for columns with at most 512 distinct values; skip frequency answers when the distinct-value cap is exceeded.
- [x] Add a deterministic question interpreter for row counts, totals, averages, highest/lowest numeric values, missing-value counts, non-empty counts and most-common values.
- [x] Match explicit column names and request clarification when a column cannot be identified unambiguously.
- [x] Return the answer, interpretation, supporting column evidence and method to the UI.
- [x] Reject oversized questions and oversized worksheet-summary inputs; keep question handling local and bounded.
- [x] Register a dedicated backend IPC command with a separate deny-by-default capability policy.
- [x] Add regression tests for supported calculations, most-common values, missing-value questions, ambiguity and unsupported grouped questions.
- [x] Add a question panel to the Excel workspace; clearly label the feature local/read-only and explain the supported question types.
- [x] Verify the final integrated implementation through Linux and Windows CI in #1211–#1215 (user-confirmed); failed earlier iterations #1207 and #1210 were superseded.
- [ ] Expand to grouped comparisons (e.g. revenue by product), ranking, trends and time-series analysis only after a bounded row-level query engine and stronger tests are in place.

**Known limitations:** The first release answers from computed worksheet summaries, not arbitrary raw-cell reasoning. It does not yet support grouped aggregations, top-selling products by a category, trends, date interpretation, formulas, charts or generated reports. Unsupported or ambiguous questions must be reported honestly rather than guessed. Frequency answers are withheld if the distinct-value cap is exceeded.

**Privacy boundary:** no cloud LLM, external API, network request or workbook upload is involved. The frontend submits only the already-returned bounded column summaries to a dedicated local Rust command. No workbook writes are performed.


## 33. FRIDAY Domain Intelligence & Formula Engine — reusable calculation foundation

**Phase 1 verification (2026-11-21):** User-confirmed and independently checked: workflow runs #1217–#1220 completed successfully on Linux and Windows. The backend-only fixed-point formula core and reviewed initial accounting formulas are compile/test verified. It is not yet exposed through IPC or a user-facing accounting calculation workflow.

**Phase 2 implementation in progress (2026-11-21):** Commits `d28fa9449a802ba4ab11afde592624a579f9a98d`, `3b117fda977b42e021bc37beb0185632f1388e25` and `0439136eff618877fbb58081c5fa8cd08b301f7d` add deterministic accounting-header suggestions to Excel Intelligence, explicit ambiguous/unmapped states, and worksheet cell/range references for each suggestion. Duplicate headers are forced to review; generic labels such as “Revenue” or “Sales” are not silently assigned to one concept. The UI labels these as suggestions only. **This is not yet backend-verifiable source-session provenance and must not be used as authority for financial calculations. CI verification pending.**


**Implementation progress (2026-11-21):** Phase 1 backend core has been added on `foundation/evaluation` in commits `f25cc7d8bcfde9b2c7716d2bd3835855482188f1`, `e73c4fa10db0a4e68ec1c43dceed44e54c536455` and `468913bbe57e2d4f6ebff5e133eab0cb59eb51b6`. It introduces a deterministic fixed-point decimal core (six fractional places, checked i128 operations, no floating-point arithmetic), a versioned allowlisted formula registry, explicit input states (provided/missing/unknown/not applicable), required-field and currency checks, and traceable outputs. The first reviewed formula definitions are net sales, gross profit and gross margin. This is backend-only and not yet exposed to the UI or IPC. **CI verification is pending; do not treat this phase as complete until Linux and Windows checks pass.**


**Status:** PLANNED — accepted product direction. This is the next major engineering batch after the verified bounded Excel question-answering slice. It is a reusable, deterministic calculation and domain-interpretation foundation, not an LLM-generated formula feature. **Accounting Intelligence is the first specialist domain.** No financial action, workbook write, tax filing, payment, journal posting or ledger mutation is included in this milestone.

### 33.1 Product and safety contract

- [ ] Build a deterministic formula/calculation core with typed inputs, explicit units, decimal-safe arithmetic where financial precision is required, bounded execution and structured results.
- [ ] Separate formula definitions from execution. Every formula must have a stable ID, domain, version, description, required inputs, output type/unit, precision/rounding policy, validation rules, provenance and applicable limitations.
- [ ] Never infer a financial formula solely from a natural-language prompt. Resolve a request to a reviewed formula definition or ask for clarification.
- [ ] Never silently assume currency, locale, tax jurisdiction, accounting basis, reporting period, sign convention, inclusive/exclusive tax treatment or rounding policy.
- [ ] Preserve missing, unknown, zero and not-applicable as distinct states; never silently convert missing data to zero.
- [ ] Reject incompatible units/currencies and invalid or non-finite values. Currency conversion requires an explicit, dated, sourced exchange-rate input and user awareness; no implicit live-rate lookup.
- [ ] Use decimal/fixed-point arithmetic for money and define rounding at the formula/reporting boundary. Do not use binary floating-point for authoritative monetary totals.
- [ ] Return a traceable calculation record: formula ID/version, inputs and source fields, normalized values, assumptions, intermediate steps, output unit/currency, precision/rounding and validation warnings.
- [ ] Keep the engine pure and read-only: no workbook edits, ledger writes, file writes, network access, model calls or external actions.
- [ ] Enforce resource bounds on input count, expression depth, row counts, precision/scale and execution time. No arbitrary code, eval, user-supplied executable formulas or unrestricted expression parser.
- [ ] Add property-based or invariant-focused tests where practical, plus known-answer, boundary, malformed-input, adversarial and cross-platform regression tests.
- [ ] Keep all feature claims limited to implemented and verified formula coverage; unknown or ambiguous requests must fail closed with an actionable clarification.

### 33.2 Core architecture

- [ ] Define typed domain-neutral primitives: Decimal/Money, Quantity, Unit, Rate, Percentage, Date/Period, Missing/Unknown and CalculationResult.
- [ ] Define a versioned Formula Registry with reviewed formula metadata and explicit input/output contracts.
- [ ] Define a Semantic Field Dictionary that maps user-facing labels and aliases to canonical concepts only when evidence is sufficient; expose ambiguous mappings for user confirmation.
- [ ] Define a deterministic validation layer for required fields, sign conventions, units, currency, period alignment, duplicate/missing inputs and accounting invariants.
- [ ] Define an explanation/provenance layer that renders the actual formula and calculation trace without inventing reasoning.
- [ ] Keep domain packs modular: shared engine first, accounting pack second, later inventory, payroll, tax and other domains only after their own requirements and jurisdictional boundaries are reviewed.
- [ ] Keep AI as an intent/clarification interface only. The model may help identify a requested concept, but the trusted registry and calculation engine determine whether and how a calculation executes.

### 33.3 Accounting Intelligence — first specialist domain

- [ ] Establish a reviewed accounting glossary and canonical field map: gross sales, discounts, returns, net sales, COGS, gross profit, operating expenses, operating income, net income, assets, liabilities, equity, receivables, payables, inventory, cash and cash flow.
- [ ] Implement a small, explicitly scoped first formula set with documented definitions and assumptions, starting with net sales, gross profit, gross margin, accounting equation, debit/credit balance checks and receivables/payables totals.
- [ ] Distinguish gross profit, operating profit, net profit, cash balance and cash flow; never treat them as interchangeable.
- [ ] Distinguish cash versus accrual basis, transaction date versus posting date, and opening/closing balances versus period movements.
- [ ] Validate accounting equation and debit/credit invariants only when the required complete inputs and accounting context are supplied; a balanced ledger is not proof that entries are correct.
- [ ] Make tax calculations a separately gated future domain: no tax rate, filing rule, tax treatment or jurisdiction-specific advice is assumed in this first accounting slice.
- [ ] Require explicit currency and reporting period for financial statements and comparisons; report when source data is insufficient to calculate a requested metric.
- [ ] Show formula, source columns/accounts, values, assumptions, warnings and result together; let the user inspect the source evidence.

### 33.4 Integration sequence and release gates

1. [ ] Audit current Excel types, numeric handling and IPC boundaries; document precision and provenance gaps before choosing implementation primitives.
2. [ ] Implement and test the domain-neutral typed calculation core with no UI dependency.
3. [ ] Add formula registry, validation and calculation-trace contracts.
4. [ ] Add the first reviewed accounting glossary and limited formula pack.
5. [ ] Integrate read-only calculations into Excel Intelligence using explicit column/account mapping and clarification for ambiguity.
6. [ ] Add adversarial tests for missing/duplicate fields, wrong units, mixed currencies, negative values, rounding, date-period mismatch, huge inputs and misleading natural-language prompts.
7. [ ] Verify Linux and Windows CI and update this checklist with commit/run evidence. Complete physical-device review where UI acceptance is involved.
8. [ ] Keep journal posting, ledger edits, financial transactions, tax filing, payroll submission and automatic workbook modification out of scope until separate permission, approval, audit and recovery designs pass review.

**Explicit non-goals:** no autonomous accounting advice, no invented accounting policy, no silent correction of source records, no automatic tax/legal conclusions, no money movement, no posting entries, no external accounting-platform connection and no cloud transmission of financial data in this milestone.

**Dependency note:** The existing Excel question-answering implementation calculates only from bounded worksheet summaries supplied by the frontend. Before any accounting result is represented as authoritative, the integration must establish stronger backend-verifiable source provenance and an explicit mapping from source cells/columns to accounting concepts. The current summary-only Q&A path is not sufficient evidence for ledger-grade accounting.

### 33.5 Workbook Source Provenance & Verified Sessions

**Status:** IN PROGRESS — backend-held source sessions, confirmed accounting mappings, source-bound formula execution and a read-only calculation UI are implemented on `foundation/evaluation`; final Windows installer CI and remaining adversarial hardening are still open.

- [x] Compute a SHA-256 fingerprint and exact byte length from the bytes FRIDAY actually reads for each accepted XLSX/CSV import, and return them as workbook source metadata.
- [x] Add regression assertions that CSV and XLSX analysis metadata matches the exact imported bytes.
- [x] Treat the fingerprint only as a content identifier: it does not prove who authored the file, that the source data is truthful, or that the file has not changed outside FRIDAY.
- [x] Introduce bounded, process-local backend workbook sessions with opaque session identifiers, explicit expiry/eviction, and source metadata bound to the exact parsed workbook instance (maximum 8 sessions, 30-minute TTL, bounded serialized source/summary budgets).
- [x] Keep authoritative worksheet summaries and retained source cells in the backend session; question and accounting operations accept a session identifier and bounded selectors rather than trusting frontend-supplied summaries.
- [ ] Revalidate session existence, expiry, worksheet identity, dimensions and source fingerprint for every operation; current operations validate session and worksheet identity, while calculation additionally validates the mapping fingerprint and retained header. Add the remaining source/session mismatch adversarial coverage before closing this item.
- [x] Preserve absolute source cell/range coordinates through confirmed accounting mappings and calculation traces, including worksheets whose used range does not begin at A1.
- [x] Keep retained source cells process-local and memory-bounded; do not persist workbook contents or reopen/modify the source file from the analysis session.
- [ ] Add the remaining adversarial tests for source mismatch, stale frontend summaries, duplicate headers, and bounded concurrent source-session memory. Forged IDs, unknown worksheets, expiry/eviction, mapping ambiguity and duplicate confirmation already have regression coverage.
- [ ] Verify the complete Linux and Windows CI workflows before treating this integration as CI-complete. Latest code run #1266 has passed Linux tests and Windows Rust tests; Windows installer build/artifact steps are still running.

**Provenance boundary:** A SHA-256 fingerprint establishes byte-level identity for the imported content. It is not a digital signature, source-authenticity guarantee, accounting validation, or audit opinion. FRIDAY must not describe fingerprinted data as verified or authoritative. Session eviction drops in-memory source objects; secure memory zeroization is not currently claimed.

### 33.5.1 Source-bound Accounting Calculation (Initial Read-only Integration)

**Status:** IMPLEMENTED — CI VERIFICATION IN PROGRESS.

- [x] Retain parsed CSV/XLSX source cells privately inside the backend workbook session; the private source-cell structure is skipped from the serialized workbook-analysis response.
- [x] Expose a permission-gated calculation command that accepts only session ID, worksheet, registered formula ID and explicit three-letter currency code. It does not accept frontend-provided values, summaries or cell contents.
- [x] Require every formula input to have an explicitly confirmed accounting mapping in the same workbook session and worksheet; re-check source fingerprint and source header before reading.
- [x] Aggregate confirmed source-column values with FRIDAY's fixed-point decimal engine and registered formulas: net sales, gross profit and gross margin. No floating-point formula arithmetic or implicit currency conversion.
- [x] Reject non-numeric source values; skip blank cells only with an explicit warning. Retain contributing source-cell coordinates and values, mapping provenance, source fingerprint, formula ID/version and calculation trace.
- [x] Preserve absolute worksheet coordinates for non-A1 used ranges and cap returned source-cell evidence at 20,000 references.
- [x] Add a read-only UI for formula selection, explicit currency entry, confirmed-mapping prerequisites, results, warnings and source evidence.
- [ ] Add explicit user-selected row-range controls and stronger subtotal/grand-total detection. Current initial integration aggregates every nonblank numeric cell in each confirmed column; the UI warns users to inspect ranges for subtotal or grand-total rows before relying on a result.
- [ ] Add adversarial coverage for source mismatch, oversized source evidence, unusual numeric formats and workbook formula cached-value caveats.
- [ ] Complete Linux and Windows CI, including Windows installer build and artifact upload.

**Safety boundary:** This integration is read-only. It does not modify workbook cells, save a workbook, execute workbook formulas, recalculate cached formulas, or create a backup. It is not an audit opinion or a guarantee that source data is correct. CSV/XLSX source values are interpreted as provided; formula cells in XLSX use cached values and may differ from a workbook recalculated by Excel. Currency is explicitly supplied by the user and is never inferred or converted.


### 33.6 Permissioned Excel Editing, Verified Backups & Recovery

**Status:** PLANNED — accepted product requirement (2026-11-22). FRIDAY must eventually support authorized edits to Excel workbooks, including natural-language requests that identify source ranges, matching records, calculation fields and destination cells. This is a separate write capability; the existing Excel Intelligence and formula-engine work remains read-only.

- [ ] Interpret bounded natural-language edit requests such as “search D22:M55 for Adnan Traders, sum the correct amount field, and place the result in J10”; resolve worksheet, range, match column, amount column, currency, aggregation and destination explicitly.
- [ ] Treat cell references and ranges as untrusted selectors: validate worksheet identity, bounds, merged cells, hidden rows/columns, filters, formulas, protected cells/sheets, tables, named ranges and destination-cell suitability before proposing a change.
- [ ] Never guess which numeric column to total when multiple plausible amount fields exist; ask for clarification and show the selected source rows/values before proceeding.
- [ ] Keep Analysis/Preview mode read-only. Editing mode must be a distinct, permissioned capability with a narrowly scoped approval for the exact workbook, worksheet, source range, operation and destination.
- [ ] Before every approved modification, create a separate, recoverable backup of the original workbook. Verify backup completion and byte-level identity against the exact original source before allowing the write; if backup creation or verification fails, abort without modifying the source.
- [ ] Preserve the original file and its workbook features as far as the selected library/format safely supports; detect unsupported or lossy formats/features (including macros, formulas, external links, drawings, pivots and workbook protection) and refuse or require an explicit safe alternative rather than silently dropping content.
- [ ] Prepare changes in a bounded temporary output, validate the result, and use an atomic/recoverable replacement strategy where supported. Never partially overwrite the original during calculation or serialization.
- [ ] Show a change preview containing original and proposed destination values, exact source rows/cells, matched-record count, formula/method, currency, warnings and backup location; require explicit user approval immediately before applying.
- [ ] After saving, reopen/re-read the output and verify the intended destination value, workbook readability, expected sheet structure and preservation invariants. Report success only after verification.
- [ ] Maintain a privacy-safe change journal with operation ID, source fingerprint, backup fingerprint/location, target identity, source references, old/new values, calculation/formula version, approval and verification outcome. Do not log unrelated workbook contents.
- [ ] Provide a user-controlled recovery workflow. Before restoring, verify the backup identity and inspect whether the current workbook has changed since the failed edit; never silently overwrite newer user work. Prefer restoring to a new file or require explicit confirmation for replacement.
- [ ] Make operations idempotent or detect duplicate/retried requests; handle cancellation, process crash, disk-full, permission loss, concurrent modification and interrupted replacement without claiming success.
- [ ] Keep backups under a bounded, user-visible retention/storage policy with explicit cleanup controls; never silently delete the only recoverable copy. Integrate with Recovery Storage Manager only after its identity-bound cleanup safety gates pass.
- [ ] Add adversarial tests for backup failure/tampering, destination substitution, source changes between preview and apply, duplicate matches, ambiguous amount columns, formula cells, protected sheets, macro-enabled workbooks, serialization loss, disk-full, crash recovery, concurrent edits, rollback and backup retention.
- [ ] Verify Linux and Windows CI and perform real-device acceptance for supported Excel formats before enabling workbook modification for users.

**Non-negotiable rule:** No verified backup, no write. No explicit approval for the exact proposed change, no write. No post-save verification, no success claim. Recovery must protect newer user changes and must never silently replace a workbook with a stale backup.

**Initial scope:** Start with a narrowly supported XLSX/CSV workflow and clearly disclose format limitations. CSV has no multi-sheet workbook structure or Excel formulas/styles; macro-enabled and feature-rich XLSX files must remain unsupported for modification until preservation is demonstrated. Keep the existing read-only analysis path available independently.


### 33.7 FRIDAY Smart Undo & Conversational Recovery

**Status:** PLANNED — accepted product requirement (2026-11-22). Smart Undo extends permissioned Excel editing and the broader FRIDAY recovery experience. It allows users to request reversal in natural language while preserving strict transaction, provenance and user-approval boundaries.

- [ ] Understand natural-language reversal requests such as “I don't need this change; put the sheet back the way it was” and resolve them against FRIDAY's recorded operation history.
- [ ] Bind every reversible operation to a unique operation ID, exact workbook/source fingerprint, target workbook identity, worksheet and affected cell/range references, before/after values or formulas, and verified save outcome.
- [ ] Resolve vague requests such as “reverse it” to a clearly identified candidate operation; show the workbook, sheet, affected cells/ranges, prior values and current values before asking for confirmation.
- [ ] Require explicit user confirmation of the proposed undo immediately before writing. Natural-language intent alone is not authorization to modify a workbook.
- [ ] Support three recovery scopes: undo the last FRIDAY operation; undo a user-selected FRIDAY operation or bounded set of operations; restore from a verified original backup. Clearly distinguish these actions in the preview.
- [ ] Before restoring a cell or range, verify that its current value/state still matches the post-change state FRIDAY recorded. If another application or the user has changed it since, detect the conflict and do not overwrite newer work.
- [ ] Reverse dependent multi-step or multi-cell operations in the correct order. Treat a grouped operation as one transaction: if its required preconditions cannot all be satisfied, abort the whole undo rather than leave a misleading partial reversal.
- [ ] Preserve formulas, values, styles and workbook features within the supported format contract; never reconstruct an original workbook from incomplete journal data when a verified backup is required.
- [ ] Prefer restoring a backup to a new file. Replacing the current workbook requires a separate, explicit confirmation after checking the current file identity and detecting newer user changes.
- [ ] Verify the saved result by reopening/re-reading it, checking every intended restored value/formula and relevant workbook structure, then record the undo outcome against the original operation.
- [ ] Keep an auditable, privacy-safe history of original operations and undo attempts, including operation linkage, approval, conflicts, verification and recovery artifact identity. Retention must be bounded and user-visible; never silently discard the only recovery evidence.
- [ ] Make undo requests idempotent; reject repeated undo of an already-reversed operation unless a separately designed redo capability is available and safe.
- [ ] Add adversarial tests for ambiguous operation selection, stale workbook identity, external edits, partial multi-cell conflicts, dependent operations, backup tampering, duplicate/retried undo, crash during restoration and failed post-undo verification.
- [ ] Verify Linux and Windows CI and perform real-device acceptance for every supported workbook format before enabling Smart Undo for users.

**Non-negotiable rule:** FRIDAY may interpret a conversational request, but it must never silently reverse a workbook. No exact operation identification, no preview and approval, no safe precondition check, or no post-restore verification means no undo. Never clobber newer user edits.



## 34. Formula Transparency & Regional Financial Preferences

**Status:** PLANNED — accepted product direction (2026-11-21). These are first-class FRIDAY capabilities and should be designed into the shared formula engine and application settings, not implemented as isolated Excel-only features.

### 34.1 Formula Transparency — Show Calculation

- [ ] Make formula visibility available by default for every supported deterministic calculation.
- [ ] Provide progressive disclosure: concise result/explanation, formula view, and detailed audit/provenance view.
- [ ] Show the exact registered formula name, ID and version; render the symbolic formula and substitute the actual validated input values.
- [ ] Show calculation steps/intermediate values, output, unit/currency, precision and rounding policy.
- [ ] Show source workbook, worksheet, source cell/range or record references, confirmed field mappings, and source integrity/session identifier where available.
- [ ] Show assumptions, missing/unknown inputs, validation warnings and limitations. Never fabricate a step or source reference.
- [ ] Allow copying the symbolic formula and calculation summary; do not imply that a displayed formula is an Excel cell formula unless it is actually compatible with Excel syntax.
- [ ] Preserve an immutable calculation snapshot so later preference changes or source edits cannot silently rewrite an earlier result.
- [ ] Add accessibility and compact/mobile-friendly presentation; formula detail should be expandable rather than overwhelming the main result.

### 34.2 Regional Financial Preferences — Preferred Currency

- [ ] Add a global Preferred Currency setting in FRIDAY Settings, shared by Excel Intelligence and future financial features.
- [ ] Use an explicit ISO 4217 currency code as identity (e.g. PKR, USD, EUR); symbols are presentation only and are never the currency identity.
- [ ] During onboarding, offer a regional default based on the user's selected country or device locale, with explicit user confirmation/editing. Do not silently infer currency from IP or location.
- [ ] Keep country/region, display locale, number grouping, date format and preferred currency as distinct settings; users can change them independently.
- [ ] Treat preferred currency as a default for new inputs and a display preference—not proof of the currency of imported data and not an instruction to convert existing values.
- [ ] Every monetary source value and calculation result must retain its own currency code. Preserve the original currency in saved calculation snapshots.
- [ ] Never combine incompatible currencies or relabel an amount after a preference change. Currency conversion requires an explicit user request, dated exchange rate, named source, rate provenance and disclosed rounding; no silent or implicit conversion.
- [ ] Support mixed-currency workbooks by identifying and grouping currencies separately; request clarification or explicit conversion instructions before cross-currency totals.
- [ ] Use currency metadata for minor-unit/display precision (not a universal two-decimal assumption); keep internal calculation precision and presentation rounding as separate policies.
- [ ] Add tests for PKR defaults, user overrides, locale/currency independence, currency mismatch, mixed-currency workbooks, historical snapshot stability, and explicit conversion provenance.

**Design principle:** Formula Transparency answers “How did FRIDAY get this result?” Regional Financial Preferences answers “Which currency and regional conventions should FRIDAY use?” Neither feature may weaken source provenance, formula validation, or the no-silent-conversion rule.

### 34.3 Professional Financial Reporting — three presentation levels

**Status:** PLANNED — accepted product direction (2026-11-21). This presentation model applies to formal accounting outputs and future financial reports, not only Excel Intelligence.

- [ ] Provide three coordinated presentation levels over the same trusted calculation result and immutable calculation snapshot:
  1. **Professional Report:** clean, publication-ready financial statements and management reports. Keep formula mechanics out of the main statement by default; present appropriate headings, reporting entity, reporting period/date, currency, comparative columns where supported, subtotals, totals, notes and validation status.
  2. **Working Papers:** reviewer-facing schedules that expose each material figure's registered formula, formula ID/version, applied input values, intermediate calculations, source accounts/columns/cells/ranges, mapping confirmations, assumptions, rounding and exceptions.
  3. **Audit Trail:** detailed evidence and history, including source-session/integrity references, source locations, confirmed mappings and who/what confirmed them where identity is available, formula versions, calculation snapshot identifiers, validation outcomes, warnings, overrides/adjustments with reasons, and reproducible calculation inputs.
- [ ] Make Professional Report the default presentation when the user requests a formal financial statement or report; keep formula details available through Working Papers and evidence through Audit Trail without cluttering the report.
- [ ] Allow users to switch presentation levels without recalculating, mutating or silently changing the underlying result. Any genuine change to inputs, mappings, formula version or accounting policy must create a new calculation snapshot and be clearly distinguished from a presentation-only change.
- [ ] Support the same underlying figures consistently across all three views. Reconcile totals, labels, currency, reporting period, signs, precision and rounding; do not let a view independently recalculate or reinterpret values.
- [ ] Design professional templates for balance sheets/statements of financial position, income statements/statements of profit or loss, cash-flow statements, trial balances, general-ledger reports, receivables/payables ageing, budgets, forecasts and management reports. Add each report type only after its accounting definitions and required data are reviewed.
- [ ] Separate arithmetic checks, accounting-invariant checks, source-data completeness/classification checks and reporting-framework/policy checks. Display exactly which checks were performed and which remain outstanding.
- [ ] Never equate a mathematically balanced balance sheet or ledger with proof that the accounts are correct, complete, compliant or audited. Do not label a report “audit approved”, “compliant” or equivalent without a separately defined and evidenced authorization/review process.
- [ ] Provide honest report states such as Draft, Review Required and Validated, with explicit criteria and explanations. “Validated” must identify the validations passed and must not imply external audit assurance.
- [ ] Keep report generation read-only with respect to source workbooks and accounting records unless a separate, permissioned export workflow is explicitly designed and approved. Generated documents must disclose source period, currency, relevant limitations and preparation status.
- [ ] Add regression tests proving that the three views render the same calculation snapshot, presentation switching does not change values, changed inputs create a distinct snapshot, formula/source evidence remains consistent, mixed currencies are not combined, and a balanced-but-incomplete dataset is not misrepresented as fully validated.

**Design principle:** One trusted calculation engine and one immutable result; three purpose-built experiences. Professional presentation should be clean, Working Papers should explain the numbers, and the Audit Trail should preserve the evidence. Presentation must never overstate accounting correctness or assurance.


