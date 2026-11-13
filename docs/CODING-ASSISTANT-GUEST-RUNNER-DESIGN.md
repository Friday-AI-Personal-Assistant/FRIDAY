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
- `maxDurationMs`: selected by FRIDAY policy, never by the model; integer from 1 through 120,000 milliseconds.
- `maxOutputBytes`: selected by FRIDAY policy; integer from 1 byte through 1 MiB for the combined structured result and captured diagnostics.

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
- `stdout` and `stderr`: optional UTF-8 text; their combined UTF-8 byte length must not exceed the host policy limit, and the complete JSON document is separately capped at 1 MiB.
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


## Host-side guest result contract validator — implemented, not connected to execution

The Rust host now contains a strict JSON contract validator for a future guest result. It accepts only schema version 1, the fixed operation enum (`rust-cargo-test`, `node-npm-test`, `python-unittest`), the fixed status enum, the expected 32-character run identifier, and the expected operation. Unknown fields and duplicate fields are rejected by the typed deserializer. The complete JSON document is capped at 1 MiB, combined UTF-8 stdout/stderr must also remain within the exact `maxOutputBytes` budget assigned to that run (never above 1 MiB), and reported duration is capped at 120 seconds. The host validator checks the per-run budget rather than relying only on the global ceiling. Status/exit-code combinations are checked so a successful result must report exit code 0, a failed result must report a non-zero exit code, and non-process terminal states must not claim an exit code.

Unit tests cover valid success, malformed JSON, unknown and duplicate fields, run/operation mismatch, inconsistent status/exit codes, excessive output, excessive duration, oversized documents, and invalid expected identifiers.

This is deliberately only a pure host-side JSON validator. It is not yet connected to a result directory, safe file opening, a guest process, Windows Sandbox, VHDX storage, or any execution command. The parser does not authenticate guest output; even accepted diagnostics remain untrusted data. All storage, ownership, session, resource, and isolation gates remain outstanding, and execution remains disabled.


## Host-side guest run request validator — implemented, not connected to execution

The host now also validates a bounded version-1 request structure for a future guest runner. The only accepted operation values are the three fixed enum values already listed above. The request contains only a schema version, generated run ID, snapshot ID, fixed operation, maximum duration and maximum output bytes. Both IDs must be exactly 32 hexadecimal characters. `maxDurationMs` must be between 1 and 120,000; `maxOutputBytes` must be between 1 byte and 1 MiB; and the complete JSON request is capped at 4 KiB. Unknown fields and duplicate keys are rejected. In particular, there are no fields for a shell command, executable path, arbitrary arguments, environment, working directory, or network settings.

Unit tests cover the valid contract, unknown/duplicate fields, unsupported versions, path-like identifiers, out-of-range limits and oversized request documents. These checks constrain the data shape only; they do not prove that a future guest runner safely launches a runtime or that the supplied snapshot is safe to execute.

The request parser is currently a pure host-side validator and is not wired to any UI, file, guest process, or Sandbox session. Execution remains disabled.

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

## Windows Sandbox mapped-folder policy and session constraints

Windows 11 24H2 and later expose device policies for allowing mapped folders and allowing writes to mapped folders. The policy values are under `HKLM\\SOFTWARE\\Policies\\Microsoft\\Windows\\Sandbox`:

- `AllowMappedFolders = 0` blocks mapped folders; `1` allows them.
- `AllowWriteToMappedFolders = 0` blocks guest writes to mapped folders; `1` allows them.
- An absent value is reported as **Not configured**, not silently rewritten by FRIDAY. An unexpected registry value is **Unknown** and must remain unverified.

The read-only preflight now reports both policy states separately. This is diagnostic only: it does not change Group Policy, registry values, or permissions, and it does not mark result storage ready. A policy that blocks mapped folders means the snapshot cannot be shared through that mechanism; a policy that blocks writes means a future writable result share cannot be used. FRIDAY must fail closed rather than change policy or choose a normal host folder.

