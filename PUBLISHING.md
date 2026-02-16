# Publishing Guide

Complete publishing workflow for all distribution channels.

## Distribution Channels

### 1. GitHub Releases (Automated)

**Trigger**: Push a tag matching `v*` (e.g., `v0.8.0`).

**What happens**:
- `.github/workflows/release.yml` builds release binaries for 4 targets:
  - `x86_64-unknown-linux-gnu` (tar.gz)
  - `x86_64-apple-darwin` (tar.gz)
  - `aarch64-apple-darwin` (tar.gz)
  - `x86_64-pc-windows-msvc` (zip)
- Archives contain both `atm` and `atm-daemon` binaries
- SHA256 checksums are generated automatically (`checksums.txt`)
- A GitHub Release is created with auto-generated release notes

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

### 3. crates.io (Not Yet Published)

**Status**: Not published. All three crate names are available on crates.io.

**Crates to publish** (in dependency order):
1. `agent-team-mail-core` — core library
2. `agent-team-mail` — CLI binary
3. `agent-team-mail-daemon` — daemon binary

**Prerequisites**:
- Create a crates.io account and generate an API token at https://crates.io/settings/tokens
- Login: `cargo login <token>`
- Ensure all `Cargo.toml` files have required metadata (already present):
  - `description`, `license`, `repository`, `homepage`, `keywords`, `categories`
- The workspace `Cargo.toml` uses path dependencies with version pinning (`version = "=0.8.0"`), which is correct for publishing

**Publishing commands** (must be run in order):
```bash
cargo publish -p agent-team-mail-core
# Wait for crates.io to index (~1-2 minutes)
cargo publish -p agent-team-mail
cargo publish -p agent-team-mail-daemon
```

**Note**: The `atm-daemon` crate has an optional `ssh` feature (depends on `ssh2`). This is fine for crates.io — optional dependencies are not required at install time.

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

4. **Update Homebrew tap**:
   - Get SHA256s from `checksums.txt`
   - Update `Formula/agent-team-mail.rb` in `randlee/homebrew-tap`

5. **Publish to crates.io** (when ready):
   ```bash
   cargo publish -p agent-team-mail-core
   # wait ~2 minutes
   cargo publish -p agent-team-mail
   cargo publish -p agent-team-mail-daemon
   ```

6. **Announce**: Update any relevant documentation or channels

---

## Version Strategy

Version numbers track the project phase: `0.N.0` corresponds to Phase N completion.

| Version | Milestone |
|---------|-----------|
| 0.8.0 | Phase 8 — Cross-computer bridge plugin |
| 0.9.0 | Phase 9 — CI monitor integration (planned) |
| 1.0.0 | Stable release (TBD) |
