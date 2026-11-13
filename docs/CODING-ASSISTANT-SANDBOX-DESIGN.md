# Coding Assistant Stage 4 — Sandbox design and constraints

Status: design constraints documented; execution remains disabled.

## Current snapshot boundary

FRIDAY creates a private, unique copy under its application-local data directory. The selected project is revalidated before copying. Only allowlisted source and project-manifest text files are copied. Hidden folders, common dependency/build folders, symbolic links/reparse points, likely secret filenames, binaries, empty files, and files above 1 MiB are excluded. Bounds are 2,000 files, 32 MiB total, 1 MiB per file, and 12 directory levels.

The snapshot is not an execution boundary. No project code is launched by snapshot preparation. The original project is not mapped to a sandbox and is not modified.

## Microsoft Windows Sandbox facts that affect the design

Primary references:

- [Windows Sandbox overview](https://learn.microsoft.com/en-us/windows/security/application-security/application-isolation/windows-sandbox/windows-sandbox-overview)
- [Configure Windows Sandbox with a .wsb file](https://learn.microsoft.com/en-us/windows/security/application-security/application-isolation/windows-sandbox/windows-sandbox-configure-using-wsb-file)
- [Windows Sandbox sample configuration files](https://learn.microsoft.com/en-us/windows/security/application-security/application-isolation/windows-sandbox/windows-sandbox-sample-configuration)
- [Windows Sandbox command-line interface](https://learn.microsoft.com/en-us/windows/security/application-security/application-isolation/windows-sandbox/windows-sandbox-cli)
- [Install Windows Sandbox](https://learn.microsoft.com/en-us/windows/security/application-security/application-isolation/windows-sandbox/windows-sandbox-install)

Important constraints:

1. Windows Sandbox networking is enabled by default. A generated configuration must explicitly disable it; absence of a setting is not safe.
2. Mapped host folders are a security boundary. Never map the original selected project. Map only the FRIDAY-created snapshot as read-only. Any writable result folder must be a separate, empty, per-run directory with bounded collection and strict path/reparse-point validation.
3. Disable vGPU, clipboard, audio input, video input, and printer redirection unless a future reviewed requirement proves one is necessary.
4. The documented .wsb configuration supports a memory cap through `MemoryInMB`, with a minimum effective value of 2,048 MB. The documented configuration surface does not expose a CPU quota, process timeout, or host-output disk quota.
5. The newer Windows Sandbox app introduced with Windows 11 version 24H2 provides the `wsb` CLI for starting, listing, executing in, connecting to, sharing with, and stopping Sandbox sessions. The newer app is a Store-delivered update and may not be present merely because the host is on 24H2; Microsoft's FAQ recommends `wsb --version` to distinguish the newer CLI from the older app. Microsoft documents that `wsb exec` currently does not provide process I/O, and that an active user session is needed for `ExistingLogin` execution. FRIDAY must not assume stdout/stderr can be captured directly through that command.
6. Windows Sandbox is not available on Windows Home. Microsoft lists Pro, Enterprise, and Education editions as supported; hardware virtualization and minimum host resources are also prerequisites.
7. Windows Sandbox permits only one running instance at a time. FRIDAY must detect an existing session and refuse to attach to or stop a Sandbox it did not create.
8. Closing a Sandbox discards its guest state, but host folders explicitly mapped into it remain host data. Therefore a writable output mount must be treated as untrusted input after the guest stops.

## Consequences for FRIDAY implementation

- A detected `WindowsSandbox.exe` alone is not proof that a usable, policy-permitted Sandbox can be launched. Preflight must validate the supported Windows edition/build, required feature and CLI availability, and other observable prerequisites before any launch attempt.
- A future launcher must use only a generated, schema-validated configuration with networking disabled and no original-project mount.
- Test commands must be selected from a fixed backend allowlist. Never pass model-generated shell text, user-supplied command strings, or arbitrary executable paths to `wsb exec`.
- Since `wsb exec` does not return process output, any future output collection needs a dedicated per-run result directory and a small trusted guest-side runner. Result files must be bounded, regular files only, reparse-point safe, schema-validated, and treated as untrusted.
- A watchdog may stop the entire Sandbox session after a deadline. This is a session-level kill, not proof of a per-process CPU quota.
- Native Windows Sandbox does not document an enforceable CPU quota or writable mapped-folder disk quota. FRIDAY must not claim those limits are enforced by .wsb. Before execution can be enabled, either a separately verified OS-enforced control must be designed for those dimensions or the feature must clearly refuse the unsupported execution mode.
- A guest needs test runtimes and dependencies. Host-installed tools are not automatically available inside a clean Sandbox, and networking must remain disabled. FRIDAY must not silently download packages or fall back to host execution.

## Gate before enabling execution

Execution remains disabled until all of the following are implemented and verified:

- Strong preflight for Windows edition/build, Sandbox feature/CLI version, virtualization and policy constraints, including detection of an already-running Sandbox session.
- Generated configuration with networking, vGPU, clipboard and device redirection disabled.
- Read-only snapshot mount only; separate, empty output mount with restrictive ACLs.
- Fixed command allowlist and trusted guest runner; no arbitrary shell strings.
- Real OS-enforced resource controls, deadline watchdog, bounded output collection and reliable cancellation/stop.
- Recovery and cleanup for app crashes, interrupted launches, stale snapshots, and partial result directories.
- Regression tests for malformed configs, path traversal, reparse points, allowlist bypasses, timeout, cancellation, oversized output, invalid results, and cleanup failures.
- Windows acceptance testing on supported Windows editions.

Until this gate is met, FRIDAY may prepare and discard snapshots but must not execute project code.