References:
- [Windows Sandbox Policy CSP — AllowMappedFolders and AllowWriteToMappedFolders](https://learn.microsoft.com/en-us/windows/client-management/mdm/policy-csp-windowssandbox)
- [Windows Sandbox configuration and mapped-folder behavior](https://learn.microsoft.com/en-us/windows/security/application-security/application-isolation/windows-sandbox/windows-sandbox-configure-using-wsb-file)

The Windows Sandbox CLI uses an explicit Sandbox ID for operations such as `share` and `stop`. The host must retain the exact ID returned when FRIDAY creates a session and must never infer ownership from a process name alone, adopt a pre-existing session, or stop a session whose ID cannot be matched to the current FRIDAY run. Current process detection is only a conservative blocker; it is not a session-ownership mechanism.

Windows Sandbox is a shared environment rather than a per-process container. Do not run mutually untrusted tasks concurrently in one guest. Before any future launcher is implemented, verify the supported Windows build's single-instance/session behavior, how the CLI reports existing sessions, and how FRIDAY can prove it created the exact session it later shares with or stops.

Reference:
- [Windows Sandbox command-line interface](https://learn.microsoft.com/en-us/windows/security/application-security/application-isolation/windows-sandbox/windows-sandbox-cli)

## Native Virtual Disk API feasibility findings

The native API is a more portable candidate than depending on the optional Hyper-V PowerShell module, but it does not remove the host privilege or storage-initialization constraints.

- `CreateVirtualDisk` in `virtdisk.dll` can create a VHDX. For a fixed-capacity disk, the API's `CREATE_VIRTUAL_DISK_FLAG_FULL_PHYSICAL_ALLOCATION` flag requests pre-allocation of the physical backing space. The disk must have no parent path; FRIDAY must never create a differencing disk for guest results.
- With `CREATE_VIRTUAL_DISK_VERSION_2`, the creation call requires `VIRTUAL_DISK_ACCESS_NONE`. The VHDX block size must be a multiple of 1 MiB in the documented 1–256 MiB range; the sector size is 512 or 4096 bytes. The maximum size must be sector-aligned and at least 3 MiB. These are API validity constraints, not proof that a disk is safe to mount or share.
- Before creation, FRIDAY must reserve the exact run-specific destination with exclusive creation, ensure the parent workspace is a normal private directory, and reject compressed or EFS-encrypted backing volumes. It must not overwrite an existing path, even if a file appears to have the expected name.
- `AttachVirtualDisk` requires `SE_MANAGE_VOLUME_PRIVILEGE`. It also inherits the virtual disk file's security descriptor if no explicit descriptor is supplied. Therefore the disk file and containing workspace ACL must be restrictive before attach. The current preflight only reports whether the privilege is enabled; it does not attempt to enable it or elevate.
- Detach requires a handle opened with detach access, and other open handles must be closed before `DetachVirtualDisk` succeeds. Do not use permanent-lifetime attachment. FRIDAY must retain enough verified run metadata to detach only the disk it created, and must report recovery-required rather than deleting a file if detach cannot be confirmed.

References:
- [CreateVirtualDisk](https://learn.microsoft.com/windows/win32/api/virtdisk/nf-virtdisk-createvirtualdisk)
- [CREATE_VIRTUAL_DISK_PARAMETERS](https://learn.microsoft.com/windows/win32/api/virtdisk/ns-virtdisk-create_virtual_disk_parameters)
- [CREATE_VIRTUAL_DISK_FLAG](https://learn.microsoft.com/windows/win32/api/virtdisk/ne-virtdisk-create_virtual_disk_flag)
- [AttachVirtualDisk](https://learn.microsoft.com/windows/win32/api/virtdisk/nf-virtdisk-attachvirtualdisk)
- [DetachVirtualDisk](https://learn.microsoft.com/windows/win32/api/virtdisk/nf-virtdisk-detachvirtualdisk)

The native API creates and attaches the virtual disk; it does not by itself create a partition, format a filesystem, assign a mount point, or prove that Windows Sandbox mapped-folder writes are constrained to that volume. The next implementation must not use an unreviewed `diskpart` script or select a disk by a transient disk number alone. It must correlate the attached device to the exact VHDX/run identity, verify the disk's unique identity and capacity before any initialization, and verify the resulting volume and mount path after formatting. If FRIDAY cannot do those steps safely under the current token without elevation, it must stop before initialization and keep `resultStorageReady = false`.

No VHDX creation, attachment, partitioning, formatting, mounting, or guest sharing has been implemented by these findings. They refine the native API design only; the execution gate remains closed.

## Host disk initialization privilege gate

The next blocker is not VHDX creation alone: the newly attached disk must be initialized, partitioned, formatted and mounted. Microsoft's Windows disk-management guidance states that initializing a new disk requires membership in the local **Administrators** or **Backup Operators** group, and the Disk Management UI instructs users to run with Administrator permissions. The Storage module exposes safer identity-oriented parameter sets such as `Initialize-Disk -UniqueId` / `-VirtualDisk` and `New-Partition -DiskId` / `-DiskPath`; these are preferable to selecting a disk by a transient disk number, but they do not remove the privilege requirement.

References:
- [Initialize new disks — Microsoft Learn](https://learn.microsoft.com/en-us/windows-server/storage/disk-management/initialize-new-disks)
- [Initialize-Disk — Microsoft Learn](https://learn.microsoft.com/en-us/powershell/module/storage/initialize-disk)
- [New-Partition — Microsoft Learn](https://learn.microsoft.com/en-us/powershell/module/storage/new-partition)
- [Format-Volume — Microsoft Learn](https://learn.microsoft.com/en-us/powershell/module/storage/format-volume)

Therefore, a standard FRIDAY process cannot assume it can complete the VHDX lifecycle merely because `virtdisk.dll` exists or `SeManageVolumePrivilege` is enabled. The design must explicitly decide whether a narrowly scoped, separately reviewed elevated helper with a visible Windows UAC approval is acceptable, or whether a non-elevated result-transfer architecture is needed. No elevated helper is implemented or approved by this design note. FRIDAY must not silently relaunch itself as administrator, collect credentials, run arbitrary elevated PowerShell, or proceed with partial storage and expose it to a guest. If the user declines or the required token rights are absent, the run must stop before disk initialization and leave execution disabled.



## Default privilege and result-transfer decision — fail closed

For the initial FRIDAY release, the guest-runner design will **not** introduce a separately elevated helper or silently relaunch FRIDAY with administrator rights. No credentials may be requested or stored, and no arbitrary elevated PowerShell or command script may be used. A future narrowly scoped helper would require a separate threat-model and code review, a clear user-visible Windows UAC prompt for the exact operation, strict input validation, and an explicit product decision before implementation; it is not part of the current plan.

The next design investigation prioritizes a non-elevated result-transfer mechanism. It is acceptable only if it can demonstrate, on supported physical Windows versions, all of the following:

- A hard, enforceable upper bound on guest-writable storage before untrusted code runs (not merely a host-side file-size check after the fact).
- Isolation of the writable result area from the original project, user profile, FRIDAY data, and all unrelated host paths.
- A fixed, narrowly scoped guest-visible destination, with no ability to redirect writes through symlinks, junctions, reparse points, alternate streams, or path tricks.
- Reliable ownership binding to the exact FRIDAY-created Sandbox session and run ID.
- Safe handling of disk-full conditions, output floods, guest/app crashes, cancellation, restart, and incomplete cleanup.
- Bounded host-side parsing and validation of returned files; guest output remains untrusted.
- No network access, no host-execution fallback, and no weakening of the read-only project snapshot.

Microsoft's documented Windows Sandbox mapped-folder configuration exposes a read-only versus writable choice, but does not document a per-mapped-folder capacity quota. Microsoft also warns that writes to a writable mapped folder persist on the host after the Sandbox is disposed. The newer `wsb share` CLI similarly exposes an `--allow-write` switch, not a documented byte quota. This does not prove that no lower-level Windows mechanism could enforce a quota; it means FRIDAY must not assume Windows Sandbox itself supplies one. References: [Windows Sandbox configuration and mapped folders](https://learn.microsoft.com/en-us/windows/security/application-security/application-isolation/windows-sandbox/windows-sandbox-configure-using-wsb-file) and [Windows Sandbox CLI share command](https://learn.microsoft.com/en-us/windows/security/application-security/application-isolation/windows-sandbox/windows-sandbox-cli).

A normal writable mapped folder plus post-write size checks does **not** satisfy the hard storage-bound requirement: the guest could consume host disk space before FRIDAY notices. A VHDX or other bounded volume is not considered viable until its creation, initialization, mount, ACL, mapped-folder behavior, and cleanup can all be proven under the selected non-elevated model. If no mechanism satisfies these conditions, the runner remains unavailable and the existing safe preview/proposal workflow remains the supported behavior.

This is an architecture decision and acceptance gate, not an implementation claim. No storage mechanism, guest runner, launcher, or execution permission is added by this decision.


The host result validator now has a per-run output-budget entry point. It rejects zero or above-policy expected limits and rejects combined stdout/stderr whose UTF-8 byte count exceeds the exact budget supplied for that run. Regression tests cover the boundary, one-byte overflow, zero, and above-policy limits. The existing convenience validator still applies the fixed 1 MiB ceiling for parser-only tests; no guest execution or result-file collection is connected.


## Guest result-file boundary validation

The host now has a pure result-file reader for the future collection boundary. It accepts only a normal result directory containing exactly one entry named `result.json`; missing files, additional files/directories, alternate casing, non-text names, symlinks/reparse points, non-regular files, empty files, oversized files, and paths that do not resolve as a direct child of the verified result directory are rejected. The file is opened with the existing no-follow/reparse-point inspection behavior, its opened metadata is checked against the path metadata, and the read is bounded to the global result-document ceiling before the existing run ID, operation, schema, status/exit-code, duration, and per-run output checks are applied.

Regression tests cover the exact filename allowlist, missing and extra entries, a valid result file, oversized content, and a symlinked result file on Unix. This remains a disconnected validator: FRIDAY does not create a result directory, mount writable storage, collect a guest-produced file, launch or stop Windows Sandbox, or enable execution. The Windows reparse-point and real guest mapped-folder lifecycle still require dedicated Windows acceptance testing before any collection workflow can be connected.


## NTFS per-user quota investigation — not accepted as a per-run boundary

NTFS disk quotas were evaluated as a possible way to bound guest writes without creating and formatting a per-run VHDX. Microsoft's quota model charges storage to the Windows security identifier (SID) recorded as the file owner and applies warning/hard limits to a user on a volume; it is not a per-directory quota. A hard quota can deny further allocation after the user's limit is reached, but the quota state and per-user limits are managed at the volume/system level.

References:
- [Microsoft: Managing Disk Quotas](https://learn.microsoft.com/en-us/windows/win32/fileio/managing-disk-quotas)
- [Microsoft: System-level Administration of Disk Quotas](https://learn.microsoft.com/en-us/windows/win32/fileio/system-level-administration-of-disk-quotas)
- [Microsoft: Disk Quota Limits](https://learn.microsoft.com/en-us/windows/win32/fileio/disk-quota-limits)

This is not currently an acceptable FRIDAY per-run storage design:

- A quota is per host-side file-owner SID and volume, not scoped to the exact result directory or run ID.
- FRIDAY cannot assume that files created through a Windows Sandbox mapped folder are charged to a distinct, stable host SID representing that one guest run. The guest's in-container account name is not proof of the host-side file owner used by NTFS quota accounting; this requires physical Windows measurement.
- Reusing a host identity such as SYSTEM could affect unrelated writes charged to that SID on the same volume, so it cannot safely isolate one run.
- Provisioning and enforcing a dedicated host identity/volume quota is an administrative, machine-level configuration, not a self-contained non-admin per-run operation. FRIDAY must not silently change machine-wide quota policy or quota settings.

Therefore NTFS quotas are rejected for the initial non-admin result-transfer design. They may be reconsidered only if a separately reviewed design proves a unique host-side identity and quota scope for each run, safe provisioning/cleanup, no impact on unrelated host activity, and hard enforcement before untrusted code can write. No quota settings or host identities have been changed by FRIDAY.

The current research has not identified a documented Windows Sandbox guest-to-host result channel that both avoids writable host mappings and provides a hard per-run storage cap. Until a supported mechanism meets that requirement, guest execution remains unavailable; read-only snapshots and safe proposal workflows remain the supported path.


Additional regression coverage now rejects a result directory that is itself a symlink and rejects non-text entry names on Unix. These tests strengthen the pure reader's boundary checks; they do not replace Windows reparse-point tests on supported physical Windows installations or a future ACL/ownership-bound result-directory lifecycle.
