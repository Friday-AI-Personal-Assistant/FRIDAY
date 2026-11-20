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
