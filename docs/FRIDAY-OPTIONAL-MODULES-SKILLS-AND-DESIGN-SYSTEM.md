# FRIDAY Product Experience and Optional Skills Distribution

## Status

Product direction recorded on 2026-11-20. This document clarifies the intended user experience and the boundary between the small desktop application and optional capabilities. It does not claim that a remote skills marketplace or updater is already implemented.

The visual direction in this document is **OFFICIALLY APPROVED**: **FRIDAY — Midnight Azure**. Treat this as the product's canonical visual identity unless the product owner explicitly changes it. Do not silently substitute a generic bright-blue, black-neon, or unrelated theme.

## Product experience

FRIDAY is one desktop assistant with a clear navigation structure and optional capability areas. The user should not need to install a different application for every feature.

The application shell provides:
- AI Chat and the assistant's everyday home.
- Overview / dashboard.
- Modules or Features, showing available capabilities and their status.
- Skills, showing installable capability packages and their permissions.
- Settings, including model/provider, appearance, language, personality and privacy controls.
- Privacy & Security, including permission and activity visibility.

The existing Modules page and Skills Manager are early foundations of this experience. They should evolve into one understandable catalogue, not become two confusing competing stores.

## Download the desktop app first; add capabilities later

The intended distribution model is a small, signed FRIDAY desktop installer followed by optional capability downloads from inside FRIDAY.

A user should be able to:
1. Download and install the core FRIDAY desktop application.
2. Open FRIDAY without being forced to install every optional feature.
3. Browse a catalogue of available modules and skills.
4. Read what each item does, its publisher/source, version, licence, required permissions, data handling, dependencies, size and whether it requires local resources or a paid external provider.
5. Choose Download / Install for only the capabilities they want.
6. Review and explicitly enable a downloaded capability.
7. Load or start it only when appropriate; installation must not silently grant permissions or start background activity.
8. Disable, unload, update or uninstall it, with clear explanation of what local data is retained or removed.

Core safety updates and security fixes are distinct from optional feature packages. FRIDAY must not disguise a feature download as a mandatory update.

## Module versus skill

- **Module / feature:** a user-facing area or workflow such as Email Assistant, CV Studio, Job Hunter, Voice Assistant, Coding Assistant or Web Research.
- **Skill:** a capability package that teaches FRIDAY how to perform a bounded task, potentially used by one or more modules.
- **Integration / connector:** an explicitly configured connection to an external service, with its own account scopes, data-flow disclosure and revocation.
- **Provider / model:** the selected local or remote AI engine. A remote provider may incur charges and send prompts/data outside the device.

The UI should explain these distinctions simply. A downloadable skill must not be able to acquire new host powers merely by being installed.

## Trust and package lifecycle

The future catalogue and package installer must:
- Use a signed or otherwise authenticated catalogue and verify package integrity before installation.
- Pin package identity, publisher, version and content hash; reject downgrade or substitution unless the user explicitly chooses a supported rollback.
- Verify licences and dependency licences before distribution.
- Treat package instructions, metadata, webpages and model output as untrusted input.
- Keep declarative instruction-only skills separate from executable extensions.
- Run executable extensions out of process with least privilege, bounded IPC, resource limits, timeouts, cancellation and OS-enforced filesystem/network isolation.
- Require explicit, understandable permission grants for each capability; deny unknown capabilities by default.
- Separate install, enable, load/run and permission grant/revocation.
- Show data access and external network requirements before installation and again before first use where relevant.
- Provide user-visible version history, update controls, uninstall, revocation and audit events.
- Never silently install a skill, enable a capability, grant access, switch to a paid cloud provider or send private data to a third party.
- Preserve the four guarantees: Safety, User Control, Cost Transparency and Reliability.

## Catalogue and download sources

The initial catalogue can include FRIDAY-maintained packages and selected third-party open-source packages after security, licence, maintenance and compatibility review. Reuse is selective, not wholesale.

The catalogue should support:
- Official FRIDAY packages.
- Reviewed third-party packages with visible publisher and source repository.
- Later, user-added catalogues only behind the same trust and permission checks.

No arbitrary URL or repository should be executable simply because a user added it. Third-party packages must pass the same review and verification gates.

## Offline and cost behaviour

- FRIDAY core and already-installed local capabilities should remain usable offline where their function permits.
- Catalogue browsing and downloads require network access and should say so.
- Local AI is the default where supported.
- Any cloud model, subscription, API key, usage charge or data transfer must be disclosed before activation.
- FRIDAY must not silently fall back from a local model to a paid or remote provider.
- Show package download size and any known runtime/resource requirements.

## Official visual identity: FRIDAY — Midnight Azure

**Decision: LOCKED / PRODUCT-OWNER APPROVED (2026-11-20).**

The supplied reference image is the visual north star: a premium, dark-first desktop interface built from deep navy surfaces, restrained blue accents, subtle layering and crisp readable typography. The desired balance is neither glaringly bright nor lifelessly dull. Blue remains FRIDAY's recognisable identity, but it should be refined rather than saturated or used everywhere.

### Core palette

