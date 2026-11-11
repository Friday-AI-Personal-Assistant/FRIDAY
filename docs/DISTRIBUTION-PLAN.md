# FRIDAY Distribution Plan

## Decision

- Keep the FRIDAY source repository public.
- Control access to official installer downloads through invitations.
- Make account registration invitation-controlled if/when accounts are implemented.
- Keep distribution and account services separate from users' private local workspaces.

## What this means

A public source repository allows anyone to inspect, clone, and build the source code. An invitation system cannot prevent someone from compiling their own copy. It controls only the official installers and the official account/licence service.

Official installer files must not be placed in a publicly accessible download location if access restriction is required. The future download channel should verify an invitation or an approved account before providing the official installer.

## Account service boundaries

If accounts are added later, the service should handle only the minimum identity, invitation, and licence information required.

It must not receive:
- Email contents or attachments
- CVs or personal documents
- Chat histories or private workspace databases
- Local encryption keys or recovery secrets
- OAuth access tokens for connected services

A valid account or paid licence must never create developer access to a user's private workspace.

## Current implementation status

This document records the chosen direction. Invitation verification, account registration, licence checks, and protected installer hosting are not implemented yet.

The current GitHub Actions workflow is for development builds and test artifacts. It is not the final invitation-controlled distribution system.

## Before public release

1. Decide where official installers will be hosted behind access control.
2. Design invitation issuance, expiry, revocation, and abuse prevention.
3. Define minimal account data and retention periods.
4. Review update signing and installer integrity.
5. Test installation, update, uninstall, and data preservation.
6. Complete a security and privacy review.
