# Coding Assistant Completion Master Checklist

> **Purpose:** durable hand-off and completion inventory. This file is the working source of truth for finishing Coding Assistant even if a development conversation is lost.
>
> **Audit baseline:** branch `feature/phase-2-skills`, commit `108a5573bb3343017658c826f3d682aeb845b3c0` (#988), 2026-11-18.
>
> **Important:** this is a static repository audit of the code, UI, command registration, tests, and design documents at the baseline above. CI success is evidence that code builds/tests in those runners; it is not proof of physical Windows Sandbox behavior. Items marked `NEEDS PHYSICAL WINDOWS ACCEPTANCE` must not be called complete based on CI alone.

## 1. Executive status

Coding Assistant already has substantial working foundations:

- User-selected project discovery and bounded source-file reading.
- Local project-structure analysis (entry points, symbols, imports, dependencies and review hints).
- Local Ollama-backed coding assistance/proposal generation in the UI.
- One-file change previews, exact approval, guarded backup/write, hash verification, undo and transaction-recovery code.
- Snapshot preparation, read-only Sandbox configuration preview, host capability diagnostics and conservative workspace cleanup.
- Extensive pure validation and Windows Job Object / storage / lifecycle primitives with regression coverage.

**It is not a finished, execution-capable coding agent.** The critical Stage 4 guest-runner integration is still deliberately disabled. Several strong building blocks exist as independent validators, plans, journals and Windows primitives, but are not connected into one production launch → execute → stop → collect → clean-up transaction. Physical Windows acceptance of the result VHDX and Sandbox behavior is still outstanding.

## 2. Repository scope inspected

Primary files at the audit baseline:

- `src/CodingAssistant.tsx` — Coding Assistant screen and user workflow.
- `src-tauri/src/coding_assistant.rs` — project scanning, grants, read operations, edit previews, approval, apply, undo and recovery.
- `src-tauri/src/coding_intelligence.rs` — deterministic structural analysis.
- `src-tauri/src/coding_sandbox.rs` — guest request/result contracts, runtime provisioning and verification, snapshots, Sandbox configuration, result VHDX/NTFS/GPT storage, journals, session ownership and preflight.
- `src-tauri/src/coding_sandbox_fs.rs` — filesystem boundary helpers.
- `src-tauri/src/coding_sandbox_job.rs` — Windows Job Object and process supervision primitives.
- `src-tauri/src/coding_sandbox_resources.rs` — adaptive memory policy.
- `src-tauri/src/lib.rs` — Tauri module and command registration.
- `src-tauri/src/skills.rs`, `src/SkillsManager.tsx` — skill catalogue and skill lifecycle.
- `docs/CODING-ASSISTANT-STAGE-3-SAFETY-DESIGN.md`
- `docs/CODING-ASSISTANT-SANDBOX-DESIGN.md`
- `docs/CODING-ASSISTANT-GUEST-RUNNER-DESIGN.md`
- `docs/CODING-ASSISTANT-RESOURCE-PROFILES.md`

The main guest-runner module is large (about 19,226 lines at this baseline) and contains hundreds of tests. Its size and test count are not a completion metric; the integration and real Windows gates below remain authoritative.

## 3. Status definitions

- **IMPLEMENTED — CI COVERED:** code and regression coverage exist. Still subject to normal release validation.
- **IMPLEMENTED — INTEGRATION PENDING:** useful backend primitives exist, but are not wired into the end-to-end product flow.
- **PARTIAL:** some functionality exists; required security or lifecycle work remains.
- **NOT IMPLEMENTED:** no acceptable implementation has been verified.
- **NEEDS PHYSICAL WINDOWS ACCEPTANCE:** cannot be closed by mocked, synthetic, Linux, or Windows-hosted CI tests alone.
- **DOCUMENTATION UPDATE REQUIRED:** implementation has moved beyond or differs from the older design note.

## 4. Existing Coding Assistant capabilities

| Area | Baseline status | Notes |
|---|---|---|
| Skill install/enable/load/unload and package integrity | IMPLEMENTED — CI COVERED | Skill packages are declarative instructions. Loading a skill does not itself connect tools. |
| Select a project folder and scan eligible source/configuration files | IMPLEMENTED — CI COVERED | Uses a user-selected project root and bounded allowlists/exclusions. |
| Read explicitly selected project files | IMPLEMENTED — CI COVERED | Current skill contract: up to 25 files, 128 KiB each, 180 KiB combined; only selected text is sent to configured local Ollama for analysis. |
| Project structural intelligence | IMPLEMENTED — CI COVERED | Heuristic entry-point, symbol, import, dependency and review-hint extraction; not a compiler or full static analyser. |
| Local Ollama coding chat/proposal generation | IMPLEMENTED — UI WIRED | Model output is an untrusted proposal; backend validation and per-file approval remain authoritative. |
| Single-file diff preview | IMPLEMENTED — CI COVERED | Bounded content, original/proposed hashes, expiry and explicit preview cancellation. |
| Exact one-use approval | IMPLEMENTED — CI COVERED | Approval is bound to the specific preview and is not a general permission. |
| Guarded file apply and verified backup | IMPLEMENTED — CI COVERED | Revalidates grant/path/type/hash; retains a verified backup and verifies installed content. |
| Guarded undo | IMPLEMENTED — CI COVERED | Refuses if the current file changed or backup is missing/corrupt; preserves recovery evidence. |
| Interrupted edit transaction recovery | IMPLEMENTED — CI COVERED | Recovery is bounded to the project explicitly selected by the user; ambiguous states preserve evidence. |
| Multi-file proposal validation | IMPLEMENTED — CI COVERED | Proposal can be validated against inspected paths, but each file must go through its own preview/approval/apply flow. |
| Execute project code/build/test commands | NOT ENABLED | No host execution fallback. Guest execution remains disabled until every Stage 4 gate passes. |

**Documentation correction:** the Stage 3 design document contains older statements saying apply/undo are backend-only, hidden from the UI, or awaiting recovery/CI. The implementation and current `CodingAssistant.tsx` have advanced beyond those statements. Update the Stage 3 document to reflect verified current behavior and link this master checklist; do not use its stale progress paragraphs as the current status.

## 5. Stage 4 guest-runner inventory

### 5.1 Request, operation and result contracts

| Area | Baseline status | Notes |
|---|---|---|
| Strict bounded guest request parser | IMPLEMENTED — INTEGRATION PENDING | Fixed operation allowlist, identifiers, schema and resource-limit validation. Not connected to a production guest runner. |
| Fixed operation-to-command mapping | IMPLEMENTED — INTEGRATION PENDING | No model-generated shell command or arbitrary executable/argument vector. |
| Guest runtime launch profiles and explicit environment | IMPLEMENTED — INTEGRATION PENDING | Pinned paths and fixed arguments are validated as plans; not launched in a real Sandbox transaction. |
| Runtime manifest, artifact digest and tree verification | IMPLEMENTED — INTEGRATION PENDING | Authentication and exact-tree validation primitives exist. |
| Runtime staging, publication and cleanup journals | IMPLEMENTED — INTEGRATION PENDING | Strong host-side staging/publish/recovery helpers exist; complete production bootstrap transaction remains unconnected. |
| Strict bounded result schema and identity validation | IMPLEMENTED — INTEGRATION PENDING | Run/snapshot identity, operation, status/exit consistency, time/output caps and unknown/duplicate fields are checked. |
| Safe result-file reader | IMPLEMENTED — INTEGRATION PENDING | Bounded regular-file and directory checks exist; must be reached only through the complete verified transfer/stop boundary. |

### 5.2 Snapshot and filesystem boundary

| Area | Baseline status | Notes |
|---|---|---|
| Private per-run snapshot with allowlisted files | IMPLEMENTED — CI COVERED | Does not execute code or modify the original project. |
| Snapshot ownership marker and conservative recovery | IMPLEMENTED — CI COVERED | Unverified/malformed entries are preserved, not deleted. Marker is accidental-ownership evidence, not cryptographic protection against same-user malware. |
| Workspace ACL/private-directory handling | PARTIAL | Windows ACL hardening exists. Complete parent-directory race resistance and handle-relative traversal must be reviewed and proven. |
| Handle-relative recursive traversal | NOT COMPLETE | Replace path-only recursive source traversal with directory-handle-relative traversal or fail closed. Revalidate parent/root identity to prevent path replacement races. |
| Windows reparse point/junction race resistance | PARTIAL | Static symlink/reparse checks exist; adversarial replacement between checks and use needs a complete handle-based boundary and tests. |
| Snapshot lifecycle across active sessions, restart and shutdown | PARTIAL | Must never delete a snapshot mounted by an active FRIDAY-owned Sandbox; foreign or ambiguous ownership must block cleanup. |

### 5.3 Windows Sandbox configuration and lifecycle

| Area | Baseline status | Notes |
|---|---|---|
| Strict network/device-isolated .wsb configuration builder | IMPLEMENTED — CI COVERED | Network, vGPU, clipboard and device redirection are disabled in the validated template. |
| Read-only project snapshot mapping | IMPLEMENTED — CI COVERED | Original project is never mapped. |
| Separate result-folder mapping policy | IMPLEMENTED — CONFIG/VALIDATION ONLY | Two-folder policy and XML validation exist. This does not prove a safe guest-to-host transfer mechanism. |
| Windows Sandbox capability diagnostics | IMPLEMENTED — CI COVERED | Read-only best-effort preflight; unknown means unverified, not ready. |
| CLI identity/signature/session inventory checks | IMPLEMENTED — INTEGRATION PENDING | Validates supported CLI, publisher evidence, inventory and run-bound session identifiers. |
| Durable launch/stop intent and ownership receipts | IMPLEMENTED — INTEGRATION PENDING | Journals and authorization/observation proofs exist. |
| Actual production `wsb start` / `wsb stop` transaction | NOT CONNECTED | No end-to-end production invocation is wired to Coding Assistant. Do not mistake launch/stop planning helpers for a working launcher. |
| Authoritative FRIDAY ownership of exact Sandbox session | PARTIAL | Causal start evidence and exact stop proof are designed/validated; native lifecycle integration and physical acceptance remain. |
| Crash, cancellation, timeout, app restart and foreign-session behavior | PARTIAL | Conservative recovery assessment exists; physical lifecycle behavior and production transaction recovery remain. Never stop a session FRIDAY cannot prove it owns. |

### 5.4 Result storage and transfer

| Area | Baseline status | Notes |
|---|---|---|
| Fixed-capacity result VHDX creation and identity | IMPLEMENTED — CI COVERED / PHYSICAL ACCEPTANCE PENDING | Native VHDX APIs, run-bound ownership markers, fixed capacity and identity checks exist. |
| GPT metadata construction and independent read-back validation | IMPLEMENTED — CI COVERED | Extensive malformed-identity, geometry and byte-level regression coverage through #988. |
| NTFS format report, geometry and boot-sector validation | IMPLEMENTED — CI COVERED | Extensive synthetic/independent byte and identity checks. |
| VHDX attach/mount/unmount/detach lifecycle helpers | IMPLEMENTED — INTEGRATION PENDING | Journaling, ownership and restoration checks exist. Full supported-Windows physical behavior is not established. |
| Guest write access to mapped VHDX-backed result directory | NEEDS PHYSICAL WINDOWS ACCEPTANCE | Must prove actual Windows Sandbox mapped-folder semantics, effective identity and that writes cannot escape the bounded volume. |
| Hard output/disk-full enforcement | PARTIAL / PHYSICAL ACCEPTANCE PENDING | Fixed-capacity storage design exists; prove guest writes are actually constrained and disk-full is safely handled on supported Windows builds. |
| Authenticated bounded guest result-transfer channel | NOT COMPLETE | There is no verified production transfer mechanism yet. Ordinary writable mapped folders, clipboard, `wsb exec`, and unprovisioned custom socket approaches are not acceptable substitutes. |
| Result collection after verified Sandbox stop | IMPLEMENTED — GATE ONLY | Collection validators require positive stop evidence; production transfer and lifecycle must supply trustworthy evidence. |

### 5.5 Process/resource containment

| Area | Baseline status | Notes |
|---|---|---|
| Windows Job Object creation/configuration/read-back | IMPLEMENTED — CI COVERED | Kill-on-job-close, no breakaway, process count, CPU and memory controls are represented and tested. |
| Suspended process creation and assignment before resume | IMPLEMENTED — PRIMITIVE / INTEGRATION PENDING | Must be wired into a verified production guest launch path; never resume untrusted code before all limits and assignment are confirmed. |
| CPU hard cap | IMPLEMENTED — WINDOWS CI COVERED | 50% Job Object CPU hard cap. Physical device acceptance still needed. |
| Active process ceiling | IMPLEMENTED — WINDOWS CI COVERED | 64 active processes across the contained process tree. |
| Adaptive memory profiles | IMPLEMENTED — WINDOWS CI COVERED | Conservative 4/8/16 GB host tiers; live available-memory reserve and absolute ceilings. This is a policy ceiling, not a performance guarantee. |
| Wall-clock deadline, timeout and cancellation supervision | IMPLEMENTED — PRIMITIVE / INTEGRATION PENDING | Job supervision exists; connect it to the exact owned guest process/session transaction. |
| Hard storage/output boundary | PARTIAL | Bounded output readers and fixed result volume exist; prove end-to-end guest enforcement and failure behavior physically. |

## 6. Required work — ordered master checklist

Do these in dependency order. Do not enable guest execution as a shortcut.

### A. Close the filesystem trust boundary

- [ ] Review `coding_sandbox_fs.rs` and all snapshot traversal call sites as one security boundary.
- [ ] Replace path-only recursive traversal with handle-relative traversal on supported platforms, or fail closed where the guarantee is unavailable.
- [ ] Bind every opened child to the expected parent/root identity; defend against parent replacement and check/use races.
- [ ] Prove Windows reparse points, junctions, symlinks, mount points and redirected parents cannot escape the selected snapshot root.
- [ ] Add adversarial tests that swap/replace parent and child paths between enumeration, open, validation and cleanup.
- [ ] Confirm snapshot ownership, ACLs, active-session state and recovery cannot delete a live or foreign resource.
- [ ] Document the precise same-user malware limitations; do not overstate ownership markers as cryptographic security.

### B. Prove result storage on real Windows

- [ ] Test VHDX creation, fixed capacity, GPT/NTFS formatting, attach, mount, read-back, unmount and detach on supported Windows Pro/Enterprise/Education systems.
- [ ] Verify required privileges without silently elevating, changing privileges, installing optional features or weakening policy.
- [ ] Prove the Sandbox mapped-folder guest identity can write only inside the VHDX-backed result volume.
- [ ] Test full-volume/disk-full behavior, maximum file count/size, output floods, malformed files and reparse-point attempts.
- [ ] Test host crash, app crash, guest crash, interruption during every storage journal transition and restart recovery.
- [ ] Verify the original mount directory and every ownership/root identity after teardown.
- [ ] If any supported host cannot prove the boundary, refuse guest execution on that host.

### C. Finish the trusted result-transfer design

- [ ] Select a supported, non-elevated transfer mechanism whose access semantics are verified on physical Windows.
- [ ] Ensure no arbitrary writable host path is exposed to the guest.
- [ ] Authenticate each run and bind output to the exact run ID, snapshot ID, operation, storage identity and expected result contract.
- [ ] Enforce bounded disk capacity, file count, per-file bytes, total output bytes, stdout/stderr retention and wall-clock duration.
- [ ] Ensure malformed, duplicate, missing, unexpected, linked, reparse, oversized or substituted output is rejected.
- [ ] Ensure network isolation is enforced by the host configuration and verified; never trust guest assertions.
- [ ] Connect the existing result-file reader/validator only after the transfer and stop-evidence boundaries are real.

### D. Connect production Sandbox lifecycle

- [ ] Implement one serialized production transaction for preflight → reserve resources → stage config/runtime → create result storage → persist launch intent → invoke exact supported `wsb start` → prove ownership → run only fixed guest operation → supervise → persist stop intent → invoke exact `wsb stop` → prove stopped → revoke share → collect/validate → unmount/detach → cleanup.
- [ ] Bind every state transition to the same durable run journal and exact FRIDAY-owned session.
- [ ] Refuse pre-existing/foreign/ambiguous Sandbox sessions; never attach to or stop unrelated sessions.
- [ ] Handle PID reuse, process replacement, CLI output ambiguity, duplicate invocation and concurrent starts.
- [ ] Make cancellation, timeout, shutdown, app restart and crash conservative: no automatic retry or cleanup where ownership/state is ambiguous.
- [ ] Keep guest execution disabled until the entire transaction is connected and tested.

### E. Connect fixed runtime and Job Object containment

- [ ] Connect authenticated runtime provisioning/publication to the production guest transaction; verify the exact installed tree immediately before launch.
- [ ] Create the guest process suspended with fixed executable and argument vectors; do not use model-generated shell text.
- [ ] Create/configure/read back the Job Object, apply live adaptive memory and fixed CPU/process limits, assign the suspended process, then and only then resume.
- [ ] Verify kill-on-job-close, no breakaway, descendant containment, output draining and cancellation/timeout in the actual launch path.
- [ ] Fail closed on any missing runtime artifact, policy mismatch, unsupported Windows build, memory-query failure or Job Object configuration/assignment failure.
- [ ] Never fall back to host execution or network package installation.

### F. End-to-end adversarial and acceptance testing

- [ ] Malicious repository and prompt-injection fixtures; repository content must never become instructions or permissions.
- [ ] Path traversal, symlink/reparse/junction swaps and parent-directory race fixtures.
- [ ] Runtime manifest tampering, missing/unlisted artifacts, wrong run/snapshot IDs and stale journal/receipt fixtures.
- [ ] Output floods, oversized files, disk-full, malformed JSON, duplicate fields, unexpected files and UTF-8 byte-limit tests.
- [ ] Foreign Sandbox, concurrent FRIDAY launch, PID reuse, cancellation, timeout, guest crash, app crash, host restart and interrupted cleanup tests.
- [ ] Supported Windows edition/build matrix and hardware/virtualization prerequisite checks.
- [ ] Hands-on physical Windows acceptance of the complete launch → run → stop → transfer → validate → cleanup flow.
- [ ] Record tested Windows versions, device RAM tiers, policy/privilege prerequisites, observed limits and known unsupported configurations.
- [ ] Final security review confirms no host execution path, no arbitrary shell, no network access, no original-project mapping and no unbounded writable host path.

### G. Release and maintenance

- [ ] Update the Stage 3 and Stage 4 design documents so they match the implementation and this checklist.
- [ ] Add a concise user-facing explanation of read-only analysis, approved edits, and the separate disabled guest-execution feature.
- [ ] Verify skill enable/load/unload revokes project grants and outstanding approvals/previews.
- [ ] Run complete Linux and Windows CI after each substantial batch.
- [ ] Run regression tests for all prior security boundaries; do not remove earlier coverage to make a batch pass.
- [ ] Update this checklist in every future Coding Assistant batch: mark only evidence-backed items complete and record commit/run IDs.

## 7. Execution-disabled invariant

Until sections 6A–6F are completed and accepted, all of the following remain mandatory:

- No project code execution on the host.
- No arbitrary shell, model-selected executable, command string or arguments.
- No original project folder mounted writable or mapped into Windows Sandbox.
- No guest execution when network isolation, resource limits, storage limits, ownership or transfer cannot be proven.
- No stopping or deleting resources that FRIDAY cannot prove it owns.
- No automatic retry/cleanup after ambiguous crash or interruption.
- No claim of end-to-end sandbox security based only on unit tests or CI.

## 8. Next implementation focus

The next batch must be selected only after the filesystem-boundary review in section 6A is complete. The earlier proposed “result-transfer integration” must not jump ahead of the filesystem and physical storage prerequisites. The safe next step is to audit and harden handle-relative snapshot traversal and parent identity checks while keeping execution disabled.

## 9. Handoff instructions for a new development conversation

1. Open this file first.
2. Confirm the branch and current commit; do not assume the baseline commit is still HEAD.
3. Read the latest changes to the files listed in section 2.
4. Find the first unchecked item in section 6 whose dependencies are complete.
5. Implement one coherent batch, add regression tests, run Linux and Windows CI, and update this checklist with the resulting commit/workflow run.
6. Never ask the user to choose routine engineering details. The developer/agent owns implementation decisions; ask only when a product/security decision genuinely requires the user's choice.
7. Never mark physical Windows acceptance complete unless it was actually performed on a supported physical Windows machine.

---

**Baseline #988:** commit `108a5573bb3343017658c826f3d682aeb845b3c0`; workflow run `44313803884`; Linux and Windows jobs completed successfully. The #988 change itself added GPT trusted-identity negative-test coverage and did not enable guest execution.
