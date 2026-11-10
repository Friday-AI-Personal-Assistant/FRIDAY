# FRIDAY Privacy and Security Principles

FRIDAY is designed as a user-owned desktop assistant. A user's private workspace must not become accessible to the developer merely because the developer distributes or maintains the application.

## Core principles

1. **Local-first:** Personal files, conversations, preferences, and application data are stored on the user's device by default.
2. **No developer access:** There must be no hidden administrator account, remote desktop access, developer backdoor, or routine developer access to a user's workspace.
3. **Separate identity from private data:** If online accounts and licences are introduced, the account service must not receive private workspace contents or encryption keys.
4. **User-controlled secrets:** Never ask users to put email passwords, OAuth secrets, API keys, or encryption keys in this repository. Use operating-system credential storage where appropriate.
5. **Least privilege:** Request only the permissions required for a feature, explain them, and allow optional modules to be disabled.
6. **Explicit approval:** External actions such as sending email or submitting an application must require a clear user confirmation.
7. **Transparent data flows:** Clearly disclose when a feature contacts Google, job websites, or an optional cloud AI provider.
8. **No silent telemetry:** Do not add analytics, tracking, or diagnostic uploads without a clear explanation and meaningful user choice.
9. **User control:** Provide practical ways to export and delete local data.
10. **Secure updates:** Release processes must protect signing credentials and prevent untrusted update packages from being presented as official.

## Important privacy boundaries

- Local-first does not mean every operation is offline. Email providers, job websites, and optional cloud AI services receive the information necessary to perform the requested operation.
- Local encryption does not protect data while the device is unlocked or from malware or an attacker who controls the operating-system account.
- The developer cannot promise absolute security. Security claims must be limited to protections that have actually been implemented and tested.
- Account recovery must not silently give the developer the ability to decrypt a user's private workspace. Recovery design must be documented before implementation.

## Before release

Every feature must document:
- What data it reads and writes.
- Whether data leaves the device and which service receives it.
- Which permissions and credentials it needs.
- How the user can disable it.
- How the user can delete or export its data.
- Known limitations and security risks.

These principles are design requirements, not a claim that all protections are already implemented.
