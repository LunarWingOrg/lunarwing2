# Explanation

> **Current status (2026-07-30, branch `aj/kimi-2-20260730-0301`): FIXED (option 1).**
> The optional-WeeChat footgun is closed by a per-tenant health-glob gate:
> `.tenants[<name>].enable_weechat` in the ports registry (absent = true,
> preserving legacy fail-loud behavior) now gates weechat classification in
> `ic-infrastructure-health-check/health-openrc.sh` and `health-systemd.sh` —
> a non-active `lunarwing-weechat(-adapter)-<t>` unit on a weechat-disabled
> tenant reports `skipped` (never critical; self-heal already honours
> `skipped`, so no restart-storm flap). mt-admin persists the flag from the
> add-tenant relay-bootstrap outcome, sets it true on successful
> `configure-weechat-relay`, and exposes manual `enable-weechat` /
> `disable-weechat` verbs. Re-running `render-units` on a half-configured
> tenant no longer creates criticals once the tenant is flagged (and fresh
> `--no-weechat-bootstrap` / failed-bootstrap tenants are flagged at
> provision time). Option 2 (generator-side pruning of rendered units) and
> option 3 (the tmux duplicate-session start failure itself) remain open
> follow-ups; the rendered units are intentionally left in place.

> **Former status (2026-07-20, rev `50c8f99`): PARTIAL.** The `render-units`
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
