# Explanation

> **Current status (2026-07-31, branch `aj/kimi-2-20260731-0001`): FIXED at
> health and service-lifecycle layers.** The per-tenant `enable_weechat` flag
> (default: enabled) controls health classification, rendered units, daemon
> dependencies, startup, and boot persistence on systemd user managers and
> OpenRC. `disable-weechat <tenant>` first requires WeeChat stop/disable to
> succeed, then records `enable_weechat=false` and re-renders without the units
> or dependencies. `enable-weechat <tenant>` records true, restores unit files
> and dependencies, then strictly enables and starts both services. Disabled
> units remain `skipped` in health; absent flag / unreadable registry / missing
> jq retain old fail-safe behavior. The tmux duplicate-session start failure
> itself (option 3) remains open. Regression coverage:
> `test-weechat-lifecycle-gating.sh`, test-health-openrc.sh S9–S11, and
> test-health-systemd.sh E–F.
>
> **Previous status (2026-07-20, rev `50c8f99`): PARTIAL.** The `render-units`
> verb and PostgreSQL readiness fix landed. The optional-WeeChat footgun surfaced
> here remains open: backend units are still rendered without a tenant-level
> enablement gate and can be classified unhealthy when intentionally unused.

  Net result

  - render-units <name> verb added — re-renders a tenant's units from the current generator, no restart, no secret/env touching. The proper mechanism for
  applying generator changes (like Rank 15) to existing tenants.
  - Verified live on both tenants: it correctly applied the Rank 15 pg_isready gate and the fold-in A weechat rename.
  - Live host restored to healthy: removed the materialized lunarwing-weechat-<t> backend units, reverted the adapter deps to the existing weechat-<t>, pg
  units still gated on pg_isready.

  The real bug it surfaced (worth a follow-up)

  fold-in A renamed weechat-<t> → lunarwing-weechat-<t>. That makes the weechat backend match health-openrc's lunarwing-* discovery glob — but the backend is 
  optional and, on these tenants, can't start (duplicate session: weechat — a tmux-session conflict; weechat is only half-configured, relay unreachable). So a
  stopped/unstartable optional weechat backend now reports critical, and self-heal would try to restart a unit that can't come up → flap → escalate.

  This is pre-existing in the merged code, not specific to render-units — a fresh add-tenant on the current generator hits it too (it renders the weechat
  backend; if it doesn't start, health goes critical). So render-units is a footgun until this is fixed (re-running it recreates the critical units).

  Fix options (follow-up):
  1. health-openrc.sh: treat the weechat backend/adapter as optional — stopped → degraded (or skip), never critical; and self-heal shouldn't restart-storm an
  optional unit that can't start.
  2. Generator: only render/enroll the weechat backend when weechat is actually enabled for the tenant (gate on a WEECHAT_ENABLED-type flag).
  3. Fix weechat itself (the tmux duplicate session start failure).
