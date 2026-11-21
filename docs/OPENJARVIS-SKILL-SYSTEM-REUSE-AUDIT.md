# OpenJarvis Skill-System Reuse Audit & FRIDAY Adoption Decision

> **Status:** Source-level comparison complete; no upstream runtime code copied in this batch.
> **FRIDAY branch / base:** `foundation/evaluation` at `da6a9aa88047ed1ef0c5d132f63c3c5130814041` (2026-11-21).
> **OpenJarvis source pin:** `5a9b5268f2432f7ea4fe402e384ecb6b64a649bb` (main, 2026-11-21).
> **Repository:** https://github.com/open-jarvis/OpenJarvis
> **Purpose:** Extract the useful skill, tool and workflow ideas from OpenJarvis, then implement only the FRIDAY-specific gaps.

This is a source-level engineering assessment, not an independent security audit. No OpenJarvis code has been copied into FRIDAY by this change. Any future copied or adapted code must be tied to an exact upstream commit and carry the applicable licence notices.

## 1. Decision

**Adopt the architecture patterns and compatible skill-package conventions; keep FRIDAY's Rust backend as the only authority for permissions and execution.**

The best near-term value is not importing the OpenJarvis Python application or its whole runtime. It is adopting a compatible, inspectable skill package format and borrowing its separation of discovery, manifest metadata, skill resolution, tool adapters and execution events.

FRIDAY already has:
- A Tauri/React desktop shell and Rust backend.
- A built-in catalogue and install/enable/load/unload lifecycle in `src-tauri/src/skills.rs`.
- Versioned manifests and SHA-256 verification for first-party instruction packages.
- A persistent permission store with exact capability/resource grants, expiry, revocation and audit in `src-tauri/src/user_permissions.rs`.
- A backend command policy gateway in `src-tauri/src/permissions.rs`.

The current skill loader is deliberately limited: loading adds declarative instructions to session context and explicitly says that tools are not connected. That is a clear boundary to extend, not a reason to replace the current system.

## 2. What is worth reusing

| OpenJarvis component | What it contributes | FRIDAY decision |
|---|---|---|
| `skills/loader.py` | Directory discovery; accepts `skill.toml` and/or `SKILL.md`; separates package parsing from manager lifecycle. | **Adopt the package convention**, prioritizing instruction-only `SKILL.md` packages. Add a strict Rust parser/validator for the supported subset. Do not port permissive fallback behavior. |
| `skills/types.py` | A manifest model with name/version/description/author/tags/dependencies/capabilities and optional steps. | **Adapt the metadata concepts** to FRIDAY's versioned manifest; add source URL, immutable revision, licence, content hash, trust status and explicit capability declarations. |
| `skills/manager.py` | Discovery, name resolution, dependency-cycle checks, catalog generation and adapter registration. | **Reuse the separation of concerns** (catalogue, resolver, dependency validation, adapters). Keep trusted package roots and precedence rules explicit; no silent overlay mutation. |
| `skills/tool_adapter.py` | Wraps a skill as a tool with a derived input schema and result metadata. | **Adapt the adapter pattern** only after every invocation is routed through FRIDAY's Rust permission gateway. Model-facing metadata is not authorization. |
| `skills/executor.py` | Sequential pipeline steps, context passing, sub-skill resolution, event lifecycle. | **Borrow the explicit step/result/event model**, not its execution implementation. FRIDAY must use typed, allowlisted tool IDs and structured arguments rather than arbitrary template expansion. |
| `tools/_stubs.py` | Tool interface/metadata, central dispatch, timeout, bounded worker queue, confirmation metadata, capability policy hooks and events. | **High-value design reference.** Build a Rust tool registry with typed schemas, capability mapping, per-call authorization, bounded concurrency, cancellation and audit. Do not copy Python thread execution. |
| `workflow/engine.py` | DAG stages, dependencies, condition/loop/transform/agent/tool nodes, event reporting. | **Defer general workflow engine.** Its synchronous execution model and thread-future timeouts do not provide hard cancellation of arbitrary work. First ship a small sequential, typed skill-to-tool bridge. |
| Event bus and telemetry | Lifecycle events for skill/tool/workflow runs. | **Adapt local audit events**, not external telemetry. Record metadata/outcomes only; do not log private file contents, prompts or secrets. |

### Source files reviewed

OpenJarvis, pinned to the revision above:
- `src/openjarvis/skills/manager.py`
- `src/openjarvis/skills/loader.py`
- `src/openjarvis/skills/types.py`
- `src/openjarvis/skills/executor.py`
- `src/openjarvis/skills/tool_adapter.py`
- `src/openjarvis/tools/_stubs.py`
- `src/openjarvis/workflow/engine.py`
- Repository-root `LICENSE`

FRIDAY:
- `src-tauri/src/skills.rs`
- `src-tauri/src/user_permissions.rs`
- `src-tauri/src/permissions.rs`
- `src/SkillsManager.tsx`
- `src-tauri/Cargo.toml`
- `src-tauri/src/lib.rs`

