# Coding Assistant — adaptive guest memory policy

## Scope and status

The host-side resource-profile calculator is implemented as a pure, deterministic policy model. It accepts an injected physical-memory snapshot and returns a bounded guest memory budget or a fail-closed reason. It does not query Windows, create/configure a Job Object, launch a guest, or enable Coding Assistant execution.

A future Windows launcher must collect a fresh and trustworthy memory snapshot immediately before launch, calculate the budget, apply the returned limits to the guest process Job Object before any untrusted process is resumed, and verify the configured limits. If memory detection or enforcement fails, guest execution must remain unavailable.

## Profiles

The tier thresholds account for the fact that Windows may report slightly less usable physical memory than the nominal installed-memory label:

| Approximate device tier | Minimum reported physical memory | Maximum Job Object memory | Maximum per-process memory |
| --- | ---: | ---: | ---: |
| 4 GB | 3.5 GiB | 1 GiB | 512 MiB |
| 8 GB | 7 GiB | 2 GiB | 1 GiB |
| 16 GB | 14 GiB | 4 GiB | 2 GiB |

Hosts reporting less than 3.5 GiB physical memory are rejected by this initial policy model rather than silently receiving a profile that has not been reviewed.

## Live available-memory guard

The calculator subtracts a fixed 512 MiB reserve from the host's currently available physical memory, then chooses the smaller of the remaining amount and the selected profile's Job Object ceiling. The per-process limit is the smaller of the profile ceiling and the resulting total Job Object budget.

A result below 256 MiB is rejected as insufficient. This avoids starting a guest when its budget would be too small to be useful while also preserving a minimum amount of currently available memory for Windows and FRIDAY. Invalid snapshots (zero values or available memory greater than total physical memory) are rejected.

The absolute ceilings are 4 GiB per Job Object and 2 GiB per process, regardless of host RAM. More powerful machines therefore do not receive unbounded guest memory.

## Windows Job Object integration boundary

The Windows resource module now queries physical memory through `GlobalMemoryStatusEx` immediately before creating a guest Job Object. API failure returns an explicit error; there is no guessed tier or fallback budget. The injected snapshot is passed through the same pure calculator described above.

The resulting job and per-process memory values are converted into the Windows Job Object policy. A second validation boundary rejects values below the minimum useful job budget, above the 4 GiB job / 2 GiB process absolute ceilings, or where the per-process limit exceeds the aggregate job limit. The existing fixed active-process ceiling and CPU hard cap are retained unchanged. The Job Object configuration path then applies and reads back every required kernel limit before a caller can assign a suspended process.

This is still a resource-containment primitive, not an enabled guest launcher: no new guest process is created by the memory provider, and this change does not authorize or resume guest execution. A future launcher must call the adaptive creation path and must not resume an untrusted process unless Job Object creation, configuration, readback, and process assignment all succeed.

Windows CI now exercises the live memory query and adaptive-policy conversion. Physical Windows acceptance is still required for low-memory devices, memory pressure, API failure, Job Object configuration failure, and the full guest lifecycle. The profile numbers are conservative initial policy ceilings, not measured performance guarantees or a promise that every 4 GB PC can run every supported test workload. CPU, storage, Windows edition, background applications, and runtime requirements still affect whether a run can proceed.
