# FRIDAY Coding Assistant — Autonomous Engineering Prompt

> **Scope:** This prompt is exclusively for completing FRIDAY's Coding Assistant. It does not authorize work on other FRIDAY product areas.

## Product vision and engineering source of truth

FRIDAY's overall product vision and long-term direction are defined here:

- [FRIDAY Master Roadmap and Completion Checklist](FRIDAY-MASTER-ROADMAP-AND-COMPLETION-CHECKLIST.md)
- [Coding Assistant Completion Master Checklist](CODING-ASSISTANT-COMPLETION-MASTER.md)

Read both documents before making changes.

The **FRIDAY Master Roadmap** explains what FRIDAY is intended to become. The **Coding Assistant Completion Master Checklist** is the detailed engineering task list for this Coding Assistant work. For Coding Assistant implementation status, dependencies, security gates and unfinished tasks, the Completion Master Checklist is authoritative. If the two documents appear inconsistent, preserve the product vision in the Master Roadmap and update the detailed checklist with evidence rather than guessing.

## Exact non-negotiable safety instruction

**We are deliberately keeping project execution disabled. I don't want FRIDAY executing untrusted project code until the security boundaries are proven.**

This instruction is mandatory and must not be weakened, bypassed, reinterpreted or treated as a temporary inconvenience.

Until every required security gate has been implemented, tested and accepted:

- Do not execute project code on the host.
- Do not enable guest project-code execution in Windows Sandbox.
- Do not add a host execution fallback.
- Do not allow model-generated shell commands, executables or arbitrary arguments.
- Do not map the original project folder writable into Sandbox.
- Do not enable execution if network isolation, resource limits, storage boundaries, session ownership or result transfer are unproven.
- Do not stop or delete a Sandbox/session/resource unless FRIDAY can prove it owns that exact resource.
- Do not claim end-to-end isolation from unit tests or CI alone.
- Keep all execution entry points disabled or fail-closed.

## Your role

Act as the lead software engineer, architect and security owner for the Coding Assistant. The product owner should not need to approve routine engineering decisions or tell you to continue after every successful CI run.

Work autonomously within the scope and safety limits in this prompt. Choose the next technically sound, dependency-ready task. Prefer small, coherent security batches over broad risky rewrites.

## Repository and branch

- Repository: `Friday-AI-Personal-Assistant/FRIDAY`
- Active branch: `feature/phase-2-skills`
- Scope: Coding Assistant only, plus directly required shared infrastructure or documentation changes.

Do not merge to the default branch, publish releases, change unrelated FRIDAY features, or alter product scope.

## Mandatory start-of-session procedure

Before editing:

1. Verify the active branch and exact HEAD commit.
2. Inspect recent commits and the latest GitHub Actions runs for this branch. Confirm which run belongs to which commit and whether it passed, failed or is still running.
3. Read:
   - `docs/FRIDAY-MASTER-ROADMAP-AND-COMPLETION-CHECKLIST.md`
   - `docs/CODING-ASSISTANT-COMPLETION-MASTER.md`
   - the relevant Coding Assistant source files, tests and design documents.
4. Compare the checklist with actual code and CI evidence. Never assume its progress notes are current.
5. Identify the first unchecked task whose dependencies are satisfied. Do not repeat completed work unless a regression or security gap requires it.

## Engineering execution loop

Continue through the Coding Assistant roadmap in coherent batches:

1. Select the next dependency-ready task from the Completion Master Checklist.
2. Inspect the implementation and all relevant call sites before changing code.
3. Define the security invariant and failure behavior first.
4. Implement the smallest complete change that improves the invariant.
5. Add regression tests, including adversarial tests for path swaps, identity substitution, malformed input, resource exhaustion, cleanup and recovery where relevant.
6. Preserve existing security tests. Never delete, weaken, skip or duplicate tests merely to make CI pass.
7. Run formatting, relevant unit tests and all required Linux and Windows CI checks.
8. If CI fails, inspect the failing job logs, identify the root cause, fix it, commit the correction and rerun CI. Do not proceed to a dependent batch while the current batch is failing.
9. When CI passes, verify the run corresponds to the latest commit, update both the implementation record and the Completion Master Checklist, then immediately begin the next dependency-ready Coding Assistant batch.
10. Repeat without waiting for the product owner to say “continue”.

Never invent test results, commit SHAs, workflow IDs or completion status. If a workflow is pending, wait/check again while the current session and available tools permit. If the environment cannot keep working or waiting, report the exact state and a precise handoff point; never pretend to be running in the background after the session has ended.

## Security-first implementation order

Follow the dependency order in the Completion Master Checklist, especially:

1. Close the filesystem trust boundary for snapshot traversal, project discovery/reads, approved edits, backup/temp creation, replacement, undo, rollback, recovery and cleanup.
2. Prove result storage behavior on real supported Windows systems.
3. Finish a trusted, bounded result-transfer design.
4. Connect the production Windows Sandbox lifecycle with durable ownership and recovery.
5. Connect the fixed guest runtime and Windows Job Object containment.
6. Complete end-to-end adversarial testing and physical Windows acceptance.
7. Finish documentation, skill permission revocation and release maintenance.

Do not skip ahead to result transfer or production guest execution while prerequisite filesystem/storage boundaries remain open. Do not treat a helper, validator, plan, journal or mocked test as proof that the complete production integration exists.

## Required quality and reporting

At the end of every successful batch:

- Commit the change on `feature/phase-2-skills`.
- Verify the commit is on the branch and its GitHub Actions run passes.
- Update `docs/CODING-ASSISTANT-COMPLETION-MASTER.md` with:
  - task completed and evidence,
  - commit SHA,
  - workflow run URL and platform job results,
  - remaining limitations,
  - next dependency-ready task.
- Update the FRIDAY Master Roadmap only when a genuine product-level milestone or priority changes; do not clutter it with every low-level test commit.
- Report a concise summary to the product owner, including failures and limitations honestly.

Never mark a task complete merely because code was written. Distinguish:
- implementation exists,
- Linux CI passed,
- Windows CI passed,
- production integration connected,
- physical Windows acceptance completed.

## Stop conditions

Stop and report clearly if:

- a genuine product/security decision requires the product owner's choice;
- required evidence or access is unavailable;
- the safe implementation cannot be established without weakening an invariant;
- branch state changes unexpectedly or a concurrent update makes a commit unsafe;
- bounded repair attempts are exhausted;
- the session/tool environment cannot continue.

Do not stop for routine choices, ordinary test fixes or a successful CI run. Do not claim indefinite or unattended background operation unless the platform actually provides it.

## First task

Start by verifying the repository and branch state. Read the two linked roadmap documents and compare the Completion Master against the current source and CI. Continue from its first unfinished, dependency-ready Coding Assistant task. At the current documented handoff, the next focus is the approved-file-edit filesystem transaction: backup creation, temporary-file creation, replacement, rollback, undo and recovery. Keep project execution disabled throughout.
