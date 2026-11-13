# Coding Assistant Stage 4 — Trusted guest runner contract

Status: design only. No guest runner is installed or launched. Coding Assistant execution remains disabled.

## Security boundary and non-goals

The guest runner is trusted FRIDAY code. Project files, build scripts, test scripts, manifests, generated output, and all text returned by a local model are untrusted. The runner must never interpret project-provided instructions as authority to choose a host executable, command line, working directory, mount, or output path.

The runner is not a general-purpose terminal, shell, package manager, or arbitrary command bridge. It must not accept shell strings, executable paths, environment-variable expansions, PowerShell, CMD, batch files, scripts, or model-authored arguments. A request to run an unsupported project or test type must fail closed with a clear unsupported-runtime result.

The runner design does not authorise execution. A separate launch gate must prove that all host and guest isolation/resource controls are enforceable before any runner can be invoked.

## Request contract

The host may send only a versioned, bounded JSON request with these fields:

- `schemaVersion`: integer, exactly `1`.
- `runId`: generated 32-character hexadecimal identifier.
- `snapshotId`: generated 32-character hexadecimal identifier for a current-session-owned snapshot.
- `operation`: one enum value from the fixed operation list below.
- `timeoutSeconds`: selected by FRIDAY policy, never by the model; initial policy ceiling 120 seconds.
- `outputLimitBytes`: selected by FRIDAY policy; hard ceiling 1 MiB for the combined structured result and captured diagnostics.

Reject unknown fields, duplicate/invalid identifiers, unsupported schema versions, oversized requests, invalid enum values, and any caller-supplied executable, shell, arguments, environment, path, or network setting. The guest runner derives every path from its fixed installation directory and the validated run identifier.

## Initial operation allowlist

The initial allowlist is deliberately narrow and may be enabled only when a compatible, pre-provisioned toolchain is present in the disposable guest:

| Operation ID | Fixed action | Conditions |
| --- | --- | --- |
| `rust-cargo-test` | Invoke the runner's pinned Cargo executable with fixed `test --locked --offline` arguments | A reviewed Rust toolchain and all required dependencies are preloaded; project manifest is present; no build script or proc-macro execution is assumed safe merely because it is Cargo |
| `node-npm-test` | Invoke a pinned Node runtime and package-manager binary with a fixed test operation | A reviewed runtime and dependencies are preloaded; lockfile is present; scripts are not implicitly run; project lifecycle scripts are not allowed |
| `python-unittest` | Invoke a pinned Python runtime using a fixed unittest discovery operation | Standard-library-only mode initially; no pip, plugin loading, arbitrary test runner, or package installation |

These are design candidates, not currently supported or executable operations. In particular, Cargo tests can compile and execute build scripts, procedural macros and test code; Node package scripts can execute arbitrary code; and Python imports can execute module-level code. Therefore an operation ID is not itself a safety guarantee. Each operation needs a reviewed runtime profile and policy that explicitly accounts for code execution in the disposable guest. Unsupported or unprovisioned profiles must be rejected.

Never implement a generic `run-command`, `shell`, `powershell`, `cmd`, `npm-script`, `cargo-run`, or arbitrary-arguments operation.

## Guest process policy

When a future execution mode is approved:

1. The runner must validate the request before creating any process.
2. It must resolve only hard-coded absolute paths for pinned guest tools; it must not search PATH for project-controlled executables.
3. It must use direct process creation with an argument vector, never a shell.
4. It must set a minimal fixed environment and a fixed working directory beneath the read-only snapshot.
5. It must disable inherited handles and standard input; stdout/stderr must be drained concurrently with strict byte caps.
6. It must create the process inside a guest-side Windows Job Object configured before the process can execute, with kill-on-close, active-process limits, memory limits, and CPU-rate controls where supported. The job must be assigned before untrusted code starts; if this cannot be guaranteed, refuse.
7. It must apply a host watchdog that stops the exact FRIDAY-created Sandbox session on deadline or cancellation. A process timeout inside the guest is not a substitute for stopping the VM.
8. It must never fall back to running on the host if guest launch, assignment, monitoring, or result collection fails.

