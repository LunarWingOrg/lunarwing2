# PRE-RELEASE CHECKLIST for LunarWing v2.0.1.0 Codename `Togishi`

## Release Codename:  研師
## English Context:   Togishi

**Open TODOs (v2.0.1.0) — To be done before release**

---

1. [ ] Check items in priv notes repo
2. [ ] Test changes related to: added automatic relay bootstrap for weechat, Added dedicated tenant-owned, mode-0600 weechat.env containing only RELAY_PASSWORD, Updated systemd and OpenRC services to load the minimal credential file instead of full 
lunarwing.env, Added configure-weechat-relay <tenant> recovery command, kawarimi behavior related to these changes should be unaffected
3. [ ] kawarimi security enhancement requires further testing (see section at bottom)
4. [x] CREATE DETAILED PLAN OF PHASE 5 OF BIG STREAM Phase 4 is interruption/cancellation; Phase 5 is opt-in WASM delivery.
5. [ ] Ensure phase 5 compatibility with all tools, mcp, skills, channels. ensure streaming. ensure gateway ui functionality. ensure tool compatibility in tensorzero. multiple phases. record progress. lot of tests needed
6. [ ] verify self improve skill
7. [ ] more doc updates
8. [ ] bump crate versions
9. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.1.0 explaining all relevant changes since v2.0.0.0 as well as revising and including an ACCURATE VERSION OF `known issues list`. Use previous release notes in docs/release for reference as to how to write up this document. The codename for this release is: `Unknown` — The file you write will be RELEASE-v2.0.0.0.md and should be written to the ROOT of the repo.
10. [ ] Improve accuracy of RELEASE-v2.0.1.0.md

___

# Kawarimi Changes in 2.0.1.0


## Summary

The kawarimi (tenant migration) bundle contained SECRETS_MASTER_KEY, full PostgreSQL dump, XMPP credentials, and OMEMO store as plaintext in a 0600 tar file. File permissions only protect against other users on the same host — nothing during transit or at rest.

## Changes

**export-tenant.sh:**
- Replace `tar cf` with `7z a -t7z -mhe=on` (AES-256 + header encryption)
- Add `--no-encrypt` flag for testing/debugging
- Passphrase via `KAWARIMI_PASS` env, `KAWARIMI_PASS_FILE`, or interactive prompt with confirmation
- Passphrase cleared from memory after use
- 7z dependency check added

**import-tenant.sh:**
- Auto-detect `.7z` vs `.tar` by extension
- `.7z`: decrypt with passphrase (env, file, or prompt)
- `.tar`: legacy backward compat with deprecation warning
- Wrong passphrase = clean failure with helpful message
- Passphrase cleared from memory after use

## Security Properties

| Property | Before (tar) | After (7z) |
|----------|-------------|------------|
| Contents at rest | Plaintext | AES-256 encrypted |
| Filenames | Visible | Encrypted (mhe=on) |
| Transit security | SSH only | SSH + AES-256 |
| If intercepted | Full compromise | Useless without passphrase |
| SECRETS_MASTER_KEY | Plaintext in tar | Encrypted in 7z |

## Test Plan

- [ ] Export a tenant → verify .7z created
- [ ] `7z l` without password shows nothing (header encryption)
- [ ] Import .7z with correct passphrase → all secrets land correctly
- [ ] Import legacy .tar → works with warning
- [ ] Wrong passphrase → clean failure
- [ ] SECRETS_MASTER_KEY survives round-trip
- [ ] OMEMO store survives round-trip

---