The following are proposed canonical starting tokens inspired by the supplied reference image. They are approximate design values, not colour values sampled from an original source file. Tune them during implementation for contrast and accessibility, while preserving the approved visual character.

| Token | Suggested value | Intended use |
|---|---|---|
| Midnight Navy | `#050E1D` | Main application background |
| Deep Slate Blue | `#0E1A2D` | Navigation, cards and primary panels |
| Muted Steel Blue | `#192840` | Elevated surfaces and subtle section separation |
| Refined Electric Blue | `#2874D8` | Primary actions, selected navigation, links and key identity accents |
| Soft Ice Grey | `#A9BBD2` | Secondary text and subdued labels |

Text and icon colours must also include accessible high-contrast foreground tokens; the palette above is not a complete contrast specification. Use semantic tokens for success, warning, danger and informational states rather than reusing the brand blue for every status.

### Visual rules

- **Dark-first, not dark-only:** make the dark theme the signature experience, while providing a complete, deliberately designed light theme.
- **Layered navy surfaces:** distinguish the app background, navigation, cards, dialogs and elevated panels with subtle changes in navy/blue-grey. Avoid a flat black canvas.
- **Restrained blue lighting:** use gentle blue gradients or ambient highlights sparingly around selected or focal elements. Do not add glow to every card, border or icon.
- **Blue with purpose:** reserve refined electric blue for primary actions, active states, links and important focus cues. Avoid large areas of intensely saturated blue.
- **Secondary accents:** use small, consistent amounts of violet, teal and amber to distinguish categories, charts or semantic statuses. They support the identity; they do not compete with blue.
- **Typography:** use clear, modern, highly legible typography with deliberate hierarchy, comfortable line spacing and readable muted text.
- **Quiet geometry:** use consistent spacing, restrained borders, moderate corner radii and subtle depth. Avoid excessive gradients, heavy shadows, decorative effects and unnecessary animation.
- **Accessible interaction:** maintain WCAG-appropriate contrast, visible keyboard focus, clear hover/pressed/disabled states, keyboard navigation and reduced-motion support.
- **Honest status communication:** permission prompts, security warnings, costs, external data transfers and approval actions must be visually unmistakable. Never make a dangerous action look like an ordinary decorative blue button.
- **Consistent application-wide language:** apply the same tokens and components to Home/Dashboard, Chat, Coding Assistant, Modules, Skills Store, Memory, Research, File/PDF tools, Settings and future downloadable features.
- **User preference:** support light/dark/system appearance where technically practical; changing appearance must not alter security, permission or network policy.

### Light theme direction

The light theme must be designed as a first-class companion, not produced by simply inverting the dark theme. Use cool whites, pale blue-grey surfaces, restrained navy text and the same refined blue accent. Preserve hierarchy, semantic status colours and accessible contrast in both themes.

### Avoid

- Neon/electric blue used across the whole interface.
- Pure black backgrounds as the default canvas.
- Excessive glow, glass effects, gradients, animations or decorative motion.
- Low-contrast grey text or colour-only status indicators.
- Different visual styles between built-in and downloadable modules.

### Implementation acceptance

- [ ] Define central semantic design tokens for both dark and light themes; avoid scattered hard-coded colour values.
- [ ] Verify text, controls, focus indicators, charts and status colours for accessible contrast in both themes.
- [ ] Build reusable components for navigation, page headers, cards, buttons, inputs, dialogs, badges/status, empty/loading/error states and permission/approval surfaces.
- [ ] Verify consistent appearance across the full application, including optional modules and downloaded skills.
- [ ] Verify appearance preference persistence and system-theme behavior.
- [ ] Keep security, cost and permission communication clear in every theme.
- [ ] Record screenshot-based visual review at desktop and narrow window sizes; do not mark the design implementation complete merely because tokens or mockups exist.

## Design system foundation

The FRIDAY visual theme is a parallel product track, not something postponed until all foundation work is finished. Establish shared design tokens and reusable components before redesigning every screen:
- typography, spacing, surfaces, borders, focus and motion;
- consistent navigation, page headers, cards, status labels and empty/loading/error states;
- accessible contrast, keyboard navigation and clear focus;
- responsive layouts and clear permission/confirmation surfaces.

The official Midnight Azure direction above governs this work. The theme must support light/dark appearance and user preferences without weakening safety cues. Security warnings, costs and approval actions must remain clear and consistent.

## Implementation sequence

1. Stabilise persistence and recovery for local data.
2. Define the capability/permission contract and package manifest format.
3. Refactor the current built-in Skills Manager to distinguish installed, enabled, loaded and granted capabilities without breaking existing packages.
4. Define catalogue metadata, signature verification and package download/update lifecycle.
5. Implement the first downloadable, declarative skill from an approved source.
6. Establish Midnight Azure design tokens and refresh the shell, navigation, Modules/Skills catalogue and Settings in parallel.
7. Add executable extensions only after OS-enforced isolation and permission revocation are proven.

## Current implementation reality

The current app already has a Modules page, a Skills Manager, and a small built-in catalogue whose package files are generated locally from hard-coded definitions. This is not yet a remote downloadable marketplace. The design above is the intended product direction; each implementation stage must be tested and marked complete separately.
