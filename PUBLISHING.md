# Publishing Guide

Complete publishing workflow for all distribution channels.

## Package Listings

Published packages and where to find them:

| Channel | URL |
|---------|-----|
| **GitHub Releases** | <https://github.com/randlee/agent-team-mail/releases> |
| **Homebrew Tap** | <https://github.com/randlee/homebrew-tap> |
| **crates.io** — `agent-team-mail-core` | <https://crates.io/crates/agent-team-mail-core> |
| **crates.io** — `agent-team-mail` (CLI) | <https://crates.io/crates/agent-team-mail> |
| **crates.io** — `agent-team-mail-daemon` | <https://crates.io/crates/agent-team-mail-daemon> |
| **Release workflow runs** | <https://github.com/randlee/agent-team-mail/actions/workflows/release.yml> |

---

## Distribution Channels

### 1. GitHub Releases (Automated)

**Trigger**: Push a tag matching `v*` (e.g., `v0.8.0`).

**Workflow**: `.github/workflows/release.yml` — runs three jobs in sequence:

1. **`build`** — Compiles release binaries in parallel across 4 platform runners:
   - `x86_64-unknown-linux-gnu` on `ubuntu-latest` → `.tar.gz`
   - `x86_64-apple-darwin` on `macos-latest` → `.tar.gz`
   - `aarch64-apple-darwin` on `macos-latest` → `.tar.gz`
   - `x86_64-pc-windows-msvc` on `windows-latest` → `.zip`
   - Each archive contains both `atm` and `atm-daemon` binaries
   - Archives are uploaded as build artifacts

2. **`release`** — Collects all build artifacts, generates `checksums.txt` (SHA256), and creates a GitHub Release with auto-generated release notes via `softprops/action-gh-release@v2`.

3. **`publish-crates`** — Publishes all 3 crates to crates.io in dependency order (see [crates.io section](#3-cratesio-automated) below). Uses the `crates-io` GitHub environment for deployment protection.

**How to trigger**:
```bash
git tag v0.9.0
git push origin v0.9.0
```

### 2. Homebrew Tap (Manual)

**Repository**: [`randlee/homebrew-tap`](https://github.com/randlee/homebrew-tap)
**Formula**: `Formula/agent-team-mail.rb`

**Update process after a new GitHub Release**:

1. Wait for the GitHub Release workflow to complete
2. Download `checksums.txt` from the release assets
3. Update `Formula/agent-team-mail.rb` in the homebrew-tap repo:
   - Update `version` to match the new release
   - Update SHA256 hashes for each platform from `checksums.txt`
   - Update download URLs to point to the new release tag
4. Commit and push to `randlee/homebrew-tap`

**Verification**:
```bash
brew update
brew upgrade agent-team-mail
# or for fresh install:
brew tap randlee/tap
brew install agent-team-mail
```

### 3. crates.io (Automated)

**Trigger**: Runs automatically as part of the release workflow after the GitHub Release is created.

**Crates published** (in dependency order, with 60s indexing delay between each):
1. `agent-team-mail-core` — core library
2. `agent-team-mail` — CLI binary
3. `agent-team-mail-daemon` — daemon binary

**Setup** (one-time):
1. Create a crates.io account at https://crates.io (login with GitHub)
2. Generate an API token at https://crates.io/settings/tokens with publish scope
3. Add the token as a GitHub repository secret named `CARGO_REGISTRY_TOKEN`:
   - Go to https://github.com/randlee/agent-team-mail/settings/secrets/actions
   - Click "New repository secret"
   - Name: `CARGO_REGISTRY_TOKEN`, Value: your crates.io token
4. Create a GitHub environment named `crates-io`:
   - Go to https://github.com/randlee/agent-team-mail/settings/environments
   - Click "New environment", name it `crates-io`
   - Optionally add protection rules (e.g., required reviewers)

**What happens**:
- The `publish-crates` job in `.github/workflows/release.yml` runs after the GitHub Release is created
- Publishes each crate in dependency order with 60s delays for crates.io indexing
- Uses the `crates-io` environment for deployment protection

**Cargo.toml metadata**: All required fields (`description`, `license`, `repository`, `homepage`, `keywords`, `categories`) are already present in workspace config.

**Note**: The `atm-daemon` crate has an optional `ssh` feature (depends on `ssh2`). This is fine for crates.io — optional dependencies are not required at install time.

**Manual publishing** (fallback if automated publish fails):
```bash
cargo login <your-crates-io-token>
cargo publish -p agent-team-mail-core
# Wait ~60s for crates.io indexing
cargo publish -p agent-team-mail
# Wait ~60s
cargo publish -p agent-team-mail-daemon
```

---

## Release Checklist

### Before Release

- [ ] All tests pass: `cargo test --workspace`
- [ ] Clippy clean: `cargo clippy --workspace -- -D warnings`
- [ ] Version bumped in workspace `Cargo.toml` (`[workspace.package] version`)
- [ ] Internal dependency version updated (`agent-team-mail-core = { version = "=X.Y.Z" }`)
- [ ] CHANGELOG or release notes drafted (optional — GitHub auto-generates from PRs)
- [ ] All changes merged to `main` via PR from `develop`

### Release

1. **Tag the release**:
   ```bash
   git checkout main
   git pull origin main
   git tag v0.9.0
   git push origin v0.9.0
   ```

2. **Monitor GitHub Actions**: Watch the Release workflow at https://github.com/randlee/agent-team-mail/actions

3. **Verify the release**: Check https://github.com/randlee/agent-team-mail/releases for:
   - 4 platform archives
   - `checksums.txt`
   - Auto-generated release notes

### After Release

4. **Verify crates.io publish**: The `publish-crates` job runs automatically after the GitHub Release is created. Check the Actions tab for status. If it fails, use the manual fallback commands in the crates.io section above.

5. **Update Homebrew tap**:
   - Get SHA256s from `checksums.txt`
   - Update `Formula/agent-team-mail.rb` in `randlee/homebrew-tap`

6. **Announce**: Update any relevant documentation or channels

---

## Version Strategy

Version numbers track the project phase: `0.N.0` corresponds to Phase N completion.

| Version | Milestone |
|---------|-----------|
| 0.8.0 | Phase 8 — Cross-computer bridge plugin |
| 0.9.0 | Phase 9 — CI monitor integration (planned) |
| 1.0.0 | Stable release (TBD) |