The exact Job Object limits, Windows API behavior, and assignment race must be independently reviewed and tested on supported Windows versions before this contract is implemented.

## Result contract

The guest may write only to a dedicated per-run output location. The result is a bounded JSON document:

- `schemaVersion`: integer, exactly `1`.
- `runId`: must exactly match the request.
- `operation`: must exactly match the request.
- `status`: one of `passed`, `failed`, `timed_out`, `cancelled`, `unsupported`, or `runner_error`.
- `exitCode`: optional signed 32-bit integer; required only for a process that exited normally.
- `stdout` and `stderr`: optional UTF-8 text, each bounded; combined result remains within the host policy limit.
- `truncated`: boolean indicating any output truncation.
- `durationMs`: non-negative integer bounded by the host watchdog policy.

The host must reject unknown fields, malformed UTF-8/JSON, duplicate keys, wrong IDs or operation, impossible status/exit-code combinations, negative or excessive duration, oversized files, multiple result files, and missing required fields. Result files must be opened without following reparse points; verified as ordinary files; checked against the expected per-run directory; and read with a strict maximum-plus-one byte limit. Never trust guest-reported paths or use them to locate host files.

A valid result is still untrusted diagnostic text. Do not automatically apply patches, run suggested follow-up commands, open links, or treat output as instructions. Show the result to the user as data.

## Output storage and cleanup

- The project snapshot remains read-only.
- The result directory is separate, empty, unique per run, and has restrictive host ACLs.
- The host must not map a broad user directory or the original project as writable.
- Disk use must be bounded by an enforceable host-side mechanism (for example, a separately reviewed fixed-size virtual disk or equivalent quota). A byte counter in the runner is not a disk quota.
- The host validates every entry before reading and removes only verified FRIDAY-owned run directories after the Sandbox is confirmed stopped.
- If session ownership, process termination, path identity, or cleanup cannot be confirmed, preserve data and report a recovery-required state rather than deleting broadly.

## Resource enforcement gate

Windows Sandbox .wsb configuration documents a memory cap but does not document a CPU quota, process deadline, or writable mapped-folder disk quota. Therefore the current .wsb preview alone cannot satisfy the execution requirements.

Before execution is enabled, FRIDAY must demonstrate all of the following on supported physical Windows systems:

- Guest process tree is contained in a Job Object before any untrusted instruction executes.
- Memory and CPU controls are configured and verified; inability to set or query them causes refusal.
- The entire Sandbox session can be uniquely associated with the current FRIDAY run and stopped without touching another user's or pre-existing Sandbox session.
- A host watchdog reliably detects timeout/cancellation and stops the owned session.
- Writable result storage has an enforceable size limit, not merely a post-hoc check.
- Output is bounded while being produced and again while being read by the host.
- No network, clipboard, device redirection, original-project write access, host fallback, or package download is possible.
- App crash, guest crash, host restart, timeout, cancellation, malformed result, and partial cleanup paths are covered by tests.

If any one of these conditions is unsupported or unverified on a machine, FRIDAY must keep execution disabled on that machine. Read-only project analysis and snapshot preview may continue.

## Windows Job Object feasibility research

Microsoft documents Windows Job Objects as a way to manage a group of processes and enforce process/job limits. Relevant controls include CPU-rate hard caps, job-wide committed-memory limits, process count limits, and user-mode process/job time limits. By default, child processes created by a process in a job are also associated with that job unless breakaway behavior is enabled. FRIDAY should not enable breakaway.

References:

- [Job Objects overview](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)
- [CPU rate control information](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_cpu_rate_control_information)
- [Basic Job Object limits](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_limit_information)

