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

## First implementation slice

Implement a single-file preview API and UI first, with no write path enabled. Add tests for diff generation, content limits, invalid paths, stale preview detection and preview expiry. Only after that passes CI should the approval token, backup, atomic apply and guarded undo be implemented and enabled.

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
