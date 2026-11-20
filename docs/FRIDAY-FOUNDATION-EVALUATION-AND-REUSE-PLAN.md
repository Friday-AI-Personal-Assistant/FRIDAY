# FRIDAY Foundation Evaluation & Reuse-First Engineering Plan

**Status:** Initial source inspection; foundation selection is NOT final  
**Evaluation branch:** `foundation/evaluation`  
**Preserved baseline:** `feature/phase-2-skills` at `85dbace48595c1600bb152cb511f65572d53706d` (#1056)  
**Product acceptance order:** Safety → User control → Cost transparency → Time saved

## Executive decision

Do not replace FRIDAY with another repository and do not import an entire assistant wholesale.

The current leading candidate for deeper evaluation is **OpenJarvis**. Its modular Python/Rust backend, local-first inference direction, Tauri desktop, tools, skills, memory, security modules and tests offer substantial reusable engineering. However, its package is explicitly classified as **Alpha**, and source inspection found defaults and execution paths that do not meet FRIDAY's required security posture without adaptation.

**Current recommendation:** evaluate OpenJarvis as a source of reusable components and architectural patterns. Do not yet select it as FRIDAY's application foundation, enable its agents/tools, or ship its desktop app.

The Thoth project is a secondary reference for user-facing breadth (Windows/macOS desktop, local models, graph memory, workflows, voice, documents and integrations), not a selected dependency. The inspected `zeroitp/Thoth` repository identifies itself as a fork and its README points to a different canonical repository. Its dependency set is broad and mostly unpinned in `requirements.txt`; a proper canonical-source and dependency/license/security review is still required before any reuse.

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
