# OpenJarvis Integration Audit & FRIDAY Migration Plan

> **Status:** Initial source-level audit completed; integration decision is deliberately staged.
> **Audit date:** 2026-11-19
> **FRIDAY branch audited:** `feature/phase-2-skills`
> **FRIDAY audit base:** `6cba2052157861aef56f550764db27a284d80921`
> **OpenJarvis upstream:** [open-jarvis/OpenJarvis](https://github.com/open-jarvis/OpenJarvis)
> **OpenJarvis revision inspected:** `427c2dd11d180ac254a03e92c5fb01189fb4b23b` (main, 2026-11-19 06:11:29 UTC)
> **Purpose:** Decide which upstream capabilities can save FRIDAY engineering time without weakening FRIDAY's privacy, security, low-resource, licensing or product requirements.

This is an engineering assessment, not a claim that OpenJarvis or FRIDAY has passed an independent security audit. Source-level review cannot replace running the upstream test suite, dependency/license scanning, integration tests, threat modelling or physical platform acceptance.

---

## 1. Executive decision

**Recommendation: adopt a reuse-first, selective-integration strategy. Do not perform a wholesale replacement or blind fork at this stage.**

OpenJarvis is a substantial candidate foundation. It already has a mature set of local-AI abstractions, engines, model discovery, agents, tools, memory, skills, scheduling, telemetry/traces, connectors, a React frontend, Tauri desktop packaging and Rust crates.

However, it is not a drop-in implementation of FRIDAY's product requirements. In particular, its documented system-access behavior and configurable security defaults do not meet FRIDAY's strict user-consent model without additional enforcement. Its skill system is primarily an instruction/pipeline/tool composition system, not proof of an isolated, independently installed native plugin with revocable operating-system permissions.

FRIDAY should therefore:
1. Keep its existing Rust/Tauri security authority, user-selected file grants, preview/approval/backup/undo workflows and Coding Assistant security work.
2. Reuse suitable OpenJarvis primitives or adapters after source and dependency review.
3. Treat OpenJarvis as an upstream component source, not FRIDAY's identity or an unquestioned security boundary.
4. Avoid maintaining two full application shells or two competing permission systems.
5. Make integration incremental, reversible and testable. No broad migration until a small compatibility spike passes Linux and Windows CI.

**No production guest execution is enabled by this audit.** Coding Assistant's existing guest-execution gates remain unchanged.

## 2. What OpenJarvis actually is

The inspected repository identifies itself as a local-first personal-AI framework and uses Apache License 2.0 at the repository root. Its current `pyproject.toml` marks the package as **Alpha**, requires Python `>=3.11,<3.14`, and declares a broad set of core and optional dependencies.

Important architecture clarification: OpenJarvis is **not Python-only**. It combines:
- A Python package under `src/openjarvis`.
- A Rust workspace under `rust/` with crates for core, engines, agents, skills, tools, security, telemetry, sessions, scheduling and other components.
- A React frontend under `frontend/`.
- A Tauri desktop application configuration under `frontend/src-tauri/`.
- A separate `desktop/` area containing additional desktop assets.

FRIDAY already uses Tauri 2, React/TypeScript/Vite and a Rust backend. The two projects therefore share important desktop technologies. The main integration question is not whether the UI stack is compatible; it is which backend primitives and security boundaries should remain authoritative.

### Upstream evidence inspected

- Repository metadata, default branch and licence: https://github.com/open-jarvis/OpenJarvis
- Exact inspected upstream commit: https://github.com/open-jarvis/OpenJarvis/commit/427c2dd11d180ac254a03e92c5fb01189fb4b23b
- `README.md`: installation, agents, skills, supported surfaces and project positioning.
- `pyproject.toml`: Python support range, alpha classifier, core dependencies and optional extras.
- `docs/architecture/overview.md`: five primitives and registry architecture.
- `docs/user-guide/security.md`: capability policy profiles and guardrails.
- `docs/user-guide/system-access.md`: concrete filesystem, shell, confirmation and sandbox limitations.
- `docs/user-guide/skills.md`: skill manifest, instruction and pipeline behavior.
- Source inspected: `security/capabilities.py`, `security/file_policy.py`, `security/runtime.py`, `skills/security.py`, `skills/manager.py`, `skills/executor.py`, `tools/_stubs.py`, `tools/file_read.py`, `tools/file_write.py`, `tools/shell_exec.py`, `tools/approval_store.py`, `engine/_discovery.py`, `core/config.py`.
- FRIDAY source inspected: `README.md`, `package.json`, `src-tauri/Cargo.toml`, `src/SkillsManager.tsx`, `src/styles.css`, `src-tauri/src/skills.rs`, `src-tauri/src/file_assistant.rs`, `src-tauri/src/assistant.rs`, `docs/PRIVACY-AND-SECURITY.md`, `docs/PRODUCT-PLAN.md` and the Coding Assistant completion checklist.

The upstream main branch moves frequently. Pin an exact release/commit for every integration and repeat this audit against that pin before shipping.

## 3. Feature comparison and reuse recommendation

| Capability | OpenJarvis evidence | FRIDAY evidence | Recommendation |
|---|---|---|---|
| Desktop shell | React frontend and Tauri 2 packaging exist | React + Tauri 2 already exists | **Retain FRIDAY shell.** Review upstream interaction patterns, but do not run two desktop shells. |
| Visual design | Existing OpenJarvis visual system | Current FRIDAY CSS is green/forest themed | **Build FRIDAY's chosen dark navy/electric-blue design** in the existing UI. Do not copy upstream branding or colors. |
| Inference engine abstraction | Engine registry and common inference interface; Ollama, vLLM, SGLang, llama.cpp and cloud engines are documented | Current assistant has local Ollama integration | **High-value reuse candidate.** Compare Rust engine crates and Python API; choose one integration boundary. Enforce explicit provider choice and no silent cloud fallback in FRIDAY. |
| Model catalogue/discovery | Built-in model catalogue, discovered models and engine health/model listing | Local Ollama model list/pull exists | **Reuse/adapt** model metadata and discovery where compatible; FRIDAY remains authority for user choices, cost, licence and hardware warnings. |
| Hardware detection | CPU/RAM/GPU detection and recommendations | FRIDAY roadmap requires 4/8/16 GB resource tiers | **Reuse/adapt** detection; independently verify recommendations and avoid promising performance from detection alone. |
| Agent orchestration | Simple, orchestrator, ReAct, operative, research and code-agent families | FRIDAY has local chat and specialized skill screens; broad orchestration is not complete | **Candidate for selective reuse.** Do not expose powerful agents until every tool call passes FRIDAY's permission gateway. |
| Tool registry/execution | ToolSpec, registry, executor, capability metadata, timeouts, confirmation metadata and audit events | FRIDAY has Tauri commands and Rust feature modules | **Reuse concepts and compatible tool definitions; keep execution authorization in FRIDAY Rust.** Do not grant upstream tools direct desktop authority. |
| Skills | Catalog/import, instruction-only skills, TOML pipelines, dependencies, adapters and tool wrapping | FRIDAY has a compile-time first-party catalogue of declarative instruction packages with hash verification and install/enable/load/unload | **Reuse skill format/catalog ideas selectively.** Build FRIDAY's publisher/provenance, signed package, permission-grant, lazy runtime and rollback contract. Do not equate imported instructions with safe executable plugins. |
| Memory | SQLite/FTS5 default and optional vector/BM25/ColBERT/hybrid backends | FRIDAY has an early local memory/workspace implementation | **Audit and reuse/adapt** a suitable backend only after data ownership, encryption, export/delete and resource behavior review. |
| Web research | Search tools, browser-related tools and deep-research agent | FRIDAY has a web research module foundation | **Reuse/adapt** provider/tool connectors; enforce explicit network grants, source citations and external-data disclosure. |
| PDF | PDF tool and optional PDF dependencies | FRIDAY has a PDF Reader module/backend | **Compare before extending.** Retain whichever implementation has better bounded parsing, page references and tests. |
| Excel intelligence | No verified complete Excel-workbook intelligence feature established in this audit | FRIDAY roadmap marks Excel Intelligence planned | **Build as a FRIDAY first-party skill**, reusing appropriate document/data libraries after licence and security review. This is not a reason to duplicate an existing verified OpenJarvis Excel feature; none was established here. |
| Job Hunter | No verified end-to-end job discovery/application skill established | FRIDAY product plan describes permitted sources and human-reviewed applications | **Build as a FRIDAY skill** on shared browser/search, CV and approval services. Respect source terms; no LinkedIn scraping or mass-apply automation. |
| Email/connectors | Many channel and connector integrations, including Gmail-related optional dependencies | FRIDAY has no active email integration yet | **Reuse connector patterns or supported official APIs** after OAuth scope and data-flow review. FRIDAY approval remains mandatory before sending. |
| Voice/speech | Speech, transcription and TTS packages/integrations | FRIDAY voice is planned | **Reuse optional adapters** after checking licences, platform support, model download size and microphone disclosure. Keep optional dependencies out of the core install. |
| Scheduling | Scheduler and persistent/continuous agent support | FRIDAY has no verified production scheduler | **Candidate for reuse** after idle-resource, wake behavior, permissions and cancellation review. |
| Telemetry/traces | Local telemetry/traces and learning feedback; project also declares analytics-related dependencies | FRIDAY requires a transparent, privacy-first product | **Do not enable external analytics by default.** Reuse local metrics only if users can inspect, disable, export and delete them. Audit every outbound event. |
| Sandboxing | Docker/Podman sandbox wrapper and tool execution options are documented | FRIDAY has a substantial Windows Sandbox design and guest-runner primitives; execution remains disabled | **Do not substitute one for the other without threat-model comparison.** Keep FRIDAY's stronger execution-disabled invariant and existing Stage 4 acceptance gates. |
| Security / permissions | Multiple scanners, capability policy, file policy, confirmation metadata and audit events | FRIDAY has explicit grants, file previews, approvals, backups, undo and a Coding Assistant security track | **Do not adopt upstream policy as FRIDAY's authority unchanged.** Reuse individual reviewed mechanisms only after deny-by-default and runtime enforcement are demonstrated. |

## 4. Security findings that affect the integration decision

These are specific behaviors documented or visible in the inspected upstream source. They do not mean every OpenJarvis configuration is insecure; they mean FRIDAY must not assume the defaults satisfy FRIDAY's product promise.

### 4.1 Tool availability is not the same as user-granted permission

OpenJarvis documentation says the tool list is empty by default, but its system-access guide also documents that:
- `shell_exec` runs arbitrary commands as the current user.
- `apply_patch` and `code_interpreter` can reach beyond the file-tool directory restriction.
- Empty `allowed_dirs` for file tools means every path is allowed.
- The documented sensitive-file filter is a limited filename list, not a complete security boundary.

**FRIDAY requirement:** each file/folder/application/network/account grant must be explicit, narrowly scoped, attached to a user-visible purpose and enforced at the operation boundary. A tool being absent from a default config is not sufficient.

### 4.2 Capability policy has permissive modes

The inspected Python `CapabilityPolicy` constructor defaults to `default_deny=False`. The security guide documents stricter shared/server profiles, while the personal/default profile leaves capability checks opt-in. FRIDAY cannot rely on a permissive or optional policy configuration.

**FRIDAY requirement:** backend-enforced deny-by-default. Missing, malformed, stale or unverifiable grants must fail closed. The policy must be tied to the actual skill/provider/session identity and resource, not merely a model-supplied name.

### 4.3 Confirmation behavior differs by entry point

The upstream system-access guide states that confirmation behavior depends on how the agent is launched: interactive CLI prompts, while HTTP server and desktop app paths are documented as auto-approving confirmation-marked tools. The same guide notes a configuration option named `enforce_tool_confirmation` that it says is not read by the tool execution path.

**FRIDAY requirement:** never trust a UI flag, tool metadata or an upstream callback as the sole approval boundary. Sensitive operations must be approved by FRIDAY's trusted UI/backend transaction, bound to the exact operation, resource, content/hash and expiry, and revalidated at execution.

### 4.4 Skill trust labels are not an execution sandbox

OpenJarvis classifies skills by provenance/trust tier and validates declared capabilities. Its skill format can include executable structured pipelines, scripts and tools. A trust tier or manifest declaration alone does not confine code or prevent a tool from using broader process privileges.

**FRIDAY requirement:** third-party skill content is untrusted. First-party and external skills must declare capabilities, but grants are enforced by FRIDAY. Do not load arbitrary third-party scripts into the privileged application process. Any executable skill runtime needs a separately reviewed isolation boundary, signed provenance and resource limits.

### 4.5 Cloud routing and telemetry need explicit user control

OpenJarvis supports local and cloud engines and documents engine discovery/fallback behavior. It also records telemetry/traces; the telemetry guide describes persistent local SQLite records, while the dependency list includes analytics-related packages.

**FRIDAY requirement:** explicit provider selection per request or policy, no silent local-to-cloud fallback, clear disclosure before data transfer, provider-specific cost/data-access display, and an enforced local-only/offline mode. Audit actual network behavior and analytics separately; do not assume a dependency proves data is transmitted, or assume it is not transmitted without checking runtime paths.


### 4.6 Rust security crate also defaults to allow

The Rust `openjarvis-security` crate independently confirms the permissive default: `CapabilityPolicy::default()` calls `new(false)`, and an agent with no explicit policy is allowed unless the policy was constructed with `default_deny = true`. This is a direct source-level finding, not only a documentation concern.

**FRIDAY requirement:** never use the upstream default constructor for FRIDAY authorization. If any upstream capability code is reused, construct an explicit deny-by-default policy and wrap it with FRIDAY's own user-grant and resource-identity enforcement. Prefer keeping the FRIDAY authorization service authoritative.

### 4.7 Upstream Rust hardware detection needs Windows work

The inspected `openjarvis-core/src/hardware.rs` currently:
- Checks for GPU tools by invoking the Unix `which` command.
- Reads CPU brand from `/proc/cpuinfo` except on macOS.
- Reads total RAM from `/proc/meminfo` except on macOS.

Those paths do not provide a native Windows implementation. On Windows, the current Rust detector can therefore return no GPU, `unknown` CPU brand and `0.0` GB RAM rather than reliable Windows hardware details. Its current recommendation logic also maps no detected GPU to llama.cpp, which is not a complete FRIDAY 4/8/16 GB resource policy.

**FRIDAY requirement:** do not adopt this hardware detector as-is for Windows. Add native Windows detection (or use FRIDAY's already implemented Windows resource queries), test it on Windows CI, and report unknown values honestly rather than recommending a model based on missing data.

### 4.8 Upstream desktop capabilities and package licence need separate review

The inspected OpenJarvis Tauri capability file grants shell plugin permissions including execute, spawn, stdin-write and kill, plus a sidecar Ollama execute rule. This may be appropriate for OpenJarvis's own app design, but FRIDAY must not copy these permissions without reviewing every command and sidecar invocation. FRIDAY should keep the narrowest possible Tauri capabilities and mediate process launches in trusted Rust code.

There is also a licence metadata discrepancy requiring clarification before reusing the desktop crate: the repository root reports Apache-2.0 and has an Apache-2.0 LICENSE, while `frontend/src-tauri/Cargo.toml` declares `license = "MIT"`. This audit does not assume which notice governs every file in that subproject. Obtain provenance/maintainer clarification and inspect applicable file headers, assets and dependency licences before copying or redistributing the desktop crate. The overall dependency/licence audit remains incomplete.


### 4.9 Engine crate is a plausible narrow integration, not a complete provider platform

The inspected `openjarvis-engine` Rust crate exposes an `InferenceEngine` trait and concrete Ollama, llama.cpp and OpenAI-compatible server adapters (including vLLM/SGLang wrappers). The generic OpenAI-compatible adapter targets `/v1/chat/completions` and `/v1/models`, accepts a base URL and optional bearer key, and can be configured for a compatible cloud endpoint such as OpenAI's API.

Important limitations:
- The crate is version `0.1.0` in the upstream workspace and is not yet proven compatible with FRIDAY's target matrix.
- `generate()` is synchronous/blocking while streaming is async. FRIDAY must not call blocking inference on the Tauri async runtime thread.
- The adapter's generic OpenAI-compatible endpoint does not by itself provide native provider-specific APIs for every cloud provider (for example, Anthropic/Gemini-specific semantics).
- Tool calls returned by the engine are model proposals only; they must be routed through FRIDAY's own allowlisted tool gateway and approval rules.
- The engine crate depends on `openjarvis-core`, `rig-core`, Tokio, Reqwest, futures, schemars and other workspace dependencies. Its actual compile time, binary-size and dependency-license footprint must be measured before adoption.

**Decision:** this is the best first compatibility-spike candidate because FRIDAY already has Ollama and wants optional cloud engines. Test it as an adapter in a reversible branch. Do not replace the current Ollama path until behavior, cancellation, streaming, timeouts, provider disclosure and CI are proven.

## 5. What FRIDAY already has that should be preserved

### 5.1 Desktop foundation

FRIDAY already uses Tauri 2, React, TypeScript, Vite and Rust. Its product identity, selected dark navy/electric-blue direction, Control Centre, Transparency Centre and feature priorities belong to FRIDAY.

### 5.2 Existing skill lifecycle foundation

At the inspected FRIDAY revision, `src-tauri/src/skills.rs` contains a compile-time catalogue of first-party skills. It creates declarative-instruction packages, hashes instruction content, verifies package integrity, and exposes install, enable, load, unload and uninstall commands. The frontend already exposes these lifecycle states.

This is a useful foundation, but it is **not yet the full user-facing Skills Store**:
- The catalogue is compile-time and first-party only.
- Loading currently adds instructions to session memory; the backend explicitly says the skill's tools are not yet connected.
- The listed permission strings are descriptive metadata, not proof of a complete per-skill runtime grant/revocation system.
- Arbitrary third-party package installation, publisher verification, signatures, dependency isolation, updates and rollback remain future work.

Build on this instead of replacing it wholesale.

### 5.3 File and Coding Assistant security work

FRIDAY's File Assistant includes bounded selected-file access, previews, explicit approval, backup and undo/recovery flows. Coding Assistant has a separate detailed completion checklist and a large security-focused implementation. Preserve these as first-party security assets. Do not replace them with generic upstream file read/write tools.

The Coding Assistant guest-execution invariant remains: no host execution and no guest execution until its own complete security and physical acceptance gates pass.

## 6. Recommended target architecture

### 6.1 Keep one FRIDAY desktop application and one permission authority

- **UI:** existing FRIDAY React/Tauri shell; implement dark navy/electric blue and the FRIDAY Control Centre.
- **Trusted core:** Rust remains the authority for user grants, permission revocation, operation approvals, file access, backups/undo, provider secrets, audit records and high-impact action policy.
- **Skill contract:** FRIDAY-owned manifest and package lifecycle, potentially compatible with the Agent Skills format and selected OpenJarvis skill metadata. Skill installation, enabling, loading and granting permissions are distinct actions.
- **Inference:** integrate a reviewed engine abstraction or selected upstream Rust/Python adapters behind a FRIDAY provider interface. Explicitly select the provider/model. No automatic cloud fallback.
- **Agent orchestration:** reuse an upstream agent loop only if every tool invocation is routed through FRIDAY's trusted tool/permission gateway. Agents must not receive direct unrestricted filesystem, shell, network or credential access.
- **Optional runtimes:** install optional Python/model/speech dependencies only when the user installs the relevant capability. Do not make the full OpenJarvis dependency set a mandatory core dependency.
- **Data:** local by default; every external provider has a data-flow and cost disclosure. User can disconnect it and return to local-only operation.
- **Updates:** signed/integrity-checked packages and rollback; no unverified skill update may silently gain new permissions.

### 6.2 Python sidecar versus Rust crates

Do not choose an integration boundary by language preference alone.

Evaluate in this order:
1. Inspect OpenJarvis Rust crate APIs, dependency trees, platform support, license metadata and test coverage for the exact primitives FRIDAY needs.
2. If a suitable Rust crate is stable and has a small compatible dependency footprint, test direct use behind a FRIDAY adapter.
3. If the Python implementation is materially more complete, evaluate an optional, version-pinned local sidecar. Bind it to loopback only, authenticate the IPC channel, restrict the exposed API, supervise its lifecycle and make its resource use visible.
4. A same-user sidecar is **not** a security sandbox. It must not be given direct user-file, shell, credential or unrestricted network authority. FRIDAY must mediate every privileged operation. If that cannot be enforced, do not integrate the sidecar for tool execution.
5. If neither boundary meets the security/resource/platform requirements, reuse the design or protocol only and retain/build the implementation in FRIDAY.

### 6.3 Resource policy

OpenJarvis has many optional extras, including large inference, speech, browser, vector-memory and channel dependencies. Do not install them all with FRIDAY.

- Keep the core desktop shell and minimal local provider integration lean.
- Make each optional engine/skill/runtime independently installable.
- Show download size, disk requirements, supported hardware and expected runtime memory where known.
- Unload inactive skill runtimes and release model resources where supported.
- Model weights may dominate storage/RAM; do not imply that an installed skill itself is the only resource cost.
- Preserve 4 GB minimum target, 8 GB recommended and 16 GB ideal as design targets, not verified compatibility claims.

## 7. Product capability gaps confirmed in this initial audit

The following are not established as complete OpenJarvis capabilities by the source reviewed here and remain FRIDAY product work or require a deeper targeted audit:

- FRIDAY's exact per-skill permission grant/revocation UX and backend enforcement.
- FRIDAY's Transparency Centre with provider-by-provider data categories, cost and actual routing visibility.
- A complete signed third-party Skills Store with publisher trust, permissions, rollback and safe runtime isolation.
- Excel Intelligence at the level requested (workbook analysis, formula help, charts, edits with preview/backup/undo).
- Job Hunter that uses permitted job sources, avoids prohibited scraping/mass applications and requires user approval before submission.
- A safe crypto research/paper-trading/live-trading skill with disabled-by-default live trading and explicit risk limits.
- FRIDAY's chosen blue visual identity and Control Centre.

## 8. Licensing and commercial distribution

The inspected OpenJarvis repository reports Apache-2.0 and its root LICENSE is the Apache License, Version 2.0. This permits commercial use and proprietary FRIDAY additions when the licence conditions are followed.

Before redistributing any upstream code:
- Pin the exact upstream commit/release.
- Include a copy of the applicable Apache 2.0 licence.
- Preserve applicable copyright, patent, trademark and attribution notices.
- Mark modified upstream files with prominent change notices.
- Include applicable NOTICE attributions if present.
- Audit every dependency and bundled asset separately; the repository's top-level licence does not automatically determine every dependency's licence.
- Generate and maintain a software bill of materials and third-party notices.
- Do not use OpenJarvis/Stanford marks in a way that implies endorsement.
- Obtain a qualified legal review before commercial release if the distribution model or third-party licence position is unclear.

## 9. Staged implementation plan

No wholesale migration is authorized by this audit. Proceed in small, reversible batches.

### Stage A — Complete the architecture and supply-chain audit

- [ ] Pin an upstream release/tag (not a moving `main` branch) for the candidate integration.
- [x] Inventory candidate Rust crates and inspect engine/core/security crate manifests and key source APIs. Findings: Rust 1.89 workspace requirement; security policy defaults to allow; hardware detector lacks native Windows paths; desktop crate declares MIT while repository root is Apache-2.0. See sections 4.6–4.8.
- [ ] Complete API/build compatibility proof, full crate/dependency graph and platform-specific CI assessment.
- [ ] Inventory upstream Python modules/extras and determine the smallest useful optional install.
- [x] Inspect the Tauri desktop shell manifest and default capability file. Shell execute/spawn/stdin/kill permissions are present; do not copy the capability set without a command-by-command review.
- [ ] Inspect all desktop IPC handlers and sidecar launch call sites before considering any UI/runtime reuse.
- [ ] Review upstream CI, tests, open security advisories, release history and known Windows limitations.
- [ ] Generate a dependency and licence report for the chosen components and bundled assets.
- [ ] Run the relevant upstream tests in a reproducible environment; record what was and was not run.
- [ ] Complete a feature-by-feature source mapping for engine, model, hardware, agent, skill, memory, research, PDF, speech, scheduler, telemetry and connectors.

### Stage B — Prove one narrow integration

- [ ] Choose one low-risk capability (prefer engine/model discovery or hardware metadata) with clear boundaries.
- [ ] Implement an adapter without changing FRIDAY's permission authority or UI identity.
- [ ] Verify that local-only mode cannot call a cloud provider or silently fall back.
- [ ] Verify provider identity, model licence/cost metadata and telemetry behavior.
- [ ] Measure build size, install footprint, idle RAM and active RAM on available CI/dev environments; label estimates honestly.
- [ ] Run FRIDAY Linux and Windows CI and the relevant upstream tests.
- [ ] Keep the adapter behind a feature flag until its contract and tests pass.

### Stage C — Adopt shared capabilities incrementally

- [ ] Add model/engine catalogue and hardware-aware recommendations.
- [ ] Add agent orchestration only after the Rust permission gateway intercepts every tool invocation.
- [ ] Adapt skill manifests/catalogue without allowing untrusted scripts into the privileged process.
- [ ] Adopt memory/research/PDF/speech/scheduler components only after targeted data-flow, licence and resource review.
- [ ] Implement FRIDAY's Skills Store, Control Centre and Transparency Centre against real backend state.
- [ ] Preserve existing file-edit and Coding Assistant approval/backup/undo mechanisms.

### Stage D — Product skills and release

- [ ] Excel Intelligence.
- [ ] Job Hunter using permitted sources and human-reviewed submissions.
- [ ] Email Assistant with minimum OAuth scopes and send approval.
- [ ] CV Studio and Voice.
- [ ] Crypto Intelligence, then backtesting/paper trading; live trading remains a separate later release gate.
- [ ] Full provider disclosure, local-only/offline enforcement, skill revocation and package rollback.
- [ ] Windows/Linux installer, update, upgrade, uninstall and recovery acceptance.
- [ ] Physical Windows Sandbox acceptance remains separately required for Coding Assistant guest execution.

## 10. Immediate next engineering task

**Do not clone or merge the entire upstream repository yet.**

The next task is a bounded compatibility spike:
1. Inspect the OpenJarvis Rust workspace crates `openjarvis-engine`, `openjarvis-core` and `openjarvis-security` at the pinned revision.
2. Compare their API/dependency requirements with FRIDAY's current Rust crate versions and Windows/Linux build setup.
3. Select the OpenJarvis Rust `InferenceEngine` / Ollama + OpenAI-compatible adapter as the first narrow compatibility spike; separately retain FRIDAY's native Windows hardware detection rather than adopting the current Unix-oriented upstream detector.
4. Record a go/no-go decision based on compile/test results, footprint and security boundaries.
5. Only then decide whether to adopt selected Rust crates, an optional Python sidecar, or just the upstream design/protocol.

No user data, provider secrets, arbitrary shell access or unrestricted file tools may be exposed to the compatibility spike.

---

## 11. Audit decision log

| Decision | Status | Reason |
|---|---|---|
| OpenJarvis is a candidate foundation | ACCEPTED | Broad relevant capability set and Apache-2.0 root licence. |
| Wholesale replacement of FRIDAY | REJECTED FOR NOW | Duplicates the Tauri/React shell and risks discarding existing Rust security work; integration and dependency costs are not yet measured. |
| Reuse-first selective integration | RECOMMENDED | Best balance of saved engineering time, maintainability and FRIDAY-specific control. |
| Upstream security defaults as FRIDAY policy | REJECTED | Documented permissive capability mode and powerful system tools do not meet FRIDAY's deny-by-default consent requirements. |
| Upstream Skills system as complete FRIDAY Skills Store | REJECTED AS-IS | Useful catalogue/import/pipeline primitives, but does not establish FRIDAY's complete package provenance, runtime isolation and revocable permission model. |
| Keep FRIDAY Rust core as permission authority | REQUIRED | One enforceable source of truth for permissions, approvals, backups and revocation. |
| Keep Coding Assistant guest execution disabled | REQUIRED | Separate Stage 4 security and physical acceptance gates remain open. |
| Build FRIDAY's blue identity and Control Centre | REQUIRED | Product identity and user requirements are independent of upstream UI. |

## 11.1 Reversible compatibility spike started

A separate experiment branch was created so the main FRIDAY development branch does not take on an unverified dependency:

- Branch: `experiment/openjarvis-engine-compat`
- Base: `765de1fc20ad937fc2caab932fb1df3b4b59191f`
- OpenJarvis engine pin: `427c2dd11d180ac254a03e92c5fb01189fb4b23b`
- Spike scope: compile and instantiate the upstream Ollama and OpenAI-compatible Rust adapters; assert the trait objects are `Send + Sync`.
- No tools, user files, credentials, network calls or provider requests are exercised by the test.
- Workflow: [OpenJarvis engine compatibility spike](https://github.com/Friday-AI-Personal-Assistant/FRIDAY/actions/runs/44540769399)
- Run ID: `44540769399`
- Initial state at documentation update: Linux and Windows jobs started; Linux passed formatting and entered compile/test; Windows was still setting up stable Rust. Final results must be recorded after both jobs finish.

The spike is intentionally separate from production code. It has no production dependency impact unless a later reviewed change is merged.

## 11.2 Compatibility spike result and current integration decision

**Result: PASS — compile/API compatibility only.** Workflow run [44540769399](https://github.com/Friday-AI-Personal-Assistant/FRIDAY/actions/runs/44540769399) completed successfully on both `ubuntu-latest` and `windows-latest`. Both jobs passed formatting and `cargo test --all-targets` for the isolated spike crate.

- FRIDAY spike commit: `3eec6b4d96cd6aafb5367789b561b1adb903dab6`.
- OpenJarvis pin: `427c2dd11d180ac254a03e92c5fb01189fb4b23b`.
- Verified: the pinned crate compiles on both CI operating systems; `OllamaEngine` and `OpenAICompatEngine` can be constructed; both satisfy `Send + Sync`; engine IDs match the expected values.
- Not verified: real inference, streaming behavior, cancellation, health checks, provider requests, credential handling, cloud/local routing enforcement, production FRIDAY integration, dependency/licence compliance, binary/install footprint, idle/active RAM, or physical Windows behavior.
- No production dependency was added to FRIDAY. The experiment branch remains separate and reversible.

**Decision: conditional GO for a second, still-isolated evaluation stage; NO-GO for production adoption today.** The compile proof removes one compatibility uncertainty, but does not justify replacing FRIDAY's Ollama implementation or adding an unmeasured dependency graph to the core application. Next evaluate API semantics and dependency/licence/footprint cost, then run a local-Ollama integration test through a FRIDAY-owned adapter with no tools or filesystem authority. Cloud adapters remain opt-in and must be blocked in strict local-only mode. Add a lockfile/reproducible pin before any production adoption.

The desktop crate's MIT metadata versus repository-root Apache-2.0 licence discrepancy remains unresolved; the spike does not copy that crate and does not resolve the licensing question.

## 11.3 API/dependency review and final decision for the inference-engine candidate

The pinned upstream manifests and implementation were reviewed after the cross-platform compile spike.

### Dependency and reproducibility findings

- The upstream Rust workspace declares Rust 1.89 and includes 17 workspace crates; its workspace dependency set includes Tokio with `full`, Reqwest with blocking + streaming, `rig-core`, `schemars`, SQLite and additional dependencies used by other workspace members.
- `openjarvis-engine` directly depends on `openjarvis-core`, Serde, Tokio, Reqwest, async-trait, once_cell, futures, tokio-stream, rig-core and schemars. `openjarvis-core` adds TOML, regex, UUID and parking_lot among others.
- A workspace `rust/Cargo.lock` exists at the inspected upstream commit (3,301 lines). The FRIDAY spike crate itself does not currently carry a committed lockfile. The upstream workspace lock is not automatically a minimal or directly reusable lock for FRIDAY; a production candidate would need its own generated and committed lockfile and `--locked` CI.
- Exact compile/install size, cold build time, idle RAM and active RAM have not been measured. Do not make footprint claims.

### API behavior findings

- `InferenceEngine::generate` is synchronous and can block the caller; `stream` is async. The trait itself does not provide FRIDAY's permission, provider-transparency, local-only or user-approval policy.
- The inspected `OllamaEngine` uses a blocking Reqwest client for `generate`, constructs a timeout directly from an unchecked floating-point seconds value, and accepts a configurable host string. Its synchronous generation path can block a Tauri async worker if called directly.
- The Ollama generation path can send `extra.tools` to the model endpoint and returns model-produced tool-call proposals. The streaming implementation in this revision ignores its `extra` argument. Tool calls remain untrusted proposals and must never execute without FRIDAY's independent tool registry, exact approval and permission gateway.
- `OpenAICompatEngine` also has a blocking generation method, accepts a configurable host and optional bearer key, and forwards arbitrary extra JSON fields (apart from its special tools handling). A FRIDAY wrapper would need strict provider/endpoint allowlisting, credential isolation, bounded response handling, explicit cloud consent, SSRF protections and local-only enforcement.
- The existing FRIDAY `assistant.rs` already uses asynchronous Reqwest, targets the fixed loopback Ollama endpoint `127.0.0.1:11434`, applies message/model bounds, has dedicated bounded coding-response limits and supports streaming. It is a closer fit to FRIDAY's current desktop runtime and strict local-first requirements.

### Final candidate decision

**NO-GO for adopting `openjarvis-engine` as FRIDAY's production inference engine or replacing the existing FRIDAY Ollama implementation.** The compile spike was useful evidence, but the upstream abstraction does not currently save enough work to justify its blocking API, broader dependency footprint and additional policy-adaptation burden for the use case already implemented by FRIDAY.

- Retain FRIDAY's existing asynchronous local Ollama path.
- Do not add `openjarvis-engine` or its transitive dependency graph to the production application.
- Keep the spike branch as a reproducible compatibility experiment/reference; do not merge it into the product branch.
- Revisit OpenJarvis selectively for other capabilities only after the feature matrix identifies a concrete net benefit and security/licence/resource checks pass.
- Any future cloud provider adapter must be owned by FRIDAY's provider gateway and expose truthful local/cloud state, explicit connection consent, revocation, bounded I/O and strict local-only behavior.
- The upstream desktop crate licence discrepancy remains unresolved and blocks copying that crate; it does not affect the engine-only decision because no desktop crate is being adopted.

**Roadmap consequence:** the isolated inference-engine evaluation is complete with a documented NO-GO. Resume the Coding Assistant filesystem trust-boundary work at Stage 4 section 6A. Guest execution remains disabled.

## 12. Evidence and handoff

| Field | Value |
|---|---|
| FRIDAY branch | `feature/phase-2-skills` |
| FRIDAY base commit | `6cba2052157861aef56f550764db27a284d80921` |
| OpenJarvis inspected commit | `427c2dd11d180ac254a03e92c5fb01189fb4b23b` |
| OpenJarvis inspected date | 2026-11-19 |
| Audit type | Source/docs inspection through GitHub; no local build or upstream tests run in this audit |
| FRIDAY code changed | No product code changed by this audit |
| CI result | OpenJarvis engine compatibility spike PASS on Linux + Windows (run `44540769399`); audit document itself still needs its own CI |
| Physical hardware acceptance | Not performed |
| Next task | Resume Coding Assistant Stage 4 section 6A: handle-relative snapshot traversal and parent identity hardening |

**Maintainer rule:** Re-run this audit whenever the selected upstream pin changes materially. Never treat a moving upstream `main` branch as a reproducible dependency.

## 12.1 Selective reuse execution plan (not an adoption claim)

The OpenJarvis source audit and engine NO-GO decision are complete. **No OpenJarvis production component has been adopted into FRIDAY.** The next OpenJarvis activity is selective, feature-specific evaluation—not a wholesale fork and not an automatic dependency addition.

Candidate reviews may cover document/PDF processing, speech, model catalogue/discovery, web research, scheduling, memory, skill packaging and agent orchestration. Each candidate must be pinned to an exact upstream revision and assessed for licence/provenance, dependency footprint, active and idle resource behavior, Windows/Linux support, maintenance, test quality and integration effort.

Any reused code or capability remains subordinate to FRIDAY's existing product and security contracts:

- FRIDAY Rust backend remains the sole authority for file access, permissions, approvals, revocation, credentials and execution.
- Upstream shell/file tools, default-allow policies, trust tiers and UI confirmation flows must not become FRIDAY capabilities by inheritance.
- Model-generated tool calls are proposals only; they pass through FRIDAY's tool registry and permission gateway.
- Local-only mode must fail closed and must never silently fall back to a cloud provider.
- Third-party providers require explicit connection, clear data/cost disclosure, bounded access and revocation.
- Prefer selective ports/adapters; avoid a second always-running service or mandatory heavyweight runtime unless measurements justify it.
- Integrate one candidate at a time, with Linux/Windows CI, adversarial permission tests and a rollback path.

The first implementation focus remains Coding Assistant Stage 4A filesystem trust-boundary hardening. The selective-reuse track is approved for planning and review, but it does not authorize copying code before the individual review gates pass. Excel Intelligence and Job Hunter remain FRIDAY-specific product requirements and must be validated against their full acceptance criteria.

**Current evidence:** #1007 (`34b15036801ff13addc473252809cbb5d4c613e5`, workflow `44555315714`) passed Linux tests and the Windows desktop build/tests. This does not close physical Windows Sandbox acceptance or enable guest execution.