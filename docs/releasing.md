# Releasing Bonsai

A release is a Developer ID signed, notarized and stapled `Bonsai.dmg` on a
draft GitHub release. The **Release** workflow
(`.github/workflows/release.yml`) builds it on Karya's self-hosted Mac.
`script/package_release.sh` does the packaging, and
`script/create_github_draft_release.sh` creates the draft release. Both were
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

## Steps

1. Bump `version` in `Cargo.toml`, merge, and wait for CI on `main`.
2. Run a dry run: `make release-dry-run`, or dispatch **Release** with
   `dry_run` checked. It builds an ad-hoc signed DMG and uploads it as a
   workflow artifact, without using any secrets.
3. Run the release: `make release`, or dispatch **Release** with `dry_run`
   cleared. After the environment's reviewers approve, it:
   - imports the certificate into a temporary keychain;
   - builds, signs and notarizes the app, then staples it;
   - builds, signs and notarizes the DMG, then staples it;
   - checks both with `spctl`;
   - uploads the DMG and its manifest, and creates a draft GitHub release for
     the tag `v<version>`.
4. Read the draft release, then publish it by hand.

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
