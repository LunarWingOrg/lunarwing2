# Explanation

> **Current status (2026-07-30, branch `aj/kimi-2-20260730-0601`): FIXED (options 1+2 at the health layer).** A per-tenant
> `enable_weechat` registry flag (default: enabled) now gates health
> classification of the optional weechat units. `mt-admin disable-weechat
> <tenant>` records `enable_weechat=false`; `health-openrc.sh` and
> `health-systemd.sh` then classify down `lunarwing-weechat-<t>` /
> `lunarwing-weechat-adapter-<t>` units as `skipped` (never critical), so
> self-heal does not restart-storm them and `render-units` is no longer a
> footgun for weechat-disabled tenants. Fail-safe: absent flag / unreadable
> registry / missing jq all keep the old critical classification. Note the
> gate covers health + self-heal only — units are still rendered and are not
> stopped. The tmux duplicate-session start failure itself (option 3) remains
> open. Regression coverage: test-health-openrc.sh S9–S11,
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
