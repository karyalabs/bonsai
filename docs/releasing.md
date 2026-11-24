# Releasing Bonsai

A release is a Developer ID signed, notarized and stapled `Bonsai.dmg` on a
GitHub release. The **Release** workflow
(`.github/workflows/release.yml`) builds it on Karya's self-hosted Mac
and publishes it as a GitHub release.
`script/package_release.sh` does the packaging, and
`script/create_github_draft_release.sh` creates the release, attaches the
files and then publishes it. Both were
ported from the SwiftUI app's `stable` branch.

## Version

The release version is the workspace version in `Cargo.toml`
(`[workspace.package] version`). The build number defaults to the workflow's
run number. Both can be overridden when the workflow is dispatched.

## Secrets

The protected `release` environment holds these secrets. Keep them out of
repository-level secrets.

| Secret | Used for |
|---|---|
| `BONSAI_CODESIGN_IDENTITY` | The `Developer ID Application: …` identity name |
| `BONSAI_DEVELOPER_ID_CERTIFICATE_BASE64`, `BONSAI_DEVELOPER_ID_CERTIFICATE_PASSWORD` | The certificate as a base64 .p12, imported into a temporary keychain |
| `BONSAI_NOTARY_APPLE_ID`, `BONSAI_NOTARY_APP_PASSWORD`, `BONSAI_NOTARY_TEAM_ID` | `notarytool` credentials |

`make release-github-doctor` checks the environment, its reviewers and the
secrets' names, without showing their values.

## Release notes

Every release's notes come from its section of `CHANGELOG.md`, a
`## <version> (date)` heading with what's new and fixed, grouped by feature,
written for people using Bonsai and naming the pull requests.
`script/release_notes.sh <version>` prints them as they will appear.

CI fails while the version in `Cargo.toml` has no section, so a version bump
can't merge without notes. Editing a released version's section on `main`
updates its release page too: the **Release notes** workflow
(`.github/workflows/release-notes.yml`) copies each section to its release.

## Steps

Bumping the version is the release, as in gpuicn and Matty:

1. Bump `version` in `Cargo.toml` and add its section to `CHANGELOG.md` in a
   pull request, and merge it.
2. When CI passes on that commit on `main`, **Release** runs by itself:
   - karya-karta, the Karya bot app, tags the commit `v<version>`;
   - the self-hosted Mac imports the certificate into a temporary keychain,
     builds, signs and notarizes the app and then the DMG, staples both and
     checks them with `spctl`;
   - it publishes the GitHub release with `Bonsai.dmg`, its manifest and the
     notes from `CHANGELOG.md`.
     Versions before 1.0 are marked as prereleases.

A commit whose version is already released does nothing. If a run fails,
fix the cause and use **Re-run failed jobs**: a tag that already exists is
kept, and a draft left by the failed run is replaced.

To check packaging without releasing, run **Release** by hand with
`dry_run` checked (`make release-dry-run`). It builds an ad-hoc signed DMG
and uploads it as a workflow artifact, without any secrets.

## Settings this needs (Arya, once)

- The `release` environment has no required reviewers, so a release needs
  no approval. Limit its deployment branches to `main` instead.
- The organization secrets `KARYA_BOT_APP_ID` and `KARYA_BOT_PRIVATE_KEY`
  are available to bonsai (Organization settings → Secrets and variables →
  Actions, "Selected repositories").
- If a `v*` tag ruleset protects bonsai's tags, karya-karta is on its bypass
  list.

The temporary keychain is deleted at the end, whether the run passed or not.

## Locally

On a Mac with Xcode:

```sh
make release-verify            # ad-hoc signed Bonsai.app in dist/release
make release-verify-archive    # and an ad-hoc DMG with its manifest
make release-verify-artifacts  # checks the DMG against the manifest
```

The app is built with Metal shaders compiled ahead of time when Xcode's
Metal toolchain is installed (`xcrun --find metal`). Without it, the build
falls back to compiling them at launch (`bonsai/runtime_shaders`). Set
`BONSAI_CARGO_FEATURES` to choose the features yourself.
