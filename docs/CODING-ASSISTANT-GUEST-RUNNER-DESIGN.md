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


## Windows Sandbox CLI process-I/O investigation — not a result channel

The Windows Sandbox CLI was reviewed as a possible non-elevated way to collect guest test output without a writable host mapping. Microsoft's CLI documentation states that `wsb exec` runs a command in a Sandbox identified by its session ID and returns the command's exit code, but explicitly says process I/O is not supported and command output cannot be retrieved. The command also requires an active user session for `ExistingLogin` execution. Therefore `wsb exec` cannot deliver the structured guest result or bounded stdout/stderr required by the host result contract. It may be useful as a future control-plane primitive, but it is not a data-transfer mechanism.

Reference:
- [Microsoft: Windows Sandbox command-line interface](https://learn.microsoft.com/en-us/windows/security/application-security/application-isolation/windows-sandbox/windows-sandbox-cli)

Clipboard redirection is also not accepted as a substitute. Microsoft documents it as bidirectional copy/paste between host and guest, not as a dedicated authenticated or per-run result channel with a hard byte/storage quota. Enabling it would broaden the host/guest data path and would not prove a bound against a guest that floods or repeatedly changes clipboard content. The current preview configuration deliberately disables clipboard redirection; do not enable it to work around the missing result channel.

Writable `wsb share` remains a mapped-folder write path and therefore inherits the already documented concerns: host-side persistence, no documented per-run hard capacity quota, and the need to prove ownership, ACL and path behavior. It is not an alternative to the rejected writable mapped-folder design.

Conclusion: neither `wsb exec` output nor clipboard redirection satisfies the required result-transfer boundary. No documented non-elevated Windows Sandbox channel has yet been identified that avoids writable host mappings while enforcing a hard per-run storage cap. Keep the guest runner, result collection, and execution disabled. Continue with read-only snapshot/proposal workflows unless a separately reviewed and physically tested mechanism satisfies every storage, isolation, ownership, output, crash-recovery and cleanup gate.


## Hyper-V sockets and Microsoft MXC TCP bridge investigation — candidate rejected as-is

Two additional host/guest communication approaches were reviewed.

### Hyper-V sockets

Microsoft documents Hyper-V sockets as a host/guest data-stream API that does not use the ordinary networking stack. However, registering a custom guest communication service requires creating a service entry under the host's HKLM virtualization registry key, and Microsoft's setup instructions explicitly require an administrator PowerShell session. This is not a self-contained non-elevated feature FRIDAY can provision for an ordinary user. No host registry change or privilege request is approved.

Reference:
- [Microsoft: Make your own integration services (Hyper-V sockets)](https://learn.microsoft.com/en-us/windows-server/virtualization/hyper-v/make-integration-service)

Hyper-V sockets may be reconsidered only if a suitable already-registered built-in service can be used without broadening privileges or weakening isolation, and its availability/authorization can be verified on supported Windows Sandbox editions. The general Hyper-V socket API documentation alone does not prove that a usable endpoint is exposed to Windows Sandbox.

### Microsoft MXC Windows Sandbox TCP bridge

The Microsoft-owned [microsoft/mxc repository](https://github.com/microsoft/mxc) contains an experimental Windows Sandbox backend with an authenticated guest agent and separate control/stdin/stdout/stderr TCP channels. Its implementation is useful research for framing, nonce authentication, role-based channel pairing, and bounded host-side output draining.

It is not accepted as a drop-in FRIDAY design. Review of its current source identifies these mismatches with FRIDAY's security contract:

- Its generated Sandbox configuration enables networking. The guest agent then applies a guest-side Windows Firewall lockdown after the host connects. Windows Sandbox's default container account is documented as an administrator account, and the project workload is launched under that guest session. Guest-side firewall rules therefore cannot be treated as an immutable host-enforced network boundary against hostile code running with guest administrator rights.
- Its rendezvous directory is mapped writable from the host. The guest writes its rendezvous data there, so this retains a writable host-mapped storage path without a demonstrated per-run hard capacity boundary.
- Its executor starts the child process and only afterward attempts Job Object assignment; assignment failure is logged as best-effort and execution continues. Its shown Job Object configuration is kill-on-close, not the complete pre-execution CPU/memory/process-limit policy required by FRIDAY.
- Its executor accepts a script string and invokes `cmd.exe /C`, which is broader than FRIDAY's fixed operation/argument-vector contract.

Relevant source references for review:
- [MXC Windows Sandbox backend design](https://github.com/microsoft/mxc/blob/main/docs/backends/windows-sandbox/windows-sandbox.md)
- [MXC Sandbox configuration and writable rendezvous mapping](https://github.com/microsoft/mxc/blob/main/src/mxc-sdk/src/backends/windows_sandbox/lifecycle/vm.rs)
- [MXC guest firewall policy](https://github.com/microsoft/mxc/blob/main/src/mxc-sdk/src/bin/windows_sandbox_guest/firewall.rs)
- [MXC guest process launch and Job Object assignment](https://github.com/microsoft/mxc/blob/main/src/mxc-sdk/src/bin/windows_sandbox_guest/executor.rs)
- [Microsoft: Windows Sandbox configuration and default guest account](https://learn.microsoft.com/en-us/windows/security/application-security/application-isolation/windows-sandbox/windows-sandbox-configure-using-wsb-file)

Conclusion: the MXC bridge proves that stdout/stderr can be transported over a guest-agent channel, but its current design does not prove FRIDAY's required network, storage, or pre-execution resource boundaries. Do not copy its writable rendezvous share, guest-only firewall enforcement, arbitrary shell execution, or best-effort post-spawn Job Object assignment. A future TCP bridge would need host-enforced egress isolation, a read-only or otherwise bounded rendezvous mechanism, authenticated run-scoped channels, strict streaming caps, fixed operations, and Job Object assignment before untrusted code resumes. Until those requirements are proven on physical Windows, execution remains disabled.


## Unix no-follow source-file opening hardening

The shared `open_source_file` helper now uses the Unix `O_NOFOLLOW` flag when opening files. This makes the final path component fail atomically if it is a symbolic link, closing the race where a path could be swapped to a symlink between the caller's `symlink_metadata` check and `File::open`. A regression test verifies that a symlink is rejected while a normal file remains readable.

This protects the final component only. It does not by itself pin every parent directory against concurrent replacement, and it is not a substitute for Windows reparse-point testing or handle-relative directory traversal. The guest result reader remains disconnected from any guest execution or writable result channel.


## Current verification and remaining Stage 4/5 gates

CI verification update (November 13, 2026): Runs #425–#435 completed successfully; each run's `test-linux` and `build-windows` jobs passed. In particular, #433–#435 verify the Windows `FILE_FLAG_OPEN_REPARSE_POINT` symlink regression test as well as the preceding Unix `O_NOFOLLOW` final-component protection. CI success does not replace adversarial physical Windows testing or close the parent-directory replacement race.

The next implementation/review sequence is:

1. **Filesystem boundary review:** replace path-only recursive source traversal with directory-handle-relative traversal where supported, or otherwise fail closed when directory identity cannot be held stable. The current Unix `O_NOFOLLOW` protects only the final file component; `read_dir(current)` and path-based canonicalization do not pin parent directories against concurrent replacement. Review equivalent Windows directory-handle and reparse-point behavior. Add adversarial race tests and Windows junction/reparse-point tests.
2. **Snapshot/recovery ownership:** verify selected-root identity, private workspace ACLs, ownership marker handling, active-session ownership, concurrent app shutdown/restart, and conservative cleanup. Never delete an unverified or potentially mounted/active snapshot.
3. **Guest result-transfer boundary:** continue only with a supported, non-elevated mechanism that does not expose an unbounded writable host path, does not weaken host-enforced network isolation, and provides bounded/authenticated per-run output. Existing `wsb exec`, clipboard, NTFS quotas, ordinary writable mapped folders, current MXC bridge, and unprovisioned custom Hyper-V socket service do not meet this contract.
4. **Sandbox lifecycle and ownership:** launch/stop only an exact FRIDAY-owned session; handle existing/foreign sessions, process races, app restart, crash, timeout and cancellation without stopping unrelated Sandbox sessions.
5. **Fixed guest runtime and resource enforcement:** use only reviewed operation profiles and fixed executable/argument vectors; create suspended and assign to a verified Job Object before resuming untrusted code; enforce supported CPU/memory/process/time limits and an actual hard storage/output boundary. Never fall back to host execution.
6. **Result collection:** connect the already-tested request/result validators only after the transfer boundary exists; enforce run ID, operation, file identity, output caps, status/exit consistency and safe cleanup.
7. **Adversarial validation and physical Windows acceptance:** test malicious repositories, prompt injection, path/reparse-point swaps, output floods, disk-full, process-tree escape attempts, Sandbox ownership, cancellation, crash/restart, cleanup and supported Windows editions.

Stage 5 remains separate and must include malicious repository prompt-injection tests, path/reparse-point protections, interrupted-operation recovery, language fixtures, regression tests, final Windows CI and hands-on acceptance. Do not mark Stage 4 or Stage 5 complete based on CI alone.


## Follow-up filesystem traversal research (November 13, 2026)

Runs #433–#435 passed both Windows and Linux CI. The Windows symlink regression confirms that the final-component open path uses FILE_FLAG_OPEN_REPARSE_POINT in the test environment; it does not prove resistance to parent-directory replacement or physical Windows junction attacks.

A capability-oriented Rust filesystem API is now being evaluated for the snapshot copier:

- cap-std::fs::Dir represents an already-open directory and provides relative operations such as entries, open_dir, open, and symlink_metadata. This is a better foundation than repeatedly resolving absolute paths. Its documentation specifically warns that on Windows a directory handle must be opened without FILE_SHARE_DELETE to prevent rename/delete races while it is held.
- The companion cap-std-ext currently documents open_dir_noxdev (mount-boundary control), but that is not itself a no-symlink guarantee. Do not confuse the similarly named option with no-follow semantics.
- Before adopting either crate, inspect the exact pinned release's Unix and Windows implementations and prove: directory opens cannot escape the held root; symlink/junction/reparse entries are rejected rather than followed; file identity can be compared between no-follow metadata and the subsequently opened handle; Windows directory handles deny delete sharing; and child enumeration/opening stays relative to the held parent handle.
- The implementation must retain the current file-count, byte, depth, extension and secret-file limits; never use an ambient absolute path after establishing the root capability; and must fail closed on unsupported or ambiguous platform behavior.
- Required tests: replace a child directory with an outside-pointing symlink/junction between enumeration and open; replace a parent path while its handle is held; attempt .., absolute and drive-qualified child names; test file and directory reparse points; verify no outside file is copied; and verify normal nested source files still copy. Add Windows CI tests and retain physical Windows acceptance as a separate gate.

No traversal code has been changed based only on API documentation. The dependency and exact Windows semantics must be reviewed before adoption. Until the handle-relative implementation and adversarial tests pass, parent-directory race resistance remains unresolved and guest execution remains disabled.


## Capability-directory API review (November 13, 2026)

Runs #436–#438 passed both Windows build and Linux test jobs. The first-party cap-std/cap-primitives source review confirms a useful capability model, but it also exposed an important API-selection trap: cap_std::fs::Dir::open_dir uses the normal open_dir primitive, whose default options follow symlinks. The convenience method must not be used blindly for this feature.

Verified API facts from the published cap-std/cap-primitives 4.0.3 documentation/source:

- cap_std::fs::Dir holds an open directory handle. Its entries, metadata, symlink_metadata, open and open_dir operations are relative to that directory, and DirEntry deliberately has no ambient absolute path. This reduces dependence on mutable parent path strings.
- cap_primitives::fs::open_dir_nofollow is a public primitive that specifically fails when the requested directory is a symlink. It performs the relative open using the directory handle and no-follow policy. This is a stronger candidate for each one-component child-directory descent than Dir::open_dir.
- cap_primitives path resolution is component-by-component and rooted at the starting handle; it rejects root/prefix escape attempts. The code also documents explicit manual symlink handling. The exact version must be pinned and regression-tested rather than relying on a moving latest release.
- cap_std::fs::Dir::from_std_file explicitly warns that on Windows the underlying handle must be opened without FILE_SHARE_DELETE to prevent rename/delete races. This is a required Windows invariant, not an optional optimization.
- cap-std-ext open_dir_noxdev only blocks crossing mount points; it does not mean no-symlink and is not a substitute for open_dir_nofollow.

Implementation direction: use a pinned cap-std/cap-primitives pair (or a single supported public API surface) and walk exactly one normal filename component at a time. For each entry, inspect no-follow metadata, reject reparse/symlink entries, open child directories using the no-follow primitive relative to the held parent, and open regular files relative to the held parent with final-component no-follow. Never pass an entry's ambient absolute path back into std::fs. Before coding, inspect the Windows open_dir and open_unchecked implementation to confirm FILE_SHARE_DELETE is omitted, and inspect file open options to ensure no-follow is applied to the final component. If the public API cannot express both requirements, use a narrowly reviewed platform-specific helper or fail closed.

Remaining proof obligations: Windows directory handle share flags and junction/reparse-point behavior; no-follow regular-file opening; race behavior when entries are swapped between enumeration and open; device/mount policy; and whether file metadata can be reliably compared to the opened handle. Add deterministic race tests with synchronization hooks rather than timing-only stress tests. Keep physical Windows acceptance separate from CI. No production traversal code or dependency has been changed yet.


## Handle-relative traversal prototype added (November 13, 2026)

A separate, non-production prototype now lives in src-tauri/src/coding_sandbox_fs.rs and is compiled only for tests via a cfg(test) module. It accepts an already-open root directory handle, enumerates entries with cap-primitives, and descends into child directories only with open_dir_nofollow relative to the held parent handle. It never uses DirEntry::path or an ambient absolute child path. It validates every enumerated name as exactly one normal path component. The prototype returns candidate relative filenames only; it does not open file contents, create snapshots, or replace the existing copier.

The prototype dependencies are dev-dependencies only: cap-primitives is pinned exactly to 4.0.3, with ambient-authority 0.0.2 used only to open test roots. They are not linked into production FRIDAY builds. This version pin is security-significant: RUSTSEC-2026-0482 describes a critical path/symlink escape in cap-primitives versions before 4.0.3, fixed in 4.0.3. Do not downgrade or loosen this dependency without a fresh advisory review.

Prototype tests cover ordinary nested directories, Unix symlink file/directory exclusion, Windows symlink file/directory exclusion, one-component path validation, and a deterministic race where a directory is replaced with a symlink after enumeration but before open_dir_nofollow. The race test expects traversal to fail closed rather than reach the outside target.

Remaining before production adoption: verify CI on Windows and Linux; inspect the exact 4.0.3 Windows implementation to prove directory handles deny FILE_SHARE_DELETE and reparse/junction behavior is no-follow; add a Windows deterministic directory-swap test; design and test no-follow file opening relative to a held parent (the prototype intentionally does not open file data); safely acquire and identity-check the initial selected-root handle; preserve snapshot limits/exclusions and destination safety; then integrate only after all tests pass. Until then the existing snapshot copier remains in place and its parent-directory race remains unresolved. Guest execution stays disabled.


## Windows handle-relative file-open prototype and junction gate (November 13, 2026)

The test-only Windows file-open helper now uses NtCreateFile relative to the held parent directory handle with OBJ_DONT_REPARSE, FILE_OPEN_REPARSE_POINT, FILE_NON_DIRECTORY_FILE, and FILE_SYNCHRONOUS_IO_NONALERT. The synchronous-I/O option is required because the returned handle is wrapped in std::fs::File and read through Rust's synchronous Read implementation. The helper fails closed and does not retry with weaker flags. It verifies the opened handle metadata is an ordinary regular file and rejects reparse-point attributes. Run #453 exposed the missing synchronous-I/O option (Windows error 87 during Read); the correction was verified by Run #454, with both Windows and Linux jobs passing.

The helper remains Windows-only and test-only. Tests cover reading a regular file, rejecting a file symlink, rejecting a traversal name, and rejecting a file replaced by a symlink before the handle-relative open. This does not yet prove all reparse tags or filesystem configurations. A separate Windows junction test now creates a directory junction with mklink /J and asserts traversal either omits its contents or fails closed. That junction test is pending CI verification in the next run. Physical Windows acceptance and testing of additional reparse tags remain required.

The existing root handle is still acquired through a test-only ambient-authority helper; safe production acquisition and identity verification of the selected project root are not implemented. The walker still only enumerates candidate names, and the file-open helper is not integrated into snapshot copying. Existing production snapshot traversal remains path-based; guest execution remains disabled.


## Verified root-handle acquisition prototype (November 13, 2026)

The Windows junction test command was corrected to pass mklink arguments individually to cmd.exe rather than embedding quoted paths in a /C command string. The previous Runs #455–#457 all failed at junction creation with the same Windows path-syntax error; the traversal assertion itself was not reached. Run #458 passed both Windows and Linux jobs, including the junction test.

A new test-only root acquisition prototype was added to src-tauri/src/coding_sandbox_fs.rs. It requires an absolute path, rejects a final-component symlink/reparse point, opens the directory through the pinned capability primitive, checks the opened handle is a normal directory, compares filesystem identity before and after open, and rechecks that the path still names that same directory after acquisition. Unix identity uses device and inode; Windows identity uses volume serial number and file index, and missing identity fields fail closed.

The prototype is not used by production snapshot creation. It does not yet prove ancestor-path handling under all Windows namespace/reparse configurations, nor does it replace the selected-project authorization/canonicalization flow. Deterministic root replacement tests, broader reparse-tag coverage, physical Windows acceptance, and independent review are still required. Existing production snapshot traversal remains path-based and guest execution remains disabled.


## Windows reparse-point policy review (November 13, 2026)

The Windows prototype's policy is to reject every filesystem reparse point, not to maintain an allowlist of known tags. This is intentional. Windows assigns distinct tags to symbolic links, mount points/junctions, cloud placeholders, AppExecLink, WCI, ProjFS, DFS, deduplication and other filter-managed objects; third-party filters can also define tags. A tag allowlist would be incomplete by construction and could become unsafe when Windows or an installed filter introduces another tag.

The current Windows metadata predicate checks FILE_ATTRIBUTE_REPARSE_POINT (0x400) in addition to the file-type check. The root path checks, opened-root checks, ancestor checks, Windows no-follow file-open postcondition, and result-file reader use this blanket attribute policy. The handle-relative Windows file helper also requests OBJ_DONT_REPARSE and FILE_OPEN_REPARSE_POINT, then verifies the opened handle is not marked as a reparse point. The directory-walk prototype uses cap_primitives::fs::open_dir_nofollow; the Windows junction regression in Run #458 verifies a junction is not traversed. Run #467 passed both Windows and Linux CI and adds a deterministic ancestor replacement test.

Implications by category:

- Symbolic links and directory junctions: explicitly covered by existing tests; must remain rejected.
- Volume mount points: carry the mount-point reparse tag and the same FILE_ATTRIBUTE_REPARSE_POINT attribute; reject them exactly like junctions. No mount-point creation test is claimed because creating a real volume mount point requires a suitable mounted volume and privileges not assumed in CI.
- Cloud Files/OneDrive placeholders, WCI/container links, AppExecLink, ProjFS, DFS, deduplication/HSM and unknown third-party tags: reject based on the generic reparse attribute. Do not hydrate, traverse, interpret, or copy them.
- Unknown or newly introduced tags: reject by default; never fall back to ordinary path-based open or tag-specific interpretation.

This review is based on Microsoft's reparse-tag and reparse-point documentation, which describes the tag as the identifier for the owning filesystem/filter behavior and documents FILE_ATTRIBUTE_REPARSE_POINT as the generic presence indicator. References:
- [Microsoft: Reparse Point Tags](https://learn.microsoft.com/en-us/windows/win32/fileio/reparse-point-tags)
- [Microsoft: Reparse Point Operations](https://learn.microsoft.com/windows/win32/fileio/reparse-point-operations)
- [Microsoft: Determining Whether a Directory Is a Mounted Folder](https://learn.microsoft.com/en-us/windows/win32/fileio/determining-whether-a-directory-is-a-volume-mount-point)
- [Microsoft: File System Reparse Points](https://learn.microsoft.com/en-us/windows-hardware/drivers/ifs/reparse-points)

Important limitation: this is a policy/source review, not proof that every Windows filesystem, filter driver, network share, or race behaves identically. CI currently covers ordinary directories, symlinks, junction traversal, final-component no-follow behavior, and deterministic root/ancestor replacement scenarios as their respective runs complete. Physical Windows acceptance must still include a mount-point test where a safe test volume is available, plus reparse-point behavior on supported Windows editions. Any unexpected metadata/open error remains a hard failure. The prototype is test-only; production snapshot traversal remains path-based and guest execution remains disabled.


## Handle-relative ancestor acquisition follow-up (November 13, 2026)

The root acquisition prototype now starts from the filesystem root (Unix) or a local drive-letter root (Windows) and opens each normal path component relative to the directory handle already held. Each opened component is checked as an ordinary directory and rejected if it is a symlink/reparse point. The final opened root handle is compared against the selected root identity captured before traversal, and the path is checked again after acquisition. This removes repeated ambient resolution of intermediate ancestors during the component walk.

Windows UNC shares and device namespace paths are explicitly unsupported and fail closed until their root-handle semantics receive separate review and acceptance tests. Canonical local drive paths have a regression test; UNC rejection has a regression test. The Windows directory-handle sharing test continues to verify that a held directory cannot be renamed while the traversal handle is open. Unix now has a deterministic test that replaces an ancestor pathname after its directory handle has been opened: the next component is still opened relative to the original held handle, and final path/handle identity verification rejects the changed root.

Runs #467–#472 passed both Windows and Linux CI jobs. These runs verify the preceding ancestor-replacement/reparse policy changes and initial root-acquisition implementation. The newest held-ancestor race regression is in the subsequent commit and is awaiting its own CI run. This remains a test-only prototype: production snapshot traversal is still path-based, no snapshot file data is copied through this module, and guest execution remains disabled. Do not integrate until the new regression passes and the remaining Windows path/reparse/volume acceptance review is complete.


## Handle-relative regular-file opening prototype (November 13, 2026)

A Unix counterpart to the Windows handle-relative file-open prototype has been added in `coding_sandbox_fs.rs`. It validates a single normal filename component, calls `openat` relative to the already-held parent directory handle, and requests `O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK`. The opened handle must report a regular file. O_NONBLOCK prevents a raced-in FIFO from blocking before the type check. There is no path-based fallback. Regression tests cover regular-file reads, symlink and traversal-name rejection, and replacement of the parent pathname after its directory handle is held; the last case verifies that the open still reads the original authorized file rather than the replacement directory's file.

The Windows counterpart continues to use `NtCreateFile` with `RootDirectory`, `OBJ_DONT_REPARSE`, `FILE_OPEN_REPARSE_POINT`, synchronous I/O, and an opened-handle regular-file/reparse check. Both helpers remain part of the experimental prototype. They are not yet wired into production snapshot copying. The new Unix tests and the preceding held-ancestor root-acquisition regression are awaiting CI in Runs #473–#480; do not mark them verified until the relevant Linux and Windows jobs pass.


## Bounded handle-relative snapshot content prototype (November 13, 2026)

The test-only filesystem prototype now combines the verified root handle, handle-relative directory enumeration/descent, and handle-relative regular-file opening into a bounded in-memory source capture. It returns relative names plus byte buffers; it does not create destination files and is not wired into the production snapshot command.

The prototype enforces caller-supplied non-zero limits for directory depth, file count, per-file bytes, and total bytes. It accepts explicit file/directory filter callbacks so the production allowlist and exclusions can be connected only after review. It skips links and non-regular entries, opens child directories relative to held parent handles, opens files relative to the held parent with the platform-specific no-follow helper, and checks opened-handle type and size. It performs a post-read size/modified-time consistency check on the same handle. A raced file open is treated as a truncated/partial capture; a directory descent that cannot be safely opened fails closed.

Regression coverage added for nested content capture, file-count and total-byte limits, zero-limit rejection, a file replaced by a symlink between enumeration and open, and a parent pathname replaced after its handle is held. Runs #475–#478 and #480–#482 passed both Windows and Linux CI; #473–#474 and #479 were earlier failed runs whose test-only compile errors were corrected in the subsequent successful commits.

Important limitations remain:
- This is an in-memory test prototype, not production snapshot copying.
- The post-read metadata check is not an atomic filesystem snapshot and cannot prove a file's contents were immutable during the read in every filesystem.
- Destination directory/file creation and cleanup are deliberately not implemented here. A separate destination-side design must use safe, app-owned storage and defend against reparse/path replacement and partial-write cleanup.
- The production snapshot copier remains path-based and unchanged.
- No guest process is launched; guest execution remains disabled.



## Handle-relative snapshot destination creation prototype (November 13, 2026)

A separate test-only destination writer has been added to `src-tauri/src/coding_sandbox_fs.rs`. It accepts only a verified, already-open workspace directory handle, a 32-character hexadecimal snapshot ID, bounded in-memory source contents, and explicit non-zero limits. It creates the snapshot directory exclusively relative to that held handle and creates every nested directory/file relative to a held parent handle.

Platform behavior:
- Unix uses `mkdirat` for exclusive directory creation and `openat` with `O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC` for exclusive regular-file creation. Created directories/files use restrictive 0700/0600 modes (subject to the process umask).
- Windows uses `NtCreateFile` with `RootDirectory`, `OBJ_DONT_REPARSE`, `FILE_CREATE`, explicit directory/non-directory options, synchronous I/O, and no delete sharing on newly created directory handles. The opened handle's type and generic reparse attribute are checked; there is no ambient-path fallback.
- All relative path components must be normal components. The reserved ownership-marker filename cannot be supplied as source content. File count, depth, per-file size, total byte count, and the caller's truncation state are validated again before creation.
- File contents are written and flushed through the exclusively created handle. The ownership marker is written last, only after all payload files have been created successfully. Therefore a failed/partial creation is not represented as a complete owned snapshot.

New regression tests cover nested destination writes, refusing to overwrite a pre-existing snapshot directory, rejecting traversal before any directory is created, and ensuring a failed/partial write does not create an ownership marker. Run #487 independently passed both Windows and Linux jobs for the preceding bounded source-capture fix. The destination prototype and its new tests are in the subsequent commits and are awaiting their own CI verification.

Important limitations:
- This remains test-only and is not connected to the production snapshot command.
- The workspace handle must already have been safely acquired and authorized; this helper does not establish that the app-data root is trustworthy.
- Cleanup is deliberately not implemented by this writer. Partial unmarked directories are left untouched rather than risking deletion of unrelated content. A separate handle-relative cleanup/recovery design must prove ownership and delete entries without following links before this can be integrated.
- The marker is an ownership signal for the FRIDAY recovery protocol, not a cryptographic proof against a malicious process running as the same Windows user. Same-user interference and filesystem-specific semantics require further review.
- The implementation is not an atomic snapshot of the source project. Source consistency limitations described above still apply.
- The production snapshot copier remains unchanged and path-based. Guest execution, guest launch, and guest file collection remain disabled.



## Snapshot cleanup/recovery security gate (November 13, 2026)

Runs #488–#495 completed successfully on Windows and Linux CI. The destination-side writer prototype and its exclusive-create/partial-marker tests are therefore CI-verified, but this does not verify cleanup or production integration.

A review of the existing production recovery path in `src-tauri/src/coding_sandbox.rs` found that it validates the workspace/snapshot paths and ownership marker, then calls `fs::remove_dir_all`. The marker check and recursive deletion are separate path-based operations. A replacement/race between validation and deletion can invalidate the earlier checks; recursive deletion is not an acceptable security boundary for this workflow. The current schema-1 marker also records only schema version, snapshot ID and owner. It does not inventory the expected payload, so it cannot justify deleting arbitrary additional files that appear inside a marked snapshot.

Required design before implementing deletion:

1. **No deletion based on marker presence alone.** Keep schema-1 marker snapshots non-cleanable by the new prototype. Unknown marker versions, malformed markers, missing markers, mismatched IDs/owners, and partial/unmarked directories are preserved.
2. **Add a versioned inventory for newly created snapshots.** The future manifest must describe every expected regular file and directory created by the writer, use an unambiguous platform-independent encoding for path components, enforce strict entry/count/size limits, reject duplicates and reserved names, and be written last. A schema migration must not reinterpret old markers as the new format.
3. **Preflight the complete tree through held handles.** Compare the observed tree to the manifest without following links/reparse points. If any entry is unexpected, missing, duplicated, a special file, or cannot be inspected, preserve the entire snapshot and report it as unverified. Do not silently prune unknown entries.
4. **Delete only after exact preflight.** Hold the workspace and descendant directory handles throughout the operation. Use platform-specific handle-relative deletion primitives; never fall back to ambient paths or `remove_dir_all`. Recheck object type/identity at the point of deletion. Unix should use descriptor-relative operations and unlink only verified ordinary files/directories. Windows must use a reviewed handle-based disposition operation on opened handles and prove that reparse points are not traversed.
5. **Make interruption retry-safe.** A partially completed cleanup must be detectable and resumable without broadening authority. If the state is ambiguous, preserve the remainder. Do not remove the ownership evidence until all expected payload entries have been removed and the containing directory can be removed safely.
6. **Threat-model same-user interference honestly.** The marker/manifest is not cryptographic proof against another process running as the same user. Do not claim protection against a fully malicious same-user process. Keep the workspace ACL/private app-data boundary and handle-sharing semantics as separate requirements.

Planned test-only sequence: manifest parsing/version/limit tests; exact-tree preflight tests; unknown-file preservation; malformed/missing/legacy marker preservation; symlink and Windows reparse/junction replacement tests; parent-path replacement after handles are held; nested deletion; deterministic interruption after each deletion stage followed by retry; and proof that unrelated sibling snapshots are untouched. Windows-specific deletion semantics require source review and CI plus physical Windows acceptance before any production use.

Current status: this is a design/review gate only. No cleanup code has been connected to production, no existing recovery behavior has been silently changed, the production snapshot copier remains path-based, and guest execution/launch/result collection remain disabled.

## Versioned snapshot inventory prototype (November 13, 2026)

A test-only schema-2 snapshot inventory has been added alongside the handle-relative destination writer. It records the snapshot ID, owner, exact relative file paths and sizes, and the complete set of parent directories. Paths are serialized as portable slash-separated UTF-8; non-UTF-8 or ambiguous components fail closed rather than becoming lossy cleanup evidence. Parsing denies unknown JSON fields, rejects schema-1/unknown versions, malformed or mismatched identity, traversal/reserved paths, duplicate files/directories, inconsistent directory inventories, and entries exceeding the existing snapshot depth/count/per-file/total-byte limits. The serialized marker is bounded to 1 MiB and is validated before destination creation begins. The marker is still written last.

This inventory is only a prerequisite for exact-tree preflight. It does not authorize deletion, does not make the marker cryptographic proof, and does not replace a filesystem scan through held handles. No cleanup function has been implemented; legacy schema-1 snapshots and unmarked partial directories must remain preserved. CI for this inventory implementation is pending in Runs #498 and #499. Continue to keep production recovery unchanged until handle-relative preflight and deletion are independently reviewed and tested.


## Handle-relative snapshot cleanup preflight prototype (November 13, 2026)

A test-only preflight inspector now opens the completed snapshot marker and walks the snapshot tree through held directory handles. It validates the version-2 inventory, checks each expected regular file's type and recorded size, verifies the expected directory set, and rejects unknown entries, special files, symlinks/reparse points, and type/size mismatches. The inspector performs no filesystem mutation.

The inventory is an expected-tree description, not cryptographic proof of ownership. A successful preflight is not yet permission to delete the tree: the later cleanup implementation must repeat identity/type checks at the point of deletion using handle-relative operations and must preserve any entry that cannot be proven safe. Same-user malicious modification and fully atomic filesystem snapshots remain outside this prototype's guarantees.

Tests cover an exact inventory, an unexpected file, and a symlink. Windows and Linux CI must both pass before further work. Production recovery remains unchanged; guest execution remains disabled.


## First snapshot-file cleanup primitive prototype (November 13, 2026)

A narrowly scoped, test-only Unix helper now attempts to remove one ordinary file relative to a held parent directory handle. It requires a single non-marker filename, opens without following symlinks, checks the expected size, reopens the entry and compares handle identity, then uses descriptor-relative `unlinkat`. A regression test verifies that a size mismatch preserves the file and that a separate unknown file remains untouched.

Important limitation: Unix `unlinkat` is name-based, so a hostile same-user process could still race a rename between the final identity comparison and unlink. This helper is not a complete race-free delete-by-handle primitive and must not be used in production. The Windows implementation intentionally returns Unsupported and preserves the file until a reviewed delete-by-handle operation is implemented. Recursive cleanup, directory removal, marker removal, retry/recovery, and production integration are not implemented. Keep guest execution disabled.


## Windows handle-based snapshot-file deletion prototype (November 13, 2026)

The test-only Windows file-removal primitive now opens the named entry relative to the already-held parent directory handle with NtCreateFile, requests DELETE access, denies FILE_SHARE_DELETE, and uses no-reparse/open-reparse-point semantics. It verifies that the opened object is an ordinary regular file with the exact expected size, then applies FileDispositionInfoEx to that same handle. The handle is closed before the test checks that the name is gone. The regression test also verifies that a size mismatch preserves the target and an unrelated file remains untouched.

This is deliberately limited to one file at a time. The Windows delete disposition is applied to the opened object rather than re-resolving the filename for deletion; denying delete sharing is intended to prevent a concurrent rename while the handle is held. Unsupported filesystems/OS versions, sharing conflicts, and native API failures must return an error and preserve data. This remains a test-only primitive and is not production cleanup authorization.

The Unix primitive still uses name-based unlinkat and retains its documented same-user rename-race limitation. Recursive tree deletion, safe empty-directory removal, marker removal, interruption/retry recovery, full threat-model review, and production integration remain outstanding. Guest execution remains disabled.
