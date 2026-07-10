# AGENT PRE-RELEASE CHECKLIST for 2.0.0.0 Codename `?`
**Open TODOs (2.0.0.0) — To be done before release**

### Helps to do items in order (generally)

NOTE: TODO: edit dev autonomous loop routine to use this file. ensure that checkbox is checked off in the corresponding branch before committing and pushing and opening pr

1. [x] Ensure references to 1.2.0 in the code and documentation are replaced by 2.0.0 (where applicable only of course)
2. [ ] mcp
3. [ ] Finalize goals
4. [ ] Drop legacy `ironclaw-agent-v1` subprotocol offer from the daemon (delete the `SUBPROTOCOL_LEGACY` offer in `ic/src/orchestrator/external_worker.rs`) and `git rm` the 1.1.9-only repo-root compat symlinks `ironclaw_weechat_wss`, `darkirc_channel_for_ironclaw` (deployed tenants must have re-run mt-admin unit regen by then)
5. [ ] LunarWing Web UI performance overhaul
6. [ ] LunarWing Web MT admin setup integration (part 2 of earlier plan discussed)
9. [x] inspect status of cargo crates and create documented report of any crates that might still need to be updated
10. 19. [x] Run all cargo tests — full `--all-features --no-fail-fast` run: lib 4087 passed/0 failed/4 ignored; all integration binaries + doctests pass except the 6 known-deferred (4 `multi_tenant_system_prompt` architectural, 2 `e2e_advanced_traces` bootstrap-greeting). See `docs/proposals/CARGO_TESTS_FIX.md`.
20. [x] Fix any remaining broken cargo tests and ensure updated documentation. Create (or rewrite) new tests if necessary. then re-run cargo tests to ensure — Fixed the 3 stale `lib` failures this cycle: `registry::embedded::tests::test_load_embedded_parses` (github→ssh sentinel), `cli::tests::test_help_output` + `test_long_help_output` (accepted rebranded insta snapshots). Lib re-run: 4087 passed/0 failed. The 6 remaining failures are documented known-deferred (architectural / harness), not regressions.

11. [x] Finish going through all the documents under architecture directory in docs/ and update all outdated documentation. Then, consolidate documents if possible.
12. [x] Go through all documents under bugs directory in docs/ and update all outdated documentation. Then, consolidate documents.
13. [x] Update README.md - purge all outdated sections. Include the new MT Admin CLI wrapper into the README.md as part of the new, improved up to date getting started section. You may also refer to the actual mt admin setup script which the new CLI wrapper references since it offers far greater control.
14. [x] Improve accuracy of RELEASE-v1.1.9.0.md (from item #15)
15. [x] Go through all documents under proposals directory in docs/ and update all outdated documentation. Then, consolidate documents if deemed necessary. 
16. [ ] Go through all documents under reviews directory in docs/ and update all outdated documentation. Then, consolidate documents.
17. [ ] Go through all documents under guides directory in docs/ and update all outdated documentation. Then, consolidate documents.
18. [x] Write up FIRST DRAFT release notes (at root of repo) for v1.1.9.0 (note the version schema change) explaining all relevant changes since v1.1.8 as well as revising and including an ACCURATE VERSION OF `known issues list`. Use previous release notes in docs/release for reference as to how to write up this document. The codename for this release is: `Kiyome (きよめ/清め)` - The file you write will be RELEASE-v1.1.9.0.md and should be written to the ROOT of the repo.
19. [ ] Go through all documents under ops directory in docs/ and update all outdated documentation. Then, consolidate documents.
20. [ ] Write up a short doc with details of currently open PRs and Issues. Save it to docs/ops

---

