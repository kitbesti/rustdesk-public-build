# Public GitHub Build Plan

This document is for building the current RustDesk source with free GitHub-hosted runners from a public repository.

## What this flow does

- exports the current working tree without `.build`, `dist`, `target`, and other local caches
- vendors the current `libs/hbb_common` working tree, so local submodule edits are preserved
- keeps only one workflow in the public export: `.github/workflows/public-free-build.yml`
- optionally generates bridge files on the local Linux host first, which saves one Ubuntu GitHub Actions job
- builds only these targets:
  - Windows amd64
  - macOS amd64
  - macOS arm64
  - Android arm64
  - iOS unsigned xcarchive

## Why this is safe for a public repo

- The tracked source in the current repo is small enough for GitHub guidance. On this machine, tracked files are about `17M`, while the full working directory is `24G` because of local build caches and artifacts.
- The export script copies tracked files instead of the whole working tree, so ignored files such as `.env`, `.build`, `dist`, `target`, and private local caches are not pushed.
- The export removes all unrelated workflows to avoid accidental scheduled builds.

## GitHub limits that matter here

- Standard GitHub-hosted runners are free for public repositories.
- GitHub's current GitHub Free plan lists `500 MB` artifact storage and `10 GB` cache storage as included usage, and GitHub notes that Actions artifacts, Actions caches, and GitHub Packages storage share the pooled allowance.
- GitHub recommends repositories stay ideally under `1 GB` and strongly under `5 GB`.
- Git pushes above `2 GB` are blocked.
- Dependency caches are limited to `10 GB` per repository.
- This workflow sets artifact retention to `3` days, and the bridge artifact to `1` day, to keep storage pressure low.

Official references:

- GitHub Actions billing: <https://docs.github.com/en/billing/concepts/product-billing/github-actions>
- GitHub Actions runner pricing: <https://docs.github.com/en/billing/reference/actions-runner-pricing>
- About large files on GitHub: <https://docs.github.com/en/repositories/working-with-files/managing-large-files/about-large-files-on-github>
- Dependency caching reference: <https://docs.github.com/en/actions/using-workflows/caching-dependencies-to-speed-up-workflows>
- Storing workflow data as artifacts: <https://docs.github.com/en/actions/using-workflows/storing-workflow-data-as-artifacts>

## Where to fill your GitHub account and credential

Copy the example env file first:

```bash
cp tools/github/public-build.env.example tools/github/public-build.env
```

Then edit:

```bash
${EDITOR:-vi} tools/github/public-build.env
```

Fill these fields:

- `GITHUB_OWNER`: your GitHub username or org name
- `GITHUB_REPO`: the public repository name to create or reuse
- `GITHUB_TOKEN`: your GitHub token

Important:

- GitHub no longer supports account passwords for Git or REST API operations.
- Do not fill a password. Use a token in `GITHUB_TOKEN`.
- The stable choice is a classic PAT with `repo` and `workflow` scopes, because this flow creates a repository, pushes workflow files, and dispatches the workflow.

Official references:

- Token authentication for Git over HTTPS: <https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens>
- Removing password authentication: <https://github.blog/changelog/2021-08-12-git-password-authentication-is-shutting-down>
- Create a workflow dispatch event: <https://docs.github.com/en/rest/actions/workflows#create-a-workflow-dispatch-event>

## One-command flow

After editing `tools/github/public-build.env`, run:

```bash
bash tools/github/create-and-run-public-build.sh
```

This script will:

1. export the current source into a clean temporary directory
2. optionally generate bridge files locally on Linux
3. create or update the public repository
4. push the exported snapshot to the selected branch
5. dispatch the GitHub Actions workflow

If you only want the iOS intermediate artifact for later signing on a real Mac, run:

```bash
bash tools/github/create-and-run-public-ios-build.sh
```

## Local speed-up step

`USE_LOCAL_BRIDGE=1` is enabled by default in `tools/github/public-build.env.example`.

That means the local Linux host will pre-generate:

- `src/bridge_generated.rs`
- `src/bridge_generated.io.rs`
- `flutter/lib/generated_bridge.dart`
- `flutter/lib/generated_bridge.freezed.dart`
- `flutter/macos/Runner/bridge_generated.h`
- `flutter/ios/Runner/bridge_generated.h`

This saves one Ubuntu workflow job and makes retries cheaper.

If Docker is unavailable locally, set:

```bash
USE_LOCAL_BRIDGE=0
```

Then GitHub Actions will generate the bridge on its own.

## Build outputs

The public workflow uploads these artifacts:

- `rustdesk-windows-amd64`: zipped unsigned Windows app directory
- `rustdesk-macos-amd64`: unsigned DMG
- `rustdesk-macos-arm64`: unsigned DMG
- `rustdesk-android-arm64`: APK built from the release target but switched to debug signing for CI convenience
- `rustdesk-ios-xcarchive`: unsigned `Runner.xcarchive` for later signing on a real Mac

## iOS xcarchive now, signing later on a real Mac

This workflow intentionally builds:

```bash
flutter build ipa --release --no-codesign
```

That command produces an unsigned `Runner.xcarchive` and skips IPA export when signing is disabled.

For a real Mac with a free Apple Personal Team, use:

```bash
bash tools/ios/export-personal-team-ipa.sh \
  --archive /path/to/Runner.xcarchive-or-artifact-zip \
  --team-id YOUR_TEAM_ID \
  --bundle-id com.example.rustdesk.personal \
  --output /path/to/export-dir
```

Prerequisites on the real Mac:

1. Xcode is installed.
2. Your Apple ID is already signed in to Xcode.
3. `Signing & Capabilities` uses automatic signing for your Personal Team.
4. The bundle ID you pass is unique for your account.

What the script does:

1. accepts either the downloaded artifact zip or an extracted `Runner.xcarchive`
2. rewrites the archived bundle identifier to the one you provide
3. asks Xcode to export a development IPA with automatic signing
4. writes the final `.ipa` into the output directory

If Xcode rejects the export because your Personal Team setup is incomplete, open the same app once in Xcode, finish the initial signing prompts, then rerun the script.

Current hard-coded values in this repo that must be replaced on the real Mac:

- `DEVELOPMENT_TEAM = HZF9JMC8YN`
- `PRODUCT_BUNDLE_IDENTIFIER = com.carriez.flutterHbb`

References:

- `flutter/ios/Runner.xcodeproj/project.pbxproj`
- Flutter iOS setup: <https://docs.flutter.dev/platform-integration/ios/setup>
- Apple membership comparison and 7-day Personal Team limits: <https://developer.apple.com/support/compare-memberships/>

## Notes for real-device temporary signing

For a free Apple Personal Team:

- provisioning profiles expire after `7` days
- app IDs and registered test devices also expire after `7` days

So the practical sequence is:

1. build `rustdesk-ios-xcarchive` in GitHub Actions
2. move that artifact to a real Mac later
3. export a development IPA with `tools/ios/export-personal-team-ipa.sh`
4. install the signed build to the device from the real Mac