## 3. Important upstream limitations — do not inherit blindly

1. **The OpenJarvis markdown loader is tolerant.** If frontmatter parsing/validation fails, it has a legacy fallback that constructs a manifest anyway. FRIDAY's importer must reject malformed or ambiguous manifests.
2. **Signature verification is optional in the inspected loader.** A package must not become trusted merely because a signature field exists or verification was not requested. FRIDAY must distinguish publisher identity, integrity and security review; none proves the skill is benign.
3. **Prompt-injection scanning is optional and narrowly applied to pipeline argument templates.** It is not a proof that instructions, scripts, references or dependencies are safe. FRIDAY treats all imported content as untrusted.
4. **TOML pipeline templates interpolate context values into JSON-like arguments.** FRIDAY should not adopt arbitrary templating as an execution interface. Use fixed tool IDs, typed argument schemas and explicit output mapping.
5. **Capability pre-checks are not enough by themselves.** FRIDAY must authorize each actual tool invocation against trusted backend policy and the user's current exact grant, including resource scope and expiry.
6. **The workflow engine's timeout around a thread future does not forcibly stop an already-running Python function.** FRIDAY must not describe timeouts as hard execution termination. Executable third-party extensions remain blocked until OS-enforced isolation exists.
7. **Dependency declarations and nested skill calls expand the trust graph.** FRIDAY must pin dependencies, detect cycles, cap graph depth/size, and require each dependency to pass the same import/trust/permission checks.
8. **Discovery precedence can shadow packages.** FRIDAY must prevent duplicate-ID and path-precedence confusion and make publisher/source/version visible.

## 4. FRIDAY implementation contract

### Package compatibility

- Accept a clearly defined, bounded subset of the Agent Skills-style directory format: one `SKILL.md` with YAML frontmatter and markdown instructions.
- Support only explicitly documented frontmatter keys initially: `name`, `description`, `license`, `compatibility`, `metadata`, and a FRIDAY-owned capability declaration extension.
- Reject duplicate/unknown security-sensitive fields, malformed frontmatter, oversized files, invalid IDs, path traversal, symlinks/reparse points and ambiguous package layouts.
- Preserve the original source package and record a canonical content hash; do not silently rewrite imported content.
- Treat all body text, references, examples and scripts as untrusted data. Initial imported packages are **instructions-only**: no scripts, no arbitrary commands, no network or filesystem authority.
- A future TOML/pipeline compatibility mode may be added only for a safe, typed subset; arbitrary template execution is not part of the first release.

### Lifecycle and trust

- Quarantine/imported → inspected → user-reviewed → installed (disabled) → enabled → loaded → run.
- Installation never enables, loads, executes or grants permissions.
- Keep publisher, source URL, pinned commit/tag, version, licence, content hash, import date and scanner findings.
- Trust labels describe review status, never absolute safety: FRIDAY maintained, reviewed, limited, unverified or blocked.
- Updates are explicit, show a diff and new requested capabilities, and require re-review/approval. No auto-update.
- Disable/uninstall revokes skill-owned grants and sessions; preserve audit and explain retained data.
- Imported skill content cannot alter FRIDAY policy, self-grant capabilities or override user consent.

### Tool execution bridge

- First supported execution stage: **first-party tools only**, selected by a trusted registry.
- Every call is checked in Rust against a backend-owned tool ID → capability mapping and current user grant (exact capability + exact resource + expiry).
- Unknown tools, undeclared capabilities, missing grants, mismatched resources, disabled skills and invalid arguments fail closed.
- Consequential actions use the existing one-use approval model and a fresh user-visible preview.
- Bound argument bytes, output bytes, step count, nesting depth, total runtime, concurrency and cancellation.
- Log skill ID/version/hash, tool ID, capability/resource category, approval/grant reference and outcome; never log sensitive content by default.
- External executable skills are out of scope until a separate OS-enforced process isolation and resource-containment design passes adversarial Windows and Linux tests.

## 5. Licence handling

The inspected OpenJarvis repository root carries Apache License 2.0. If FRIDAY copies or adapts any upstream source:
- include a copy of the Apache-2.0 licence with redistributed copies;
- retain applicable copyright, patent, trademark and attribution notices;
- mark modified upstream files prominently;
- include applicable NOTICE attributions if present in the upstream distribution;
- record exact source commit and copied-file provenance in a third-party notices/inventory record.

A package format or independently implemented idea does not by itself require copying upstream source. The first implementation should prefer clean Rust implementation against the documented compatible format; copy source only where a measurable benefit justifies the compliance and maintenance burden. Check all transitive dependencies and bundled assets separately.

## 6. Delivery order — large coherent batches

