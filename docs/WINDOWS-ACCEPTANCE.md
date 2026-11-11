# Windows acceptance still required

Run these on an interactive Windows 11 account, ideally Ryzen 5 5600 / RTX 4060 8 GB / 16 GB RAM. A CI process-launch smoke test is not equivalent to these tests.

1. Install the NSIS or MSI artifact from the PR's successful workflow. Record artifact name, commit and installer SHA256 (`Get-FileHash <installer> -Algorithm SHA256`). Do not treat unsigned builds as publisher-verified releases.
2. Launch FRIDAY, open Agent, and connect to a loopback-only Ollama installation. Ensure the model list contains only installed local models. Test a normal conversation with tools disabled.
3. Disconnect internet after models are installed. Verify chat, file actions and installed speech components still work. Stop Ollama and verify the UI reports a failure rather than pretending completion.
4. Choose a model advertising tool support. Enable tools and request a workspace file listing. Decline its first proposal; verify no tool runs. Repeat and approve exactly one action.
5. Ask for `note.txt` with known text. Approve writing and reading. Repeat with another value and check the previous file is in `.backups`. Check attempts to read `../` and outside the workspace fail.
6. Save encrypted session, restart, and load it. Verify conversation and edited memory return. Verify the earlier private-note test still works independently. Encrypted data should not contain plaintext text when searched, and the key should be in the current user's Windows Credential Manager.
7. Generate a DOCX and an English PDF, open them manually, inspect layout and text, and test importing/reading files copied into the workspace. Urdu PDF output should report its current limitation and suggest DOCX rather than generate broken glyphs.
8. Ask to launch Notepad. Approve. Inspect Windows accessibility, focus Notepad by exact title, choose an appropriate control, and test a short harmless value update. UIA value input may be unsupported in some applications; verify a real error is returned when unsupported.
9. Test mouse/keyboard and scrolling only in a disposable local application. Confirm the approval card contains the exact input. Test the emergency stop while waiting for a model/tool and verify no further workflow steps execute. Previously completed actions are not undone.
10. Capture screens with no sensitive information visible. Verify the preview includes intended monitors. Analyze only with a selected local vision model, then discard capture and verify future messages do not include it. A text-only model must refuse image analysis.
11. Configure a preinstalled local faster-whisper directory. Test five-second English and Urdu recordings. Confirm transcripts can be reviewed before sending, a missing model gives an error, and cancel ends the recording worker. Test installed SAPI voices, rate, volume and cancellation. Urdu speech quality/availability is not guaranteed.
12. Repeat accessibility, screenshots and input at 100%, 125%, 150% display scaling and on multiple monitors. Coordinate clicks use actual screen coordinates, not resized preview coordinates. There is no automatic visual grounding.
13. Uninstall and reinstall without selecting any option to remove personal files. Record whether workspace data is retained. MSI install/uninstall needs separate acceptance from the NSIS CI smoke test.

Do not test consequential actions such as sending mail or submitting forms as though they were integrated modules. Browser, OAuth, wake-word, continuous voice, scheduler, terminal and full workflow recovery remain unimplemented. See FREE-LOCAL-STATUS.md.
