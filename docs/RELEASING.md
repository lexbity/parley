# Build and qualify a Parley software version

The checked-in release pipeline produces an **internal Linux x86_64 launcher archive**, using Ubuntu 24.04 for qualification. The launcher embeds Vue, starts the same local Rust service used in development, and opens the system browser. A public desktop release requires an installed Tauri application and the platform qualification below.

## Build locally

From the repository root, activate the pinned tools and build into a new directory. [Getting started](GETTING_STARTED.md#toolchain-versions)

```sh
scripts/dev-install full
scripts/release --output artifacts/releases/local-launcher
```

The build runs formatting, Clippy, Rust and frontend tests, and the contract and migration verifiers. It builds Vue before compiling the launcher and requires all embedded assets. It then launches the executable extracted from the archive, checks the embedded HTML, JavaScript, and styles against their digests, and saves a Resource using Chromium. All verification Workspaces are disposable.

An untagged build records the source commit and whether the tree was dirty. The default output directory is `artifacts/releases/parley-<version>-linux-x86_64-local-<commit>`. Pass `--output` to select a new directory. A successful run produces the archive, `release-manifest.json`, `qualification.json`, `RELEASE_NOTES.md`, and `SHA256SUMS`. The archive includes the executable, manifest, license, and installation instructions. The script writes the output directory after qualification passes.

Check the artifact set before handing it to anyone.

```sh
(cd artifacts/releases/local-launcher && sha256sum --check SHA256SUMS)
```

## Stage a tagged internal build

1. Set `workspace.package.version` in `Cargo.toml` and commit the corresponding `Cargo.lock` update. Use an annotated `v<version>` tag on a clean, reviewed commit.
2. To rehearse locally, run `scripts/release --tag v<version>`. If the tag has a previous version in its ancestry, include the previous same-platform archive and its trusted checksum file:

   ```sh
   scripts/release --tag v<version> \
     --previous /path/to/parley-<previous-version>-linux-x86_64.tar.gz \
     --previous-checksums /path/to/SHA256SUMS
   ```

3. Pushing the tag runs [the internal launcher workflow](../.github/workflows/release.yml) against that commit. It obtains the previous tagged archive when one exists, runs the same local release script, and stages a draft GitHub prerelease. Review the manifest, qualification report, and notes in the draft. Keep launcher archives in the internal distribution channel.
4. Rerunning the draft job accepts matching assets and uploads missing files. A digest mismatch or published release stops the job. Resolve a mismatched candidate before sharing its download.

The upgrade drill starts the previous executable, writes synthetic records, creates a verified backup, and closes the Workspace. It launches the candidate on a stopped copy, checks the records, writes a new record, and restarts the candidate. It then checks that the previous executable reopens a separate pre-upgrade copy. The first tagged build runs the same-version restart and recovery drill. The qualification report records whether a predecessor archive was exercised.

## Upgrade and recover a Workspace

Before a real upgrade, create a verified backup in **Settings**, stop Parley, and keep a copy of the whole Workspace and the previous software version. Extract the candidate into its own directory and try it against a stopped copy. Use the previous executable with a pre-upgrade Workspace when recovering. Preserve the upgraded Workspace for export and review if it contains later changes. [Workspace backup and restore](OPERATIONS.md)

## Qualify a public Tauri release

Build and sign platform installers after the Tauri shell can start or reuse the existing local service. Qualify Linux x86_64 on Ubuntu 24.04, Windows x64 on Windows 11, and macOS arm64 on macOS 15 before advertising those targets. Windows needs a working OS-level Workspace lock and launcher handoff. Exercise installation, the single writer across browser and desktop, session bootstrap, native file and clipboard handling, shutdown and restart, accessibility, and migration and recovery with the previous installer. Record the supported OS and WebView versions alongside the artifact digests.

Keep updates explicit and offline through a signed installer. Verify the package and a pre-upgrade Workspace backup before replacement. macOS direct distribution requires signing and notarization. The internal launcher workflow stages only the browser-based archive. [Tauri distribution and signing](https://v2.tauri.app/distribute/)
