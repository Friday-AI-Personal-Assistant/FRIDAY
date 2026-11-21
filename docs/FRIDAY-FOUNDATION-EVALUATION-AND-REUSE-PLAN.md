# FRIDAY Foundation Evaluation & Reuse-First Engineering Plan

**Status:** Active comparative evaluation; foundation selection is NOT final  
**Evaluation branch:** `foundation/evaluation`  
**Preserved baseline:** `feature/phase-2-skills` at `85dbace48595c1600bb152cb511f65572d53706d` (#1056)  
**Product acceptance order:** Safety → User control → Cost transparency → Time saved

## Executive decision

Do not replace FRIDAY with another repository and do not import an entire assistant wholesale.

The current leading candidate for deeper evaluation is **OpenJarvis**. Its modular Python/Rust backend, local-first inference direction, Tauri desktop, tools, skills, memory, security modules and tests offer substantial reusable engineering. However, its package is explicitly classified as **Alpha**, and source inspection found defaults and execution paths that do not meet FRIDAY's required security posture without adaptation.

**Current recommendation:** evaluate OpenJarvis as a source of reusable components and architectural patterns. Do not yet select it as FRIDAY's application foundation, enable its agents/tools, or ship its desktop app.

The Thoth project remains a secondary reference, not a selected dependency. The inspected `zeroitp/Thoth` repository identifies itself as a fork whose GitHub parent is `siddsachar/row-bot`; its README badges and links refer to `siddsachar/Thoth`, while GitHub's API redirects that repository identity to `siddsachar/row-bot`. Treat this as a repository rename/identity transition that must be pinned and verified before citing a canonical source. The fork's dependency set is broad and mostly unpinned in `requirements.txt`; no reuse approval follows from the Apache-2.0 repository metadata.

The previously mentioned Personal Jarvis candidate could not be reliably resolved to a canonical public repository during this pass. It is not ranked until the exact upstream repository and license can be verified.

## Candidate observations

### OpenJarvis

Source: https://github.com/open-jarvis/OpenJarvis

- Repository metadata reports Apache-2.0, active development, and a substantial modular codebase.
- The package metadata marks development status as Alpha and supports Python >=3.11,<3.14.
- Includes Python and Rust components, a Tauri desktop shell, a frontend, local Ollama inference, optional cloud engines, agent/tool infrastructure, memory, scheduler, skills, speech, telemetry and tests.
- Has explicit security modules for capability policies, file sensitivity, outbound-boundary scanning, guardrails, signing, SSRF, taint, audit and subprocess execution.
- Has a large security test suite; this is positive evidence of engineering investment, not proof of security.
- The README describes Windows installation and packaged desktop releases. Physical Windows installation and runtime behavior still need independent acceptance testing.

#### Source-level risks requiring adaptation

1. `CapabilityPolicy` documents `default_deny=False` as its default. FRIDAY must use deny-by-default and test it at every tool-dispatch boundary.
2. `SkillExecutor` documents `allowed_capabilities=None` as “no capability policy”; in that state skills execute without capability validation. FRIDAY must never pass or inherit this permissive state.
3. `run_sandboxed` and `shell_exec` invoke caller-provided commands using `shell=True`. Environment filtering and timeouts are useful controls but are not an OS security sandbox. FRIDAY must keep host shell execution disabled until a separately verified isolation boundary exists.
4. `file_write` permits unrestricted paths when no allowed-directory list is configured. FRIDAY must always supply explicit, user-authorized workspace roots and enforce them below the UI layer.
5. Skill trust tiers and capability declarations are useful primitives, but provenance/signature classification is not equivalent to a user granting a particular skill specific permissions.
6. Boundary scanning is useful defense-in-depth, not a guarantee that sensitive data cannot leave the device. The default scanner path can degrade if the Rust-backed scanners are unavailable; failure behavior must be reviewed and made fail-closed for protected operations.
7. Analytics uses PostHog and a persistent anonymous installation identifier when enabled. Documentation describes explicit opt-in and redaction, but frontend and installer paths, defaults, endpoints, and network behavior must be audited. FRIDAY telemetry must be disabled by default, opt-in, visible, and removable.
8. Apache-2.0 at repository level does not settle licenses for all dependencies, bundled models, voices, fonts, icons, frontend assets, or optional providers. A complete SBOM and notices review is required before redistribution.

### Thoth (secondary reference only)

Source inspected: https://github.com/zeroitp/Thoth

- The inspected repository declares Apache-2.0 and describes local-first Windows/macOS desktop use, Ollama, optional cloud providers, voice, workflows, document intelligence, graph memory and integrations.
- It is marked as a fork in repository metadata; its README points to a different canonical repository. Evaluate the upstream repository, not just this fork, before considering reuse.
- Its `requirements.txt` contains a large mixed dependency set, with many dependencies not version-pinned. This increases reproducibility, supply-chain and license-audit work.
- The README makes claims about sandboxed plugins, approvals and secure credential storage. These claims have not yet been independently validated in source.
- Do not import its shell, browser, tunnel, messaging, health, or computer-use features without individual threat-model and permission reviews.

### Row-Bot / Thoth upstream candidate (new; not yet approved)

Sources: https://github.com/siddsachar/row-bot and the `zeroitp/Thoth` fork.

- GitHub repository metadata identifies `siddsachar/row-bot` as the non-fork upstream parent of `zeroitp/Thoth`; the fork README presents the product as Thoth and links its release/CI badges to `siddsachar/Thoth`, which GitHub currently redirects to the `row-bot` repository identity.
- The upstream repository metadata reports Apache-2.0, active development, Python as its primary language, Windows/macOS desktop packaging claims, local Ollama support and optional cloud providers. These are repository/README claims, not verified runtime acceptance.
- The project appears substantially broader and more actively maintained than the earlier `zeroitp/Thoth` fork snapshot. It is a serious candidate for a deeper architecture and component-level reuse review, not an automatic foundation choice.
- Its advertised capabilities include agent/tool execution, shell, filesystem, browser automation, messaging channels, workflows, plugins/MCP and background tasks. These are high-risk surfaces. FRIDAY must not inherit their permission model without direct source review and adversarial tests.
- The repository is large and Python/LangGraph-oriented, while FRIDAY already has a Rust/Tauri/React architecture. Evaluate selective components and integration cost; do not add a second full desktop/backend stack by default.
- Source review has begun. Positive evidence: a central approval policy defaults missing/unknown stored approval levels to Ask; the documented developer network gate uses a per-workspace internal network and a separate constrained egress proxy, and fails without network when the gate cannot start; the project has a substantial `uv.lock` with hashes and explicit Python constraints; and a dedicated security policy discloses a prior security advisory rather than hiding it.
- Important risks and costs: the current package requires Python >=3.12,<3.14; the core dependency set is already broad (FastAPI, LangChain/LangGraph, Ollama, provider SDKs, scheduler, desktop wrapper and more), with large optional extras for voice, browser, channels, MCP, embeddings and media. This conflicts with a small low-resource FRIDAY core if adopted wholesale.
- Security policy records GHSA-68j6-243q-h2hp (published 2026-11-13): an unreadable tool settings file re-enabled switched-off tools and cleared the shell blocked-commands list; fixed in 5.3.3. This is a concrete configuration-failure regression to inspect in current source and tests. It is not by itself a disqualification, but fail-closed configuration loading and regression coverage are mandatory reuse gates.
- Computer-use security documentation says the 5.3 rebuild removed an earlier verdict layer and relies on visibility, interruption and moment-of-action approvals; it explicitly notes residual risks and that the upstream Cua telemetry remains enabled after disclosure. FRIDAY's policy requires independent opt-in, clear disclosure and a user-controlled telemetry choice, so this integration cannot be adopted unchanged.
- Shell tooling runs commands on the user's machine and classifies commands using blocklists/allowlists plus approval levels. This is not a general-purpose OS sandbox. Any FRIDAY reuse must keep shell disabled unless a separate isolation boundary and adversarial validation are proven.
- The developer egress design is promising but Docker/Podman and host networking assumptions must be tested on supported Windows configurations. It is not evidence that all Row-Bot tools/plugins are sandboxed.
- Required next checks: pin the exact current commit; trace all tool registration and dispatch call sites; inspect the fix and tests for GHSA-68j6-243q-h2hp; inspect filesystem boundary and plugin/MCP permission enforcement; verify Docker/Podman network gate and sandbox lifecycle on Windows; inspect workflow persistence/cancellation, credential storage, telemetry/network behavior, updater/signing, installer scripts, SBOM/license inventory, bundled assets/models and actual Windows runtime behavior.

### Personal Jarvis

Not ranked. The earlier candidate reference did not resolve cleanly to a canonical repository during this inspection. Do not rely on a name match or a README snippet as a license or security review.

## FRIDAY build sequence

This is the product sequence. It deliberately puts the permission and cost model before powerful integrations.

| Order | Milestone | Scope and exit condition |
|---|---|---|
| 0 | Foundation evaluation | Compare source, architecture, Windows support, maintenance, licenses, dependencies, privacy and threat model. Select only components with clear reuse value. |
| 1 | FRIDAY platform shell | Stable desktop app, navigation, settings, diagnostics, update strategy and packaging. Preserve FRIDAY identity and existing code. |
| 2 | Local-first assistant core | Chat, conversation history, provider/model catalogue, Ollama/local model setup and explicit optional cloud providers. No silent cloud fallback. |
| 3 | Privacy, permissions and cost centre | Deny-by-default capability model, scoped/revocable grants, approval UX, audit log, network/telemetry controls, cost disclosure and secret storage. Must pass security tests before enabling tools. |
| 4 | Memory and personal knowledge | User-controlled local memory, inspect/edit/export/delete, source provenance and retention controls. |
| 5 | Safe skills and tools | Signed/provenance-aware skills, allowlisted tools, per-action capability checks, isolated execution, safe defaults and clear approval tiers. Untrusted imports stay disabled. |
| 6 | Coding Assistant integration | Reuse the existing FRIDAY Coding Assistant work where sound; evaluate OpenJarvis coding components separately. Keep execution disabled until real containment and Windows acceptance tests pass. |
| 7 | Excel Intelligence | Spreadsheet read/analysis, formula and data-quality assistance, previews and user-approved writes; use mature spreadsheet libraries rather than a bespoke engine. |
| 8 | Documents and Web Research | PDF/document ingestion, citations, source tracking, bounded web access and explicit outbound-data policy. |
| 9 | Voice and personal automation | Local speech where practical, user-visible schedules, cancellable jobs and explicit permissions for external actions. |
| 10 | Job Hunter | CV/profile workspace, job discovery, matching, application drafts and user-approved submissions. No automatic application submission by default. |
| 11 | Crypto Intelligence | Market data, research, alerts, backtesting and paper trading first. Live trading is a separate future product gate requiring independent risk, key custody, exchange and approval reviews. |

## Reuse rules

- Prefer maintained, permissively licensed components with reproducible builds and clear provenance.
- Reuse libraries and isolated subsystems; do not merge two complete assistant applications into one.
- Keep FRIDAY's architecture, UX, product charter and security enforcement authoritative.
- Pin dependencies, generate an SBOM, scan advisories and retain all required license/NOTICE attribution.
- Audit models, voice datasets, fonts, icons, web assets and provider terms separately from source-code licenses.
- Do not permit a plugin/skill to grant itself permissions. User grants must be explicit, scoped, revocable and enforced by backend code.
- No hidden telemetry, cloud fallback, paid API calls, tunnel exposure or remote actions.
- Keep risky tools disabled until adversarial tests and platform-specific acceptance tests pass.

## Existing FRIDAY work to preserve

The existing `feature/phase-2-skills` branch remains the source of truth for current FRIDAY work. The latest known baseline is #1056 at `85dbace`, with its recorded Windows and Linux CI run successful.

The Coding Assistant is not discarded. Its bounded project reading, analysis, diff preview, approval, backup, verification and undo work should be inventoried and retained. Its Stage 4 execution/sandbox integration remains incomplete and disabled. The 61 outstanding checklist actions are not automatically cancelled by this foundation review; they should be reclassified as keep, adapt, replace or defer after the architecture decision.

## Next evaluation tasks

1. Resolve the canonical Thoth upstream and inspect its permission enforcement, plugin sandbox, shell/browser execution, credential store, updater, telemetry and Windows packaging.
2. Resolve the exact Personal Jarvis upstream if it remains a serious candidate; verify license and maintenance status.
3. Inspect OpenJarvis end-to-end tool dispatch, runtime policy wiring, skill import/execution, filesystem boundaries, desktop frontend, installer scripts, telemetry defaults, update mechanism and Windows process isolation.
4. Produce a dependency/license inventory for the candidate components actually proposed for reuse.
5. Compare all candidates against FRIDAY's acceptance criteria and choose a narrow integration strategy.
6. Only after that, begin a small, reversible prototype on a dedicated branch and run CI. Do not modify or merge the preserved feature branch as part of evaluation.

## Decision status

**No upstream project has been approved for wholesale adoption.** OpenJarvis is the leading candidate for component-level reuse pending the remaining security, privacy, license and Windows acceptance work.


## Evaluation checkpoint — 2026-11-20

### Repository and CI verification

- [x] Verified the four-guarantees documentation commit on `feature/phase-2-skills`: `db57764283e449d71eb73e8d045914abc6940c53`.
- [x] Verified Actions run #1057 (`44767368952`) completed successfully. Linux unit tests passed; Windows frontend build, Rust tests and installer build passed. This is hosted CI evidence, not physical-device acceptance.
- [x] Confirmed the evaluation workspace can read its own roadmap and master checklist on `foundation/evaluation`. The existing development branch remains separate and untouched by this evaluation update.

### New candidate lead and current decision

- [x] Traced `zeroitp/Thoth` fork metadata to parent `siddsachar/row-bot`.
- [x] Confirmed the upstream repository currently identifies as `siddsachar/row-bot`, reports Apache-2.0, and is active. The fork README uses Thoth branding and links to the old `siddsachar/Thoth` identity, which GitHub redirects to the Row-Bot repository.
- [ ] Pin the exact upstream commit and inspect the source-level permission, execution, plugin, workflow, credential, telemetry, updater and Windows packaging boundaries.
- [ ] Reconcile the repository rename/identity in the audit record before treating Thoth and Row-Bot as one canonical candidate.
- [ ] Do not import or execute Row-Bot/Thoth code in FRIDAY during evaluation.

### Revised candidate evaluation order

1. **Row-Bot / Thoth upstream:** deep review first because it is the newly identified active upstream behind the previously inspected Thoth fork.
2. **OpenJarvis:** retain as a serious component-reuse candidate; its Rust engine production integration remains NO-GO based on the already recorded compatibility audit, but other subsystems may still offer value.
3. **Personal Jarvis:** remain unranked until the exact project reference is supplied or resolved without ambiguity.

### Engineering guardrails

This evaluation branch is a planning, source-inspection and isolated-prototype workspace. Do not merge to `main`, overwrite `feature/phase-2-skills`, replace FRIDAY wholesale, enable external agent tools, or introduce new runtime dependencies until the relevant four-guarantee release gates have evidence.


### Row-Bot source-review checkpoint (2026-11-20; preliminary)

Inspected source files at Row-Bot commit `547116cfd03b4b391813e8e2d638ffc22b8255ff` (2026-11-20; `main` at time of inspection), including:
- `SECURITY.md` (blob `8dece46f74733ef1e27aeeec316726ba2a3b8735`)
- `docs/COMPUTER_USE_SECURITY.md` (blob `4058ca16f021acdc0d336ffaeb1b8cd31fad0089`)
- `src/row_bot/access/policy.py` (blob `06e909eb645c89a3f410acc7f9864658ee2610db`)
- `src/row_bot/access/runtime_policy.py` (blob `8300cf81e1d221522e8553c231b41718e12639e3`)
- `src/row_bot/approval_policy.py` (blob `ce30774cef0e26c82b17a9c1d7ac18af5a200660`)
- `src/row_bot/developer/sandbox.py` (blob `1f13dffaf7cdf63a2af569c7facf31783c84b74b`)
- `src/row_bot/developer/sandbox_egress.py` (blob `7591fba10ffd5fb73b3d5bb96c4c33c1d322e718`)
- `src/row_bot/tools/shell_tool.py` (blob `10c16d881b1195d135e9bd1adfdcbf2b012bcf1a`)
- `src/row_bot/application/capability_policy_controls.py` (blob `9ad2647cfbc52a7a1d12330f7ab2123fb05a60de`)
- `pyproject.toml` (blob `f0e9a25b86dad0ce8d6c1e2c64446e642d4a4c72`)
- `uv.lock` (blob `17bc1ab8bd412abd709377ded400d47e3195060d`)

**Preliminary assessment:** Row-Bot currently has the strongest visible investment in approvals, user-facing controls and a constrained developer network-egress design among the candidates inspected so far. That is a reason to continue source review, not proof that its controls satisfy FRIDAY. Its shell remains host command execution, computer-use documentation retains upstream telemetry, and the disclosed tool-settings fail-open advisory needs careful regression analysis. No Row-Bot code is approved for import at this checkpoint.


### Row-Bot advisory regression review (2026-11-20)

**Reviewed source revision:** `siddsachar/row-bot@547116cfd03b4b391813e8e2d638ffc22b8255ff`.

**Advisory:** GHSA-68j6-243q-h2hp, published 2026-11-13, fixed in 5.3.3 according to the upstream `SECURITY.md`.

**What the failure could do:** if `tools_config.json` became unreadable, the old behavior could restore default-enabled tools and lose the user's shell blocked-command settings. That is a serious user-control regression even though upstream rates the advisory Low.

**Evidence of the current fix:**
- `src/row_bot/tools/registry.py` treats a failed settings read as `None`; a tool without previously loaded settings is registered disabled rather than falling back to its default-enabled state.
- If the registry had already loaded the user's settings, it retains the loaded enablement and tool configuration when a later reload finds the file unreadable. It does not clear the user's known blocked-command list.
- `src/row_bot/tool_configuration.py` uses a bounded strict read, rejects malformed/duplicate-key/non-regular/symlink-like input, verifies file identity around the read, and raises a stable unavailable error rather than returning partial settings.
- `tests/subsystem/tools/test_tool_registry_saved_reload.py` covers truncated, half-written and non-file settings; it asserts that switched-off tools remain off, the `curl` blocked-command setting remains intact when it was already loaded, and a fresh startup with unreadable settings leaves tools off. It also distinguishes a genuinely missing first-run settings file (defaults apply) from an unreadable existing file (fail closed).
- `frontend/src/features/settings/ToolSettingsUnreadable.test.tsx` verifies the user is told every tool is off while settings are unreadable, and that reset-to-defaults is a separate, explicit confirmation. The dialog warns that blocked commands are cleared.

**Assessment:** The specific advisory appears credibly addressed in the inspected source and regression tests. This is a positive signal for configuration failure handling. It is still source inspection, not an independently executed test run or a full audit of all tool execution paths. Resetting unreadable settings intentionally clears blocked commands after explicit confirmation; FRIDAY should make such a reset equally explicit and should never silently discard a user's safety preferences.

**Next source-review question:** prove that every execution route (built-in, MCP, plugin, custom tool, channel, background workflow and computer use) checks the effective user/profile/workspace policy at the point of execution, and that stale approvals cannot be reused after permissions or configuration change.


### Row-Bot execution-boundary review (2026-11-20; source-only)

**Reviewed revision:** `siddsachar/row-bot@547116cfd03b4b391813e8e2d638ffc22b8255ff).

**Positive evidence**
- `src/row_bot/tools/profile_policy.py` documents action-level restrictions at canonical tool dispatch. It classifies only exact registered source/operation pairs; unknown actions are not promoted to read-only by name or user-supplied descriptions. It checks MCP effects from the existing runtime catalog rather than trusting worker-supplied metadata.
- `src/row_bot/mcp_client/safety.py` classifies tool names and descriptions, combines declared hints and destructive metadata, and treats unknown or destructive tools conservatively in the MCP policy layer. The policy controls reviewed earlier default uncertain tools to disabled/locked and require approval.
- Plugin code is run in a separate owned worker process with a bounded JSON protocol, environment filtering, timeouts, cancellation/ownership logic and a Windows Job Object. The worker revalidates its prepared source/environment receipt at execution and callback boundaries; stale preparation revokes its API.
- The plugin worker explicitly states it is **not** an OS filesystem or network sandbox. Its separate process and private virtual environment are useful isolation from the host Python process, but must not be mistaken for containment against arbitrary malicious plugin code.
- `src/row_bot/computer_use/policy.py` has explicit refusals for password managers, protected screens, typing into password/one-time-code/card fields, and dangerous system key combinations. Computer use is limited to interactive local chat; channel chat is refused.

**Important limitations / unresolved questions**
- Row-Bot exposes a user-selectable Hands-off approval mode. Its own policy says this can approve actions without asking, including consequential actions unless separately marked as person-only. FRIDAY's default must remain user-controlled and must not silently adopt this autonomy model; any optional autonomy would need narrower per-action scopes and explicit, revocable grants.
- Plugin subprocess isolation is not a true filesystem/network sandbox. A plugin with arbitrary Python code may still have the host user's OS permissions unless the worker is further constrained by OS-level controls. Do not import this as a trusted security boundary.
- MCP tool classification depends partly on names, descriptions and server-provided hints. The code has conservative heuristics and locks unknown/destructive effects, but metadata-based classification is not a proof of benign behavior. Treat all remote MCP servers and their results as untrusted.
- Computer use's own architecture document explicitly says the prior extra verdict layer was removed in the 5.3 rebuild. Its safety model relies on visibility, interruption and action approvals; the upstream Cua telemetry remains enabled after a disclosure. FRIDAY's stricter user-control and privacy requirements mean no direct adoption without a separate decision, and telemetry must be independently opt-in.
- The audit has not yet traced every tool wrapper and dispatch middleware call site end-to-end. The existence of a canonical policy module does not prove every execution path is wired through it.

**Preliminary decision:** Row-Bot is a strong source of design patterns for fail-closed settings, approval records, canonical dispatch policy, and constrained developer network egress. It is **not** a safe drop-in foundation: its broad dependency stack, host shell, plugin worker without OS sandboxing, optional Hands-off mode, MCP metadata uncertainty and computer-use telemetry all need adaptation or exclusion. Continue selective component evaluation only; no code import approved.

**Next work:** inspect canonical agent middleware and tool wrappers to prove enforcement coverage, then compare these reusable patterns against FRIDAY's existing Rust/Tauri implementation and OpenJarvis before choosing a minimal prototype.


## OpenJarvis comparative source review (2026-11-20; preliminary)

**Reviewed upstream revision:** `open-jarvis/OpenJarvis@a9e59e8770f92dd8046c2e5fcc1246eef51bb96a` (Apache-2.0 repository metadata).

**Useful reusable ideas**
- A central Rust tool executor exists, with clear extension points for capabilities, taint checks, timeouts and event recording.
- File sensitivity checks cover common credential/key names, extensions, case variations and symlink targets, with dedicated tests.
- A Docker sandbox runner exists with `--network none`, read-only bind mounts and mount allowlist validation; this is a promising pattern to study, not proof of production containment on FRIDAY's supported Windows configurations.
- Analytics has an explicit opt-in flow, allowlisted events and redaction, and documentation says configurations without an analytics setting default off.
- The project has substantial automated security/sandbox tests and lockfiles across Rust/Python/frontend packages.

**Critical blockers for direct reuse**
- Rust `CapabilityPolicy::default()` calls `new(false)`: missing agent policy allows capabilities by default. The source comments explicitly document this behavior, and unit tests assert it. FRIDAY requires deny-by-default.
- Rust `ToolExecutor::execute` performs capability checks only when both a policy and an agent ID are supplied. If either is absent, that check is skipped. This makes integration wiring and mandatory context enforcement critical; FRIDAY cannot adopt this executor unchanged.
- The Rust executor shown calls `tool.execute(params)` synchronously and checks elapsed time only after it returns. That elapsed-time check does not itself interrupt or contain a hung tool.
- The shell tool directly launches host commands. Its tool spec requires `code:execute` and says confirmation is required, but these metadata flags are not a substitute for FRIDAY's own mandatory backend approval and a real OS sandbox.
- The Docker runner is an optional wrapper around agent execution. It is not proof every built-in tool or every skill path is sandboxed, and it needs Windows/container runtime acceptance testing.
- The analytics opt-in is better aligned with FRIDAY's cost/privacy guarantees than a default-on design, but it remains external telemetry and must be separately opt-in in FRIDAY, with no hidden installation identifier or network call.
- Repository Apache-2.0 metadata does not clear every dependency, model, voice asset, font, icon or provider term. Full SBOM and asset-license review remains required.

**Preliminary decision:** OpenJarvis is useful for selective patterns (file sensitivity tests, event redaction/consent UX, and sandbox design research). Its current capability defaults and optional enforcement context make its Rust executor a **NO-GO for direct reuse** until rewritten/adapted to mandatory deny-by-default policy and required identity/context. No code import approved.

**Comparison so far:** Row-Bot shows more elaborate approval and configuration-recovery flows; OpenJarvis has a simpler Rust-centric architecture closer to FRIDAY's existing technology choices. Neither is approved as a wholesale foundation. Continue comparing individual components against FRIDAY's own implementation, then build a tiny isolated proof of concept only after selecting a bounded component with a clear testable benefit.


## FRIDAY baseline architecture review (2026-11-20)

**Reviewed baseline:** `foundation/evaluation` commit `532a9d95687b01d99015d3d8fb178cdb51c00436` (same preserved #1056-era application baseline plus evaluation documentation).

### What FRIDAY already has

- A compact Tauri 2 + React desktop app with a Rust backend. The current runtime dependency set is substantially smaller than either candidate assistant's full stack.
- Local Ollama chat integration with request-size bounds, model-name validation, connection/response timeouts and streaming support.
- Encrypted local memory and encrypted tool activity history using the existing OS credential/keyring-backed encryption-key design.
- A small explicit built-in tool registry (currently calculator and a fixed allowlist of desktop destinations) with a pending-approval mechanism.
- A curated skills registry. Skills are installed/loaded as bounded instruction packages; the current Coding Assistant skill is explicitly read-only and prohibits shell, build, test and network execution.
- Existing Web Research, PDF/file assistant and Coding Assistant modules, plus a significant Rust filesystem-boundary and Windows Sandbox preparation implementation.
- Minimal Tauri main-window permissions (`core:default`) and a written privacy/security policy.

### What this means for the foundation choice

FRIDAY already has a lightweight native foundation aligned with local-first ownership. Replacing it wholesale with Row-Bot or OpenJarvis would introduce major dependency/runtime complexity and duplicate core systems before proving a user benefit.

**Decision:** retain FRIDAY's Tauri/Rust/React architecture as the product foundation. Reuse from external projects only small, independently reviewed components, test patterns or designs that demonstrably improve FRIDAY. Do not port an entire assistant backend or adopt another project's tool execution framework.

### Immediate gaps to address in FRIDAY itself

- The current tool set is intentionally tiny; tool permission grants and approvals need to become a coherent, backend-enforced, deny-by-default capability system before adding powerful tools.
- The skill registry must keep install, enablement and permission grants separate. A skill's prompt/manifest must never grant its own authority.
- Atomic cross-platform file replacement is now integrated into memory, chat history, the skills registry, tool activity history and private workspace notes (#1066–#1074). Backup/restore and broader crash-recovery guarantees remain separate work.
- Provider/model management, cost estimates/budgets, local/cloud separation and explicit no-silent-fallback behavior need a first-class design.
- Add durable, inspectable permission records, revocation, per-action audit trails and background-task stop controls before adding agents, channels or automation.
- Keep arbitrary shell, generic plugin execution, unattended computer control, paid cloud fallback and live financial actions disabled until their individual boundaries pass the four-guarantee release gate.

**Foundation direction:** build FRIDAY forward on its existing native shell. The next implementation should be a small native capability/approval foundation (design + tests first), not importing either candidate's agent runtime. Before coding it, finish a focused audit of current `tools.rs`, skill enablement and Tauri command exposure so we extend existing ownership safely.


### FRIDAY native core implementation notes (source audit)

Reviewed `src-tauri/src/tools.rs`, `src-tauri/src/skills.rs`, `src-tauri/src/assistant.rs`, `src-tauri/src/memory.rs`, `src-tauri/Cargo.toml` and Tauri main-window capabilities at the evaluation baseline.

- **Tool approvals:** the existing implementation creates cryptographically random, 120-second, one-use tokens bound to the exact tool ID and JSON arguments; mismatches consume the token and fail. This is a sound small foundation to preserve and generalize. The current tool catalogue is intentionally only two tools.
- **Skills:** installation, enablement and loading are distinct operations. Built-in skill packages are verified against the expected manifest and instruction SHA-256; disabling/unloading file and coding skills revokes their session access. Loading adds bounded instructions to session memory; the current skill system does not execute arbitrary skill code or connect skill-declared tools. Permission labels are therefore descriptive metadata today, not a general grant engine.
- **Assistant:** the current chat backend is local Ollama at loopback and enforces model/message count and size bounds plus connection/response timeouts. It is not yet a multi-provider manager or cloud-cost system.
- **Memory and activity:** local memory and tool activity are encrypted, but current replacement logic removes the prior destination before renaming a temporary file. This creates a crash window and should be hardened with atomic replacement/recovery before broader persistent state is added.
- **Desktop permissions:** the Tauri main window has only the default core permission set; retain this minimal capability model as the app grows.
- **Coding Assistant:** the current skill instructions remain read-only; separate filesystem and Windows Sandbox groundwork exists, but it must not be treated as an enabled execution environment.

**Conclusion:** preserve the existing one-use approval token mechanism, exact built-in skill catalogue/integrity checks, local Ollama path, encryption/keyring integration, and minimal Tauri permissions. Extend them behind a coherent backend policy rather than replacing them with an external agent framework. Prioritize atomic persistence hardening and a deny-by-default permission model before adding more powerful tools.


### FRIDAY Tauri command-surface check

Reviewed `src-tauri/src/lib.rs` at the same evaluation revision. The command handler explicitly registers assistant, tool, skill, memory, workspace, file, web-research and Coding Assistant operations. This is preferable to a dynamically exposed plugin command surface, but each registered command remains a public IPC entry point and must enforce its own backend checks; frontend hiding is not authorization.

For the first capability-system increment, keep the surface deliberately small:
- centralize metadata for built-in tool IDs and required capability names;
- require an explicit, backend-validated capability decision for every execution path;
- preserve exact argument-bound one-use approval tokens for consequential actions;
- default unknown tool IDs, missing context, invalid grants and policy-read failures to deny;
- do not let skill manifests or model-generated tool calls create grants;
- do not add broad filesystem, shell, browser-control, channel-send or paid-provider grants in this increment.

The first prototype should be limited to the existing two built-in tools, with tests proving that unknown tools and missing grants fail closed and that the existing explicit approval flow remains one-use and argument-bound.


### Native capability-policy implementation — first increment

**Branch checkpoint:** `foundation/evaluation`, based on #1074 (`8721ae38b981d1026d4eb185553a0c83a36035ce`).

The first implementation adds a small Rust-owned capability policy for the two existing built-in tools:

- `calculator.evaluate` is explicitly mapped to `local.compute` and remains limited to local arithmetic without an approval token.
- `desktop.open_target` is explicitly mapped to `desktop.launch` and continues to require the existing cryptographically random, exact-arguments, one-use, 120-second approval token.
- The policy gateway rejects unknown tool IDs, capability mismatches and any attempt to weaken the backend's approval requirement. The UI's catalogue metadata is not the authority.
- Regression tests cover known allowed behavior, consequential-action approval, unknown-tool denial and declaration-tampering attempts.

**Scope and honest limitation:** this is the first central policy increment for the built-in tool dispatcher. It is not yet a durable permission centre, persistent user-grant/revocation store, or a universal authorization wrapper for every Tauri IPC command. Existing file/Coding Assistant operations retain their dedicated preview, grant and approval checks. The next increments must extend the same backend-owned policy to those command families before adding broader capabilities. Shell execution, unrestricted plugins, unattended computer control and paid-provider fallback remain disabled.

The implementation must pass Windows and Linux CI before it is treated as complete. A green CI run verifies the automated checks only; it does not establish physical-device acceptance or a complete OS sandbox.


### File Assistant and Coding Assistant IPC capability enforcement — next increment

Based on #1075 (`bf5469d3b4d0bfcaae6fed78a49b717a4d3a0907`), the next implementation extends the Rust-owned policy to the File Assistant and Coding Assistant command entry points.

- Every registered File Assistant operation for folder selection, file reading, edit/move previews, approval issuance, cancellation, apply, undo history and recovery is assigned a distinct backend capability.
- Every registered Coding Assistant operation for project selection, selected-file reading, change previews, proposal validation, approval issuance, cancellation, apply, undo and history is assigned a distinct backend capability.
- Local project structural analysis receives its own capability.
- Unknown command IDs and capability substitutions are rejected by the central Rust policy.
- Existing folder/project scoped grants, skill-loaded checks, path validation, hash checks, preview expiry, explicit approvals, backups and guarded undo remain in place. The new gateway supplements these controls; it does not replace them.
- Guest execution and sandbox lifecycle commands are deliberately not expanded in this increment. Guest execution remains disabled and must receive a separate end-to-end review before enablement.

**Limitations:** this is a static backend command allowlist, not yet a persistent user-editable permission store with grant expiry/revocation, nor an OS sandbox. The next stage will add durable grants, audit and revocation only after all command families are mapped. Do not infer that a capability name by itself grants a user permission.


### Sandbox lifecycle capability enforcement and guest-execution hold

The #1076 File/Coding Assistant capability increment is verified on both Windows and Linux. The next increment maps the registered sandbox lifecycle IPC entry points through the same Rust-owned backend policy.

- Add distinct backend capabilities for sandbox preflight, snapshot preparation, recovery, configuration preview and snapshot discard.
- Enforce each capability at the start of its Rust command handler, before host inspection, filesystem access or mutation. Existing selected-project grants, current-session snapshot ownership, bounded inventory and handle-relative cleanup remain independently required.
- Give the three registered guest-run approval-receipt endpoints their own narrow backend capabilities: issue, consume and revoke. These endpoints only manage one-use approval receipts; they do not launch a guest process. No guest process-launch command is registered or enabled by this change.
- Add regression tests for all lifecycle mappings, capability substitution and narrow guest-approval receipt capabilities.
- Preserve the existing Windows Sandbox configuration restrictions and resource limits as preparatory code only. This change does not prove a usable OS sandbox, enable guest execution, or establish a universal Tauri IPC gateway.

**Next acceptance gate:** Windows and Linux CI must pass before this increment is marked complete. Physical Windows Sandbox launch/isolation validation remains a separate requirement; guest execution stays disabled until the full end-to-end boundary is independently reviewed.


### Local user-data and skill lifecycle capability enforcement

- [x] Assign distinct backend capabilities to workspace initialization and private-note read/write operations.
- [x] Enforce separate list/read/write/delete capabilities for encrypted conversation history.
- [x] Enforce separate list/add/update/toggle/delete capabilities for encrypted personal memory.
- [x] Enforce separate catalogue/install/enable/load/instructions/unload/uninstall capabilities for the current first-party declarative skill lifecycle.
- [x] Add regression coverage for allowed exact mappings and reject cross-capability substitutions.
- [ ] Verify this increment on Windows and Linux CI.

**Scope and safety note:** these are static backend command gates, not durable user grants or a complete permission centre. Skill install/enable/load remains separate from permission to use tools, access private files, invoke integrations or execute code. No skill receives those rights from this change. The existing tool approval token and file/Coding Assistant scoped-authorization mechanisms remain independently enforced. Guest execution, shell access, generic plugin execution and unattended control remain disabled.


### Backend command coverage — AI providers, research, PDF and tool activity (#1080)

- [x] Add distinct Rust-owned capabilities for installed-model discovery and details, model installation/removal, chat streaming/completion and Coding Assistant model requests.
- [x] Gate PDF selection/extraction and web search/source-reading commands at their Rust handler entry points.
- [x] Gate tool catalogue/history access, history clearing, approval issuance and tool execution; preserve the separate exact-action one-use approval policy for consequential tools.
- [x] Add regression coverage for every new command mapping and cross-capability substitution.
- [x] Verify Linux and Windows CI: initial #1080 exposed a return-type mismatch in `list_tools`; #1081 corrected it, and both Linux and Windows CI passed.
- [ ] Complete a separate persistent user-grant design with explicit consent, narrow scopes, expiry, revocation and audit. These static command capabilities do not represent user consent or provider cost approval.

**Scope and limitations:** model pull/removal can change the user's local Ollama installation and are now classified separately; this increment does not yet add a user-facing confirmation or resource/cost budget for those operations. Web Research explicitly contacts its configured search/source hosts; this gate is not an offline-mode guarantee. PDF extraction remains limited to a file explicitly selected through the native picker. Tool execution still relies on its own trusted tool policy and exact one-use approval for consequential actions. No shell, arbitrary plugin execution, guest process launch or cloud fallback is enabled.


### Streaming response cancellation registration and authorization (#1083)

- [x] Register the existing `cancel_ollama_chat` Rust command in Tauri's IPC handler. The frontend already invokes this endpoint, but it was missing from the registered command list.
- [x] Give cancellation its own `ai.chat.cancel` backend capability rather than implicitly reusing chat-generation authority.
- [x] Apply the same bounded request-ID validation to stream creation and cancellation.
- [x] Add regression tests for accepted/rejected request IDs and cancellation capability substitution.
- [ ] Verify Linux and Windows CI before closing this increment.

**User impact:** FRIDAY's Stop action can now reach the backend cancellation handler instead of failing because the command was not registered. Cancellation remains limited to a matching active request ID; it does not cancel unrelated requests. This does not add persistent user grants or change provider/network behavior.


### Registered Tauri IPC command audit and regression guard (#1084)

- [x] Inventory all 72 commands currently exposed by `tauri::generate_handler!`.
- [x] Map every registered command to its exact Rust-owned command ID and capability.
- [x] Add a test comparing the inventory against the actual Tauri handler list, failing when commands are added, removed or duplicated without classification.
- [x] Add a test that each registered handler contains its own exact `authorize_command` call and that the pair exists in the central policy.
- [x] Remove the stale, unregistered legacy `desktop_action` command; desktop launch remains available only through the centrally authorized tool dispatcher.
- [ ] Verify both Linux and Windows CI.

**Scope:** this inventory verifies static command classification, not durable user consent. A capability policy recognizes a backend operation; it does not grant a skill, integration or provider access to a user resource. Existing scoped grants and one-use operation approvals remain separate. Next: persistent narrow user grants, explicit consent, expiry, revocation and audit history before broader integrations or executable skills.
