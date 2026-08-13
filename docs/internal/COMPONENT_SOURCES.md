# Custom Component Sources

Where each custom LunarWing component's **standalone upstream source repo** lives. Most of these
components now also live **in-tree** (see the repo-layout and "Where to Work" lists in the root
`AGENTS.md`); these URLs are the original external repos they were developed in. Consolidated
2026-06-19 from the scattered `custom_*` notes that previously lived in `docs/internal/`.

| Component | Kind | Source repo |
|-----------|------|-------------|
| DarkIRC channel | channel | `git.sobe.world/cmc/darkirc_channel_for_ironclaw.git` |
| WeeChat WSS channel | channel | `git.sobe.world/cmc/ironclaw_weechat_wss.git` |
| enjin | external script | custom WeeChat plugin for E2E with state-of-the-art crypto *(no repo URL recorded)* |
| Secret Manager (`ic_sm`) | external script | `git.sobe.world/cmc/ic_sm.git` |
| Infra health check | healthcheck / timers | `git.sobe.world/cmc/ic-infrastructure-health-check.git` |
| git-md (`ironclaw-md-git`) | tool (planned) | `git.sobe.world/starforce/ironclaw-md-git.git` — **not in-tree**; proposal is OPEN/PLANNED at `docs/proposals/GITWASM/README.md` (would land as `lunarwing-md-git`) |
| Gotify | tool | `git.sobe.world/cmc/gotify-wasm.git` |
