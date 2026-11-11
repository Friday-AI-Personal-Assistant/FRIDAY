# FRIDAY Local Workspace Design

Status: approved design baseline; implementation and security testing are still pending.

## Goals

Each FRIDAY installation owns a separate local workspace. The developer and the FRIDAY account/licensing service must not have a remote path into that workspace.

The workspace may contain conversations, preferences, imported CVs, generated documents, email metadata explicitly saved by the user, and local application state. It must never be uploaded automatically.

## Storage layout

Use the operating system's per-user application-data directory, not the repository folder or a shared system directory.

Proposed layout:

- `workspace/`: encrypted user content and metadata
- `database/`: local SQLite database
- `documents/`: imported and exported user documents
- `logs/`: privacy-minimised diagnostic logs
- `backups/`: user-requested encrypted backups only

The exact Windows path will be selected through the platform app-data API at implementation time. Do not hard-code a username or machine-specific path.

## Encryption and key handling

1. Generate a random per-installation data-encryption key using an operating-system cryptographic random-number generator.
2. Encrypt private workspace content with an authenticated encryption scheme (for example, AES-256-GCM or XChaCha20-Poly1305 from a maintained library).
3. Protect the data-encryption key using Windows DPAPI for the current Windows user, or a supported OS credential/key store on other platforms.
4. Never store plaintext keys, passwords, OAuth refresh tokens, or API keys in source control, logs, crash reports, or ordinary settings files.
5. Do not derive the encryption key directly from the FRIDAY account password.
6. Account recovery must not silently give the developer or server the ability to decrypt a user's workspace. If the user loses the only key and has no recovery copy, data may be unrecoverable. Explain this clearly before enabling encryption.

Use vetted cryptographic libraries and platform APIs; do not implement cryptographic primitives ourselves.

## Data boundaries

### Local by default
- Conversations and assistant preferences
- CVs, cover letters, job-search preferences and generated files
- Local model configuration and local model data references
- User-created notes and workspace metadata

### External only after user action and clear disclosure
- Gmail: use official OAuth; request the minimum scopes; never ask for an email password. Do not send messages without a separate explicit approval.
- Job sites: use permitted public pages, feeds, alerts, and user-provided links. Do not automate LinkedIn login, scrape behind access controls, or mass-apply.
- Cloud AI: send only the information required for the selected request, disclose the provider and possible cost, and provide a local-only option where feasible.

## Account and licensing separation

The future registration/licensing service may receive only the minimum account and entitlement data needed to issue and validate a licence (for example, account identifier, invitation status, licence status, and app version).

It must not receive workspace contents, CVs, email contents, conversations, encryption keys, OAuth tokens, or local database files.

The application must continue to provide access to local files when the licensing service is unavailable, subject to a separately documented licence policy. Do not add remote wipe, remote shell, hidden administrator access, or remote workspace browsing.

## Application security requirements

- Apply least-privilege Tauri capabilities. Frontend code must not receive unrestricted filesystem or shell access.
- Expose narrow Rust commands for specific workspace operations; validate all paths and reject traversal.
- Keep secrets out of frontend JavaScript and renderer storage.
- Validate imported file types and size limits; treat document contents as untrusted input.
- Do not execute macros, scripts, or embedded active content from imported documents.
- Use parameterised SQL queries.
- Keep logs free of email bodies, CV text, tokens, prompts, and document contents.
- Provide user-initiated export and deletion. Explain that deletion of SSD data cannot always guarantee forensic erasure.
- Sign official releases and protect the signing key outside the public repository and CI logs.
- Updates must be authenticated; never install arbitrary unsigned update payloads.

## Threat model and honest limitations

This design reduces developer access and accidental disclosure. It does not make a device invulnerable.

It cannot protect workspace data from malware, a compromised operating-system account, an attacker controlling the device while the workspace is unlocked, or a compromised dependency/update channel. Windows DPAPI protects keys in the context of the Windows user account; it is not a defence against malware running as that user.

Email providers, job sites, and optional cloud AI providers still process the information needed to provide their services under their own policies.

## Implementation sequence

1. Create the Tauri application shell and identify the supported Windows app-data location.
2. Implement key generation and OS-backed key protection with tests.
3. Implement authenticated encryption and encrypted SQLite/document storage.
4. Add explicit lock/unlock and recovery/export flows.
5. Add privacy-minimised logs and deletion.
6. Perform dependency, permission, and security review before distributing installers.

No feature should be described as encrypted, private, or secure until the corresponding implementation and tests exist.


## Current preview implementation (November 2026)

The first Windows implementation stores one random 256-bit workspace key in Windows Credential Manager and uses XChaCha20-Poly1305 to encrypt a single test note before writing it to the user's application-data directory. The key is generated with the operating system random source. The Rust backend exposes only workspace initialization and fixed-note save/read commands; it does not expose arbitrary filesystem access.

Unit tests cover encryption/decryption round-trip and rejection of tampered ciphertext. These tests are included in the Windows build workflow.

Not implemented yet: encrypted SQLite, multiple document records, workspace lock timeout, key rotation, encrypted backup/recovery, uninstall cleanup, independent security review, and non-Windows credential-store support. The current note preview must not be treated as a complete production-ready workspace.
