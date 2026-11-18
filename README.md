# FRIDAY

A private, modular, user-owned personal AI desktop assistant.

> **Early development:** FRIDAY's first desktop interface is a scaffold. AI, accounts, email integrations, persistent workspace storage, and job features are not active yet.

## Project principles

- Local-first personal workspace.
- No hidden developer access or backdoor.
- Keep account/licence management separate from private user data.
- Optional modules and clear external data flows.
- Human approval before sending emails or submitting applications.

Read [Privacy and Security](docs/PRIVACY-AND-SECURITY.md), [Product Plan](docs/PRODUCT-PLAN.md), and [Security Policy](SECURITY.md).

## Technology

- Tauri 2 desktop shell
- React + TypeScript + Vite frontend
- Rust desktop backend

## Development requirements

- Node.js (current LTS recommended)
- Rust toolchain
- Tauri's Windows prerequisites (including Microsoft C++ Build Tools and WebView2)

## Run locally

```bash
npm install
npm run tauri dev
```

Create a production build:

```bash
npm run tauri build
```

The Windows bundle configuration currently targets NSIS and MSI installers. Build and test on Windows before distributing anything.

## Current status

The current preview includes a navigable desktop interface and an early encrypted-note workspace test. On Windows, the workspace encryption key is stored using Windows Credential Manager and the test note is encrypted before being written locally. This is not yet a complete workspace/database, backup or account recovery system, and it has not received an independent security audit. AI, user accounts, email connections and job features are not active yet.

## Security

Never commit personal CVs, emails, passwords, OAuth tokens, API keys, encryption keys, signing certificates, or real user data. See [SECURITY.md](SECURITY.md) for reporting guidance.
