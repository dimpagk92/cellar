# Distribution

How the `cellar` CLI is packaged and installed (WS12).

## Homebrew (macOS) — build from source

The formula lives at [`packaging/homebrew/cellar.rb`](../packaging/homebrew/cellar.rb).
It is a **build-from-source** formula: Homebrew downloads the GitHub release
*source* tarball and compiles `cellar-cli` with `cargo install`. GitHub
generates a source tarball for every tag automatically, so this works without
publishing any prebuilt-binary artifact.

```sh
# one-time: point Homebrew at the cellar tap
brew tap cellar/cellar https://github.com/dimpagk92/cellar
brew install cellar

cellar --help
cellar completions zsh > "$(brew --prefix)/share/zsh/site-functions/_cellar"
```

`brew install` pulls a Rust toolchain as a build dependency (`depends_on "rust" => :build`),
builds the `cellar` binary against the committed `Cargo.lock` (`--locked`), and
installs it to the Homebrew prefix. The formula's `test do` block smoke-tests
two no-daemon subcommands (`--help` and `completions zsh`).

## Cutting a release (maintainer)

1. Tag the release on the public repo (`vX.Y.Z`). GitHub publishes the source
   tarball at `…/archive/refs/tags/vX.Y.Z.tar.gz`.
2. Update [`packaging/homebrew/cellar.rb`](../packaging/homebrew/cellar.rb):
   - bump `url` to the new tag,
   - set the real `sha256`:
     ```sh
     curl -sL https://github.com/dimpagk92/cellar/archive/refs/tags/vX.Y.Z.tar.gz | shasum -a 256
     ```
3. Copy the updated formula into the tap repo as `Formula/cellar.rb` and push.
   `brew install cellar` then picks up the new version.

The signed macOS **app** (the Tauri bundle) is a separate artifact built and
notarized by [`.github/workflows/release-signed-macos.yml`](../.github/workflows/release-signed-macos.yml)
and attached to the GitHub release as a `.dmg`; the npm NAPI binaries come from
[`.github/workflows/release.yml`](../.github/workflows/release.yml). Homebrew
here distributes only the **CLI**.

## Future: prebuilt bottles

To avoid a from-source compile on every install, a release job could produce
per-arch tarballs of the `cellar` binary (`aarch64`/`x86_64-apple-darwin`) and
the formula could gain `bottle` blocks or per-arch `url`s with `on_macos` /
`Hardware::CPU.arm?` selection. That is intentionally deferred — the
from-source formula needs no new CI surface and is the lowest-maintenance
starting point.

## Auto-update (Tauri app)

The desktop app ships the [`tauri-plugin-updater`](https://v2.tauri.app/plugin/updater/)
(registered in `app/src-tauri/src/lib.rs`, permitted in
`capabilities/default.json`, configured under `plugins.updater` in
`tauri.conf.json`). The frontend can call `check()` / `downloadAndInstall()` to
self-update.

**Maintainer setup (one-time):**

1. Generate a signing keypair:
   ```sh
   pnpm tauri signer generate -w ~/.tauri/cellar.key
   ```
2. Put the **public** key in `tauri.conf.json` → `plugins.updater.pubkey`
   (replacing the `PLACEHOLDER_…` value), and point `endpoints` at where you'll
   host the update manifest (`latest.json`).
3. At release time, build with the **private** key so the bundler emits signed
   updater artifacts:
   ```sh
   TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/cellar.key)" \
   TAURI_SIGNING_PRIVATE_KEY_PASSWORD=… \
   pnpm tauri build
   ```
   Enable `bundle.createUpdaterArtifacts` (or `"updater"` in `bundle.targets`)
   so the `.sig` + `latest.json` are produced, then publish them to the
   `endpoints` URL (e.g. attach to the GitHub release).

Until a real `pubkey` + feed are wired, the plugin is present but `check()`
returns "no update / signature error" — expected, and safe (no unsigned update
can install).
