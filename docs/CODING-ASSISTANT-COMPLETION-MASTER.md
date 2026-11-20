# Coding Assistant Completion Master Checklist

> **Purpose:** durable hand-off and completion inventory. This file is the working source of truth for finishing Coding Assistant even if a development conversation is lost.
>
> **Latest verified baseline:** branch `feature/phase-2-skills`, commit `ea72fe4aabf473a7e0fbd91131f8f7c5d4bc9c42` (#1051), 2026-11-20. GitHub Actions run [#1051](https://github.com/Friday-AI-Personal-Assistant/FRIDAY/actions/runs/44740178481) completed successfully.
>
> **Important:** this checklist distinguishes CI-covered implementation from production integration and physical Windows acceptance. CI success proves only the paths exercised by those runners; it does not prove physical Windows Sandbox behavior or end-to-end isolation. Never mark physical acceptance complete based on CI alone.

## 1. Executive status

**Current verified repository position:** #1051 is the branch HEAD and passed CI. The checklist's historical detail below has been reconciled through #1051; the work is not complete. The strongest recent progress is in filesystem identity, bounded traversal, snapshot cleanup, project discovery/read validation and guarded file-edit/rename operations. Stage 4 guest execution remains disabled.


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
| Handle-relative recursive traversal | PARTIAL — CI COVERED PRIMITIVES | Bounded handle-relative traversal, identity binding/revalidation and rename primitives have advanced through #1051. A remaining Unix source-name substitution window exists between final identity revalidation and the rename syscall; do not claim the filesystem boundary is closed. |
| Windows reparse point/junction race resistance | PARTIAL — MORE ACCEPTANCE REQUIRED | Identity-aware traversal and cleanup protections have expanded; continue adversarial parent/child replacement, reparse/junction and cleanup testing across the complete production call graph. |
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

Continue Stage 4A before any result-transfer or guest-runner integration. The immediate engineering focus is the approved-file-edit filesystem transaction: inspect backup creation, temporary-file creation, replacement, rollback, undo and recovery as one boundary; anchor operations to verified directory/file identities where supported; preserve no-overwrite and crash-recovery guarantees; and add adversarial Linux/Windows regression coverage. Explicitly resolve or constrain the remaining Unix source-name substitution race before describing the rename boundary as complete. Keep guest execution disabled throughout.

## 9. Handoff instructions for a new development conversation

1. Open this file and the [FRIDAY Master Roadmap](FRIDAY-MASTER-ROADMAP-AND-COMPLETION-CHECKLIST.md).
2. Confirm the active branch, latest commit and latest GitHub Actions result; never assume this document's baseline is still HEAD.
3. Read the latest relevant implementation, tests and design documents before editing.
4. Work only on Coding Assistant and its directly required dependencies; do not start unrelated FRIDAY roadmap features.
5. Select the first dependency-ready unchecked task in section 6 and implement one coherent, reviewable batch.
6. Add regression tests, run the relevant Linux and Windows CI jobs, diagnose/fix failures and rerun CI.
7. Update this checklist with verified changes, commit SHA, workflow run URL, remaining limitations and the next task.
8. Do not ask the product owner to approve routine engineering choices or to say “continue” after a successful batch. Ask only for a genuinely necessary product/security decision.
9. Never mark physical Windows acceptance complete unless it was actually performed on a supported physical Windows machine.
10. Never enable project-code execution until every required security gate has been completed and accepted.

---

## 10. Verified progress through #1051

This section supersedes the stale #988/#1012 hand-off status that was previously at the end of this file. The historical commits are retained in Git history. The following are verified implementation areas, not a declaration that the complete Stage 4A boundary is finished.

### Completed with CI evidence

- [x] **#1007 — source hard-link rejection.** Reject multiply-linked snapshot source files and verify the external hard-link alias remains unchanged. Commit `34b15036801ff13addc473252809cbb5d4c613e5`; run [#1007](https://github.com/Friday-AI-Personal-Assistant/FRIDAY/actions/runs/44555315714).
- [x] **#1008 — hard-link race coverage.** Deterministically create a second hard link between enumeration and open; require fail-closed behavior. Commit `e49eb5160d5ddc713f31ca14844db55a6e421184`; run [#1008](https://github.com/Friday-AI-Personal-Assistant/FRIDAY/actions/runs/44557584318).
- [x] **#1009–#1010 — bounded snapshot directory enumeration and compile correction.** Count ignored/excluded entries and reject incomplete scans. #1010 Linux and Windows CI passed. Commits `18b624d47135540689b30f86daa90c50dc63227e` and `6cd8ca12d3725f15fff402e3a592a3be02f84d61`; run [#1010](https://github.com/Friday-AI-Personal-Assistant/FRIDAY/actions/runs/44561234862).
- [x] **#1011 — bounded candidate traversal.** Shared non-zero entry budget across recursive candidate traversal, including ignored names and links. Commit `a7e396f5b961e8a80c083dfe5ea8a8f7d1a14e08`; run [#1011](https://github.com/Friday-AI-Personal-Assistant/FRIDAY/actions/runs/44563160955).
- [x] **#1012 — bounded snapshot inventory and cleanup preflight.** The implementation commit `6be07d2a290a2fb08b677df0f3e110e217b6afb6` is present in the branch history. Later recovery and cleanup work builds on this bound.
- [x] **Recovery and cleanup hardening after #1012.** Added bounded workspace recovery inventory, handle/identity-aware cleanup, retention of snapshot identities through cleanup and regression fixes for Windows handle-sharing/cleanup behavior. Relevant commits include `1de880561a64af0cca2d47fe2c8480f9700a38e3`, `a991e58d5bff3ce876a13be4b422793b9e4bee7b`, `a4b7bb3873e2db2ea28e38a1c3f80c0c82c51d45`, `f31cde0f73f983c46bc606d49bab107e1e29e54c` and subsequent cleanup regression corrections through #1044.
- [x] **Runtime inventory identity hardening.** Runtime tree inventory binds entries to opened/native file identities and includes Windows metadata compatibility corrections. Relevant commits: `c171950db998884f6a9660e527d7291c9392d859`, `68aa13baa4a99336b140328c788769bc682990dd`, `5a2a30261007956cf6f28ed7196ba8ffd44d4f46`.
- [x] **Snapshot source identity hardening.** Bind snapshot source reads to enumerated identities and reject source identity substitution. Relevant commits: `558aa953db55e32e4fed84bca71840b3cca8aa9a`, `dd3a0cbd2cc77b928995542e759f0fa908ee4cad`.
- [x] **Project discovery and read identity hardening.** Bind discovery scans to directory identities, release root handles safely, bind project reads to scanned identities and revalidate identities before code-edit application. Commits include `c6f4d89f9144c31400ac6c67e7ab04f84c8f80ec`, `bae9d573150b8a27887a449c624943d04f670d3e`, `7d1bf6fa59b3db9e653a4644f7bb941cabd0cc61` and #1048 `c56d99faca6c793ec4d86252312767fa5a7b0a18`.
- [x] **#1049–#1050 — handle-relative no-replace rename primitive and regression coverage.** Added a handle-relative rename primitive and tests for verified-file movement without overwriting an existing destination or accepting a changed parent identity. Commits `2e89ca0bd6b8dee9bd0c0f7a59206b332aff56fb` and `9a2df5e875d0b892682908188935e78f200bee6f`.
- [x] **#1051 — revalidate source identity immediately before rename mutation.** Added immediate source-name reopen and identity checks plus a source-replacement regression. Commit `ea72fe4aabf473a7e0fbd91131f8f7c5d4bc9c42`; [GitHub Actions #1051 passed](https://github.com/Friday-AI-Personal-Assistant/FRIDAY/actions/runs/44740178481).

### Remaining work — do not mark complete

- [ ] Complete the filesystem trust-boundary review across snapshot traversal, project reads, approved edits, backups, temporary files, replacement, undo, rollback, recovery and cleanup.
- [ ] Close or explicitly constrain the remaining Unix source-name substitution race between final identity revalidation and the rename syscall. Do not describe the primitive as race-free.
- [ ] Harden backup/temp creation and the full approved-edit transaction using verified parent/file identities; preserve no-overwrite behavior, backups, undo and crash recovery.
- [ ] Finish Windows reparse point, junction, mount-point and parent/child replacement adversarial coverage across production call sites.
- [ ] Prove snapshot ownership/ACL/session protections cannot delete live or foreign resources.
- [ ] Complete all physical Windows result-storage acceptance in section 6B.
- [ ] Complete trusted result-transfer design and acceptance in section 6C.
- [ ] Connect and validate the production Sandbox lifecycle in section 6D.
- [ ] Connect fixed runtime and Job Object containment to the real launch path in section 6E.
- [ ] Complete end-to-end adversarial and physical Windows acceptance in section 6F.
- [ ] Complete documentation, skill revocation and release maintenance in section 6G.

### Next batch selection

Start with the approved-file-edit transaction boundary: inspect the current implementation in `coding_assistant.rs` and filesystem helpers in `coding_sandbox_fs.rs`; map every backup, temporary-file, replacement, rollback, undo and recovery operation; then implement the smallest security-coherent improvement with regression tests. Do not jump to result transfer or guest execution.

**Execution-disabled invariant:** all project-code execution remains disabled. CI success does not authorize enabling it and does not prove physical Sandbox isolation.