1. **Compatibility and import foundation:** strict instruction-only `SKILL.md` parser; bounded directory inspection; quarantine metadata; provenance/licence/hash; reject scripts and unsafe layouts; unit/adversarial tests on Linux and Windows.
2. **Trust review UX:** show source, publisher, revision, licence, hash, declared capabilities, scanner limitations and review status; require explicit user action to install disabled.
3. **Permissioned tool bridge:** trusted Rust registry, typed schemas, per-invocation authorization and exact grants; connect one small first-party tool end-to-end.
4. **First usable official skill:** choose a bounded skill with immediate user value (Excel Intelligence should be prioritized once suitable local workbook libraries and file approval boundaries are verified).
5. **Community import expansion:** GitHub/catalogue sources, dependency inventory, update diff/rollback and reviewed capability subsets. Executable scripts remain disabled.
6. **Executable extensions:** separate future project; no release until OS-enforced isolation, containment, cancellation and adversarial acceptance are proven.

**Immediate next implementation batch:** implement the strict instructions-only import/inspection foundation and add regression tests. Do not yet wire imported skills into tool execution. This creates useful compatibility without granting imported content authority.

## 7. Acceptance criteria

- Valid supported `SKILL.md` packages parse into a typed, bounded manifest.
- Malformed frontmatter, duplicate keys, invalid IDs, path traversal, symlink/reparse paths, oversized content and unsupported executable content are rejected.
- Import leaves packages quarantined and inert; no install, enable, load, tool execution, network request or permission grant occurs implicitly.
- Content hash and source/licence metadata are persisted and verified on subsequent inspection.
- Tampering after import is detected; unknown/missing trust metadata is shown as unverified, not trusted.
- Tests prove imported instructions cannot create tools, invoke commands, broaden grants or bypass the backend permission gateway.
- Linux and Windows CI pass; physical Windows filesystem behavior is separately accepted before claiming complete reparse-point protection.

## 8. Upstream references

- [OpenJarvis repository](https://github.com/open-jarvis/OpenJarvis)
- [Pinned upstream revision](https://github.com/open-jarvis/OpenJarvis/commit/5a9b5268f2432f7ea4fe402e384ecb6b64a649bb)
- [Skill manager](https://github.com/open-jarvis/OpenJarvis/blob/5a9b5268f2432f7ea4fe402e384ecb6b64a649bb/src/openjarvis/skills/manager.py)
- [Skill loader](https://github.com/open-jarvis/OpenJarvis/blob/5a9b5268f2432f7ea4fe402e384ecb6b64a649bb/src/openjarvis/skills/loader.py)
- [Skill executor](https://github.com/open-jarvis/OpenJarvis/blob/5a9b5268f2432f7ea4fe402e384ecb6b64a649bb/src/openjarvis/skills/executor.py)
- [Tool executor](https://github.com/open-jarvis/OpenJarvis/blob/5a9b5268f2432f7ea4fe402e384ecb6b64a649bb/src/openjarvis/tools/_stubs.py)
- [Workflow engine](https://github.com/open-jarvis/OpenJarvis/blob/5a9b5268f2432f7ea4fe402e384ecb6b64a649bb/src/openjarvis/workflow/engine.py)
- [Apache License 2.0](https://github.com/open-jarvis/OpenJarvis/blob/5a9b5268f2432f7ea4fe402e384ecb6b64a649bb/LICENSE)


## 9. First importer implementation — committed, CI pending

The first implementation increment is now present in FRIDAY:
- `src-tauri/src/skills.rs`: strict bounded frontmatter parser and `inspect_and_quarantine_skill` command.
- `src-tauri/src/lib.rs`: Tauri command registration.
- `src-tauri/src/permissions.rs`: explicit deny-by-default IPC policy entry for `skills.import.inspect`.
- `src/SkillsManager.tsx`: Import community skill action and quarantine inspection preview.
- `src/styles.css`: visible unverified/quarantine review treatment.
- Unit tests cover valid supported metadata, duplicate/unknown fields, nested YAML/anchors, invalid names, missing delimiters, empty body and oversized content.

Current supported format is intentionally narrower than the full Agent Skills metadata surface: one folder containing exactly one `SKILL.md`; frontmatter accepts only `name`, `description`, optional `license` and optional `compatibility`. Nested `metadata`, scripts, assets, dependencies, and additional files are rejected rather than ignored.

The importer stores the original content and inspection metadata under FRIDAY's local `skills/quarantine/<sha256>/` directory. It does not install, enable, load, execute, add instructions to session memory, grant permissions, access the network or invoke tools. The returned status is unverified; a SHA-256 hash establishes content identity, not publisher identity or benign behavior.

**Known limitations / next hardening:**
- The selected folder and SKILL.md are checked with filesystem metadata before reading, but this first increment does not yet provide a handle-bound no-follow read against every same-user replacement race. Treat this as an import preview/quarantine foundation, not a completed adversarial filesystem boundary.
- Physical Windows reparse-point behavior has not been accepted on a real Windows device.
- Licence text is self-declared and not independently verified; publisher, source repository, pinned commit and signatures are not yet collected.
- Quarantined content has no install/approve path and cannot be run. The next batch should harden source reads, add quarantine inventory/reinspection/tamper detection and build the user trust review before any installation transition.
- Linux/Windows CI for commits `ea60ec5` through `84886de` is pending at the time this note was written. Do not mark the importer complete until the latest coherent batch passes.
