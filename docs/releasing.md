# Release OMCA

OMCA ships macOS Apple silicon and Windows x64 packages from `seuput-inc/omca`.
The workspace version in `Cargo.toml` is the source of truth. Workspace entries in
`Cargo.lock` must use the same version.

## Prepare a candidate

1. Independently verify each product PR before merging it. Record the parent and
   head commits, observed behavior, and limitations on the PR. Source review and
   green CI alone do not establish runtime behavior.
2. Merge the verified changes into `main`.
3. Bump the workspace version and its lockfile entries. Commit the candidate and
   push it with the mandatory `cargo xtask ci` pre-push hook enabled.
4. Freeze the candidate commit. Build and verify both platforms from that commit.
   If product code changes, rebuild the affected packages and repeat their checks.

Use a private release-signing key through `OMCA_UPDATE_SIGNING_KEY_FILE` locally
or the `OMCA_UPDATE_SIGNING_KEY` Actions secret. Do not print or commit the key.
Packaging rejects an absent key or one that differs from OMCA's pinned Ed25519 key.

## Build and verify macOS

1. Set `DIST`, `CARGO_TARGET_DIR`, `TMPDIR`, and `RUNNER_TEMP` to release work
   directories outside the checkout. Set `CRAFT_FONTS_DIR` and
   `CRAFT_FONTS_REQUIRED=1` to the pinned font bundle from `packaging/env.sh`.
2. Run `packaging/macos/package.sh`, then `packaging/macos/verify.sh`.
3. Verify the update ZIP's raw manifest signature, identity, size, SHA-256, and
   embedded executable hash. Use the pinned public key, not a key from the package.
4. Record the packaged CLI version, provider-runtime isolated smoke result, code
   signature integrity, and updater-helper result. Use only synthetic credentials.

The package script checks the mounted DMG's app and bundled CLI, provider runtime,
and licence files. The CLI ZIP is checked separately. macOS Developer ID signing
and notarization require their own configured credentials. Ad-hoc packages are
not notarized; record that limit instead of claiming Gatekeeper approval.

## Build and verify Windows

1. Dispatch the manual workflow for the immutable candidate with publication off:

   ```sh
   gh workflow run release-windows.yml --repo seuput-inc/omca \
     --ref main -f tag=<candidate-commit> -f publish=false
   ```

2. Wait for the run to finish. Check the actual checkout in its log, not only the
   workflow's `headSha`. The checkout step uses the `tag` input.
3. Require every native gate to pass. These cover the owner lifecycle, signed
   portable helper replacement and same-install restart, synthetic credential
   preservation, private Unity profile, game engines, image permissions and
   budgets, security, builder consent, provider runtime, installed MSI and portable
   payloads, updater UI, titlebar, engine authorization, unified UI, exit recovery,
   and headless parity.
4. Download the six files in `omca-windows-x64`. Independently verify both raw
   Ed25519 manifest signatures, app/repository/version/platform/package identity,
   payload sizes and SHA-256, and the portable executable's signed hash.
5. Record the actual packaged CLI version from both installation forms. Record
   Authenticode status and any untested physical desktop interactions separately.

Builder consent uses a scripted model with the real Worker, conversation, host,
and engine. Its regression reaches tool validation without running a foreign
program. It does not prove OS permissions, original-app capture, paid provider
operations, or legal consent. Explicit project rules and those boundaries remain
separate from automatic builder tool approval.

## Publish verified packages

Do not publish a partial platform release. `packaging/release.sh` publishes macOS
before Windows finishes, so it is not the two-platform verification gate.

1. Confirm both platform verdicts cover the frozen candidate and `main`.
2. Generate `SHA256SUMS.txt` for the eleven package and update-metadata files.
3. Tag the verified commit as `v<version>` and push the tag with hooks enabled.
4. Create a draft GitHub release with all eleven files and `SHA256SUMS.txt`.
   Read the draft by its numeric release ID and compare every uploaded asset digest.
   The tag endpoint may not resolve an unpublished draft. Publish it as latest
   only after those checks. A draft is not a completed deployment.
5. Re-read the published release. Check the tag, latest status, all twelve asset
   names and digests, and public update metadata and payload downloads.
6. Record the release URL, source commit, Windows run, verification results, and
   limitations in `docs/STATUS.md`. Close only issues whose fix is in the published tag.

The release files are a macOS DMG, CLI ZIP, update ZIP and its manifest/signature;
a Windows MSI and portable ZIP, each with its manifest/signature; and the checksum
file. Historical `craft-anything-` filenames remain part of the installer/update
contract. The installed app is OMCA.