A candidate guest-side launch sequence is: create a Job Object; configure and query back all required limits; create the untrusted process suspended; assign it to the configured job; verify assignment; then resume its primary thread. Any failure before resume must terminate the suspended process and fail the run. This sequence is a design requirement, not yet implemented or proven. The runner itself must remain trusted, and process creation/assignment races and nested-job restrictions require Windows-specific testing.

Important distinctions:

- CPU hard-cap controls are a CPU scheduling limit, not a wall-clock deadline.
- Job process-time limits measure user-mode execution time, not elapsed wall-clock time. Keep a separate host watchdog for the total Sandbox session.
- Job memory limits constrain committed memory for associated processes; they do not replace the Sandbox VM memory setting.
- Job Objects do not provide a simple per-run disk-capacity quota for a writable host-mapped result folder. I/O byte notifications or I/O rate controls are not a storage-capacity limit.
- A bounded output file after the run is not protection against a guest that fills its writable volume before the host can inspect it.

Consequently, the next resource-control investigation is a separately bounded result volume: for example, a dedicated fixed-size virtual disk or a tested NTFS quota arrangement whose access semantics remain correct when Windows Sandbox maps the folder. The implementation must prove that the guest cannot write outside that volume and that the quota applies to the actual identity used for mapped-folder writes. Until this is demonstrated on supported physical Windows systems, no writable result folder may be exposed to guest code and execution remains disabled.

