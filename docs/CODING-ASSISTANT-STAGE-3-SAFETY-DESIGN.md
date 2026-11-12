# Coding Assistant — Stage 3 Safe Editing Design

Status: implementation in progress. No project-file write capability is enabled by this design document alone.

## Goals

Allow FRIDAY to propose and, only after explicit user approval, apply carefully bounded edits to files inside the currently selected Coding Assistant project. Preserve originals, detect stale previews, provide undo, and leave a local audit trail.

## Non-negotiable safety rules

1. Stage 1/2 project access remains read-only unless a separate Stage 3 edit operation is invoked.
2. A model response is an untrusted proposal. It never authorizes a write.
3. Show the complete proposed content and a readable unified diff before approval.
4. Approval is per file and per exact preview. Any content/path/hash change invalidates the approval.
5. Revalidate skill-loaded state, in-memory project grant, canonical root, canonical target path, regular-file status, reparse/symlink status, allowlisted extension, and size bounds immediately before backup and write.
6. Never write outside the selected project root. Never follow symlinks/reparse points. Do not create missing files in the first Stage 3 increment.
7. Never overwrite the original before a verified backup exists.
8. Use a same-directory temporary file, flush/sync it, then atomic replacement where the platform supports it. If atomic replacement cannot be guaranteed, fail closed.
9. Verify the resulting file hash and content after replacement. Preserve backup if post-write verification fails and report the recovery path.
10. Undo is allowed only when the current file hash still matches the hash produced by FRIDAY's edit. Never overwrite intervening user changes.
11. Record action metadata (relative path, hashes, byte counts, timestamp, outcome) in the existing local activity history; never record source contents or secrets.
12. No code execution, build/test command, shell invocation, network access, or automatic multi-file write is introduced by Stage 3.

## Planned operation sequence

### A. Prepare preview
- Input: one granted file ID and proposed UTF-8 content.
- Validate proposal bounds and reject binary/NUL content.
- Re-read the original through the existing project grant checks.
- Capture original SHA-256 and proposed SHA-256.
- Return original/proposed content, relative path, sizes, and a line-oriented unified diff.
- Store a short-lived opaque preview record in memory, bound to exact file grant, root, hashes, and content.

### B. Approve exact preview
- Display full diff and explicit per-file approval UI.
- Approval token is issued only for the exact preview ID, and is short-lived and one-use.
- Any edit to the proposal requires a new preview and approval.

### C. Apply
- Revalidate project grant and path immediately before write.
- Re-read and hash current file; require exact match with preview's original hash.
- Create a unique backup in a FRIDAY-controlled backup directory under the selected project, with restrictive collision handling.
- Verify backup hash equals original hash.
- Write a unique temporary file in the target directory, flush and sync, then replace atomically.
- Verify target hash equals proposed hash.
- Persist an undo record and append an audit event.
- If any step fails, do not silently continue; retain the backup and report exact state.

### D. Undo
- Require current target hash to equal the recorded post-edit hash.
- Verify backup hash equals original hash.
- Restore through a new temporary file and atomic replacement.
- Verify restored hash and mark the undo record consumed.
- Refuse undo if user or another process has changed the target since FRIDAY's edit.

## Implementation progress

- Preview-only slice: implemented and CI-verified in Run #279. It provides a one-file bounded preview, hashes, diff, short expiry and no disk writes.
- Approval groundwork and explicit UI: backend token issuance is wired to an explicit per-preview approval button. The token is held only in component state, approval is rotated if requested again, cancellation removes the pending preview, and editing the proposed content or changing the selected file invalidates the visible approval and cancels the backend preview. The backend consume helper rejects wrong-token, expiry and replay attempts. The UI explicitly states that approval does not change the file.
- No apply/write command is registered for Coding Assistant yet. The token is not a write authorization by itself; it must be consumed by a future apply command after full path/grant/hash/backup revalidation. The UI approval increment is awaiting CI verification.

## Remaining implementation sequence

1. Complete approval UI so the user can explicitly approve the exact displayed preview.
2. Implement a single-file apply operation that consumes the approval record, revalidates the current grant/root/path/type/reparse status and original SHA-256, and refuses stale files.
3. Add verified backup creation, safe same-directory temporary write and atomic replacement with fail-closed platform handling.
4. Verify post-write SHA-256, persist undo metadata and metadata-only audit events.
5. Implement guarded undo that refuses to overwrite intervening user changes. (Backend implementation added; CI verification pending.)
6. Add filesystem fault-injection and failure-path tests before enabling the write capability.

## Acceptance criteria

- Preview never changes the selected file.
- Per-file approval is explicit and bound to exact content/hash.
- Stale preview is rejected.
- Symlink/reparse and path traversal attempts are rejected.
- Backup integrity is verified before replacement.
- Write and undo are atomic or fail closed.
- Undo refuses to overwrite intervening edits.
- Audit entries contain metadata only.
- Cancellation, skill unload, project reselection and app restart invalidate outstanding previews/approvals.
- Rust unit tests cover every failure path; Windows CI build passes; hands-on Windows acceptance remains required.


## Safe apply backend progress

- A backend-only apply_code_change command is implemented and registered. It consumes the one-use approval token before proceeding, revalidates the loaded skill, active project grant, canonical root and file path, allowed file type, regular-file status, bounded UTF-8 content, and original SHA-256.
- Before replacement it creates a FRIDAY-controlled .friday-code-backups directory, enforces a 500-file / 100 MB storage ceiling, writes and syncs the original backup, then verifies the backup hash.
- It writes and syncs a same-directory temporary proposal, verifies its hash, stages the original aside, installs the proposal, verifies the installed hash, and attempts restoration on failed installation or verification. A transaction journal and persistent undo metadata are written.
- This command is intentionally not exposed in the Coding Assistant UI yet. The guarded undo command and fault-injection/recovery coverage must be completed and CI-verified before users can trigger writes.
- Platform caveat: replacement uses a journaled two-rename sequence with rollback rather than claiming a single universally atomic replace on every supported filesystem. A brief missing-target window may exist between renames; recovery must handle interruption there.
- New tests cover successful verified backup/undo metadata, stale-original refusal, and symlink-target refusal. These tests are awaiting CI.


## Guarded undo backend progress

- Added a backend-only `undo_code_change` command, registered with Tauri but not exposed in the UI.
- Undo requires the Coding Assistant to be loaded and the original project/file grant to still be active. The stored record is validated, its project-relative path is constrained, and the current file must still match the exact post-apply SHA-256.
- The original backup must be a normal bounded file and match the recorded original SHA-256. A corrupted/missing backup, redirected target, unsupported file, or intervening user edit causes a refusal without overwriting the target.
- Restoration uses a synced same-directory temporary file, a journal, a staged displaced copy, post-restore SHA-256 verification, and a consumed-record rename to prevent replay. The verified original backup is retained.
- Tests cover successful restore and replay refusal, intervening user changes, and corrupted backups. Startup recovery and fault-injection across interrupted rename/journal states remain outstanding; the command stays hidden from the UI until those are complete and CI-verified.
