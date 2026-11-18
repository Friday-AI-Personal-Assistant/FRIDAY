# FRIDAY Product Plan

## Product vision

FRIDAY is a modular personal AI desktop assistant for Windows. It should be useful to one person on one computer without requiring a central FRIDAY server to handle their private information.

The application is intended for free distribution to friends and family initially, with a possible future commercial offering.

## Product principles

- Each person has an independent installation and private workspace.
- Features are included in the application and can be enabled or disabled in settings.
- Local AI is the default where the computer can support it.
- Optional cloud AI may be offered for less powerful computers, with clear disclosure that data is sent to the selected provider and charges may apply.
- No email should be sent and no job application should be submitted without the user's explicit approval.
- Never fabricate qualifications, employment history, skills, achievements, or other CV information.

## Planned modules

### 1. Email Assistant
- Connect an individual's own email account using official OAuth.
- Start with the minimum required permissions.
- Summarise and categorise messages.
- Prepare reply drafts.
- Require user review and approval before sending.

### 2. Job Hunter
- Find jobs through permitted sources, official alerts, and user-provided links.
- Compare job requirements with the user's CV and preferences.
- Explain why a role may or may not be a match.
- Help prepare CVs, cover letters, and application answers.
- Keep final review and submission with the user.
- Do not implement LinkedIn scraping, automated login, or mass-application behaviour.

### 3. CV Studio
- Import PDF and DOCX CVs.
- Create and edit CVs.
- Offer ATS-friendly, modern, classic, and other templates.
- Preview and export documents.
- Never invent facts about the user.

### 4. Voice Assistant
- Optional voice input and spoken responses.
- Make microphone access optional and visible.

### 5. Personality and Preferences
- Professional, friendly, efficient, and custom styles.
- User-selected language and response preferences.

## Accounts and distribution: future design

FRIDAY's source repository will remain public so people can inspect the code. Official installer distribution and registration will be controlled separately through an invitation process. A public source repository means anyone may still clone the source and build their own copy; invitation controls apply to the official distribution and account service, not to the source itself.

The possible account system is for identity, invitations, and licence management only. It must be architecturally separate from private local workspace data.

- Keep the GitHub source repository public.
- Use a separate invitation-controlled channel for official installer downloads; do not publish official installer assets in a publicly accessible release location if access must be restricted.
- Each person registers their own account if online accounts are introduced.
- Licence status must not grant the developer access to private files.
- No private workspace content or encryption keys should be uploaded to an account server.
- Free and paid plans may be introduced later.
- Do not implement account registration or payment until the privacy model and threat model have been reviewed.

## Development phases

1. **Foundation:** project structure, architecture, security requirements, and a minimal desktop shell.
2. **Local workspace:** settings, local database, data export and deletion, and secure handling of secrets.
3. **Local AI:** optional local model integration and clear hardware guidance.
4. **CV Studio:** local document import, editing, preview, and export.
5. **Email Assistant:** OAuth integration, read-only and draft workflows, then carefully gated sending.
6. **Job Hunter:** permitted job sources, matching, and application preparation.
7. **Voice and personalisation:** optional speech features and custom preferences.
8. **Distribution:** repeatable Windows builds, update security, installation testing, and release documentation.
9. **Optional accounts and licences:** only after a separate privacy and security review.

## Non-goals for the initial version

- No central server storing users' emails, CVs, documents, or chat history.
- No hidden remote access or developer backdoor.
- No automatic email sending or job applications.
- No promise that cloud AI is free.
- No claim of absolute or unbreakable security.