The newer `wsb` CLI documents an explicit Sandbox ID for stopping a session. A future host watchdog must persist and validate the ID returned for the FRIDAY-created session, refuse to adopt an existing session, and stop only that exact owned session. See [Windows Sandbox command-line interface](https://learn.microsoft.com/en-us/windows/security/application-security/application-isolation/windows-sandbox/windows-sandbox-cli). CLI availability alone is not proof of ownership or safe cancellation.

## Candidate bounded result-storage design: fixed-capacity VHDX

Microsoft documents that Windows Sandbox writable mapped folders persist changes on the host, and that the default guest account is `WDAGUtilityAccount`. Microsoft's newer CLI also supports sharing a host folder with an explicit Sandbox ID and optional write permission. A normal writable host folder therefore remains an unacceptable result target without an enforceable capacity boundary.

A candidate to prototype is a dedicated, per-run, fixed-capacity VHDX:

1. FRIDAY creates a new fixed-size VHDX using a supported Windows virtual-disk API, with a conservative hard maximum selected by policy (initial design target: 256 MiB, subject to test needs).
2. The host initializes and formats the virtual disk, mounts it at a unique empty directory, and verifies the mounted volume identity, filesystem, capacity, free space and mount path.
3. Only that directory is shared writable with the exact FRIDAY-created Sandbox session. The original project and the ordinary app-data workspace are never writable guest shares.
4. The guest runner writes a single bounded result document and bounded diagnostic files inside the share. The host still enforces per-file, total-file-count and total-bytes limits when reading.
5. After the Sandbox is confirmed stopped, FRIDAY revokes the share, dismounts the VHDX, verifies ownership and path identity, and removes only the verified per-run disk and mount directory. On any ambiguity, preserve the disk and report recovery required.

Why this is promising: a fixed-size virtual disk has a finite virtual capacity, so the guest cannot grow the filesystem beyond the provisioned volume size. It is a storage-capacity boundary rather than an application-level byte counter. Microsoft's `New-VHD -Fixed` documentation demonstrates fixed virtual disk creation, and `Mount-VHD` documents host mounting. References:

- [New-VHD — Fixed parameter](https://learn.microsoft.com/powershell/module/hyper-v/new-vhd)
- [Mount-VHD](https://learn.microsoft.com/powershell/module/hyper-v/mount-vhd)
- [Windows Sandbox mapped folders and write behavior](https://learn.microsoft.com/en-us/windows/security/application-security/application-isolation/windows-sandbox/windows-sandbox-configure-using-wsb-file)
- [Windows Sandbox CLI share and stop](https://learn.microsoft.com/en-us/windows/security/application-security/application-isolation/windows-sandbox/windows-sandbox-cli)

This is a candidate architecture, not a verified implementation. The Hyper-V PowerShell cmdlets may not be installed or usable on every supported Sandbox host; FRIDAY must not silently install optional features or elevate. A native virtual-disk API implementation may be needed, and its privilege requirements must be tested. The critical unresolved question is whether Windows Sandbox mapped-folder write access correctly traverses a host mount point backed by the VHDX on each supported Windows build, and whether the guest's effective write identity can modify only that mount. Test access control, mount-point/reparse behavior, VHDX detach after crash, disk-full behavior, concurrent/foreign sessions, and cleanup on physical Windows Pro/Enterprise/Education machines. If any prerequisite is absent or unverified, refuse guest execution rather than fall back to a normal writable host directory.

The VHDX bounds the dedicated result volume, not all guest temporary storage or the Sandbox VM's internal OS disk. The VM memory setting, Job Object process controls, host watchdog, and output parser remain separate required controls. A fixed VHDX also does not authenticate results or make guest-written files trustworthy.

## Result-storage host capability gate

The first implementation step is a read-only capability probe, not VHDX creation or attachment. It reports whether the host exposes `virtdisk.dll`, whether the current process token reports `SeManageVolumePrivilege` as enabled, and whether the optional Hyper-V `New-VHD`/`Mount-VHD` cmdlets are available. The Hyper-V cmdlets are diagnostic only; their absence does not prove the native API is unavailable.

Microsoft documents that `AttachVirtualDisk` fails when the caller lacks `SE_MANAGE_VOLUME_PRIVILEGE`. FRIDAY must not silently install Hyper-V, change token privileges, request elevation, or use a normal writable host folder as a fallback. A privilege that is absent or merely disabled is not treated as usable by this preflight. Even when the API and privilege appear available, the result-storage gate remains unverified until a physical Windows test proves fixed-capacity creation, mount-point identity, guest write access, disk-full behavior, crash recovery and safe dismount. Therefore this probe never enables execution.

References:
- [AttachVirtualDisk — Microsoft Learn](https://learn.microsoft.com/windows/win32/api/virtdisk/nf-virtdisk-attachvirtualdisk)
- [Virtual Disk API functions — Microsoft Learn](https://learn.microsoft.com/windows/win32/vstor/vhd-functions)

## Required regression suite

The implementation must add tests for:

- Unknown operation IDs, unknown request fields, duplicate JSON keys, malformed JSON, unsupported schema versions, and oversized requests.
- Shell metacharacters, argument injection, path traversal, alternate data streams, environment injection, PATH hijacking, and project-controlled executable substitution.
- Missing or altered pinned runtimes, missing lockfiles, package lifecycle scripts, build scripts, procedural macros, Python import side effects, and unsupported language profiles.
- Job Object creation/configuration/assignment failures, assignment-before-execution ordering, process-tree escape attempts, resource-limit failures, and host watchdog expiry.
- Existing or foreign Sandbox sessions, duplicate run IDs, cancellation races, app restart, and session ownership ambiguity.
- Symlink/reparse-point output entries, junctions, hard links where relevant, alternate streams, multiple files, oversized files, malformed UTF-8/JSON, duplicate keys, wrong IDs, inconsistent statuses and exit codes.
- Disk quota exhaustion, output floods, partial writes, process termination, Sandbox stop failures, and cleanup failures.
- Confirmation that every failure path leaves host execution disabled and never modifies the original project.

## Current implementation boundary

Current FRIDAY code may prepare bounded snapshots, preview a read-only .wsb configuration, report host Sandbox prerequisites, and recover verified stale snapshots. It does not create a guest runner, write a .wsb file, launch or stop Windows Sandbox, run commands, collect guest results, or enforce execution resource limits. Keep these statements accurate in the UI and documentation.
