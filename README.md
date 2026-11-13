# FRIDAY — free local Windows assistant

Tauri 2 / Rust / React desktop application with **Ollama as the only AI brain**. No paid APIs, subscriptions, credits or cloud AI. This is a working implementation increment, **not the completed Ultimate specification**. Read the [verified feature inventory and limitations](docs/FREE-LOCAL-STATUS.md).

## Included

- Local Ollama chat, installed model selection and capability checks.
- Tool-calling loop with individual action review, cancellation and bounded execution.
- Scoped document/filesystem tools with backups and strict argument validation.
- Windows accessibility inspection and reviewed desktop input tools.
- Local recorded speech recognition using separately installed faster-whisper models, plus offline installed Windows speech voices.
- Encrypted session/preferences/approved memory, preserving the existing encrypted private note.
- Windows worker packaging and NSIS/MSI build workflow.

Windows-specific behavior and live model/audio operation require acceptance testing. See the inventory for precisely what has and has not been tested.

## Run on Windows

Install Node.js LTS, Rust, Python 3.12, Microsoft C++ Build Tools, WebView2 and Ollama using their official installation instructions. Install a local Ollama model with text completion and tool support if using tools. No weights are bundled and no model is downloaded automatically.

```powershell
npm ci
python -m pip install -r worker/requirements.txt
npm run tauri -- dev
```

Open **Agent**, click **Connect Ollama**, select an installed model, then send a message. Enable **Propose local tools** only when needed. Each proposed action shows its arguments and requires approval. The button **Emergency stop** terminates the active worker; it does not undo completed actions.

The worker uses `http://127.0.0.1:11434` only. Keep Ollama bound to loopback. Cloud models and remote-model aliases are rejected. AI requests are not sent to commercial services.

Speech input needs a preinstalled faster-whisper model directory containing its tokenizer/config/weights. Enter its absolute path under **Preferences & memory**. Click **Record 5 seconds**, review the transcript, and send it. Speech inference uses CPU int8 to leave VRAM available to Ollama. Urdu recognition is model dependent; spoken Urdu requires a suitable installed Windows voice and is not guaranteed. This is recorded speech, not hands-free conversation.

File tools operate in `%APPDATA%\com.friday.assistant\documents` (resolved by Tauri for the current user). Copy documents there manually for the agent to inspect. Files/backups in this directory are plaintext. Session and approved memory are encrypted only when **Save encrypted session** is clicked. **Load session** restores them. No automatic background memory collection.

## Tests and installers

```powershell
python -m unittest discover -s tests -v
npm run build
```

The **Free local Windows desktop** GitHub Actions workflow installs worker dependencies, runs Python and renderer tests, packages the worker, generates an icon, runs Rust encryption tests, builds NSIS/MSI installers, smoke-tests NSIS installation/launch/uninstall, and uploads artifacts. Use a standard public-repository runner or your own Windows runner; no paid build or AI service is integrated. Installer artifacts are not evidence of successful installation on a user's machine.

For local installer builds, package `worker/friday_worker.py` as `src-tauri/worker/friday-worker.exe` using the exact PyInstaller command in the workflow, generate `src-tauri/icons/icon.ico` using the workflow icon step, then run `npm run tauri -- build`. Install/uninstall and actual Windows app behavior must be tested before distribution. Builds are unsigned; no paid certificate is required or bundled.

## Privacy

No telemetry or remote access. Credentials remain in Windows Credential Manager. See [Security](SECURITY.md) and the [current implementation inventory](docs/FREE-LOCAL-STATUS.md). Earlier documents discussing optional cloud AI, licences or paid plans are historical and superseded by the free/local requirement.
