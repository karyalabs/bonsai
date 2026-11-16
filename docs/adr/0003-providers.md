# 3. Provider layer: one trait, accounts in the Keychain, Git auth via askpass

Status: Accepted
Date: 2026-11-15

## Context

Users need to connect GitHub, GitLab and Bitbucket, browse their remote
repositories, clone them, and publish local repositories that have no remote.
Any server that speaks Git must still work for clone, fetch and push, with no
API features.

## Decision

- A GPUI-free `bonsai-providers` crate defines a `Provider` trait covering what
  the first milestone needs: sign in, current user, list remote repositories
  (paged, with search), get one, create one (for publish), and clone URLs
  (HTTPS and SSH). Capabilities are explicit so a provider can lack one.
- Implementations: GitHub (OAuth device flow), GitLab (OAuth with PKCE via a
  loopback redirect, or personal access token), Bitbucket Cloud (OAuth, or API
  token). Every account stores its base URL, so GitHub Enterprise Server and
  self-managed GitLab are a host field later, not a redesign.
- HTTP goes through a small `HttpClient` trait; the app supplies GPUI's HTTP
  client so the crate stays testable with recorded responses.
- Tokens are stored in the macOS Keychain, keyed by provider host and account.
  Nothing secret goes in settings files or logs.
- Git over HTTPS authenticates through `GIT_ASKPASS` pointing at Bonsai's own
  binary in askpass mode. When the remote host matches a connected account it
  answers with that token; otherwise it defers, so the user's credential
  helpers and SSH agent keep working. SSH remotes always use the user's SSH
  setup.
- Plain remotes (any URL) need no provider: clone, fetch, pull and push work
  the same.

## Consequences

- Each provider has its own OAuth app registration and client ID to manage.
- PRs, issues, releases and notifications fit the same trait later as new
  capabilities.
- The askpass path must be covered by tests so a bad token never prompts in a
  hidden terminal.

## Implementation notes

- Git starts `GIT_ASKPASS` with the prompt as its only argument, so the
  runner also sets `BONSAI_ASKPASS=1`; Bonsai's `main` checks it before
  starting GPUI. `bonsai --askpass <prompt>` does the same by hand.
- The connected accounts reach the askpass process in
  `BONSAI_ASKPASS_ACCOUNTS` (host, account and Git user name, never a token).
  The token is read from the Keychain (service `dev.bonsai.token`, account
  `host/account`) by the askpass process itself.
- Askpass answers HTTPS prompts, and plain HTTP only on loopback. A prompt
  for another host, another user, or a host with two accounts and no user
  in the URL is declined with exit code 1, so Git reports it could not read
  a credential. Credential helpers run before askpass, as Git orders them.
