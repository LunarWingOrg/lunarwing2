# IRC Sender Identity

**Revalidated against source 2026-10-01 (v2.1.4.0); re-checked 2026-10-05 (v2.1.5.0).** Principal formats
(`darkirc:nick:<case-folded-nick>`, `account:<network>:…` /
`nick:<network>:…`, `weechat:dm:v2:<principal>`,
`weechat:group:<network>:<target>`), the owner-actor settings, and the
migration/retention policy below were re-checked against
`ic/channels-src/{darkirc,weechat}` and `ic/src/extensions/manager.rs` and
still hold. The 2.1.2.0 release only bumped the channel crate/registry
versions (darkirc 0.1.8, weechat 0.2.9); no principal-format or pairing
logic changed. The 2.1.3.0 release again only bumped the channel
crate/registry versions (darkirc 0.1.9, weechat 0.2.10); no
principal-format or pairing logic changed. The 2.1.4.0 release again only
bumped the channel crate/registry versions (darkirc 0.1.10, weechat
0.2.11; channel crates now at workspace version 2.1.4); no
principal-format or pairing logic changed. The 2.1.5.0 release again only
bumped the channel crate/registry versions (darkirc 0.1.11, weechat
0.2.12; channel crates now at workspace version 2.1.5); no
principal-format or pairing logic changed.

LunarWing treats IRC sender identity as a routing and continuity hint, not as
cryptographic proof of a person. DarkIRC and WeeChat use versioned,
case-normalized principals so pairing, owner recognition, and Engine V2
conversation scope agree on the same value.

## DarkIRC

DarkIRC exposes only a nick through the local adapter. LunarWing applies the
RFC1459 case mapping and constructs this principal:

```text
darkirc:nick:<case-folded-nick>
```

For example, `Alice`, `ALICE`, and `alice` all map to
`darkirc:nick:alice`. RFC1459 equivalents such as `Nick[One]` and
`nick{one}` also map together. The DM conversation key is:

```text
darkirc:dm:v2:darkirc:nick:<case-folded-nick>
```

The original nick remains in channel metadata and is used only for the adapter
reply target. Pairing requests use the normalized principal. Legacy pairing and
allow-list entries containing a bare nick are still accepted after the same
case fold.

DarkIRC does not provide an authenticated account or stable network-scoped
identity. Nick reuse can therefore transfer continuity to another network
participant. Operators must not treat a paired DarkIRC nick as strong
authentication for high-impact actions.

## WeeChat

Every WeeChat principal includes the IRC network. If the relay line tags carry
an authenticated account (`account_`, `account=`, or `irc_account_`), LunarWing
prefers it:

```text
account:<network>:<case-folded-account>
```

Otherwise it falls back to the nick:

```text
nick:<network>:<case-folded-nick>
```

Thus the same nick on `libera` and `oftc` cannot share a DM principal. The DM
conversation key is `weechat:dm:v2:<principal>`. Group scope is
`weechat:group:<network>:<target>` rather than an individual sender; the full
`irc.<network>.<target>` buffer remains in routing metadata. The original nick
also remains in metadata. The hostmask is used for sender allow-list checks but
is not stored in emitted metadata or treated as an authenticated account.

The adapter may supply `casemapping_ascii`, `casemapping_rfc1459`, or
`casemapping_strict-rfc1459` in line tags. LunarWing uses that mapping when
present and defaults to RFC1459 otherwise. Pairing requests and owner actor
bindings use the same principal as the DM conversation scope.

## Owner Routing

The legacy `channels.wasm_channel_owner_ids.<channel>` numeric setting remains
supported without conversion. IRC and JID-based channels can instead configure:

```text
channels.wasm_channel_owner_actor_ids.<channel>
```

The string actor setting takes precedence when both are present. Recommended
values are the exact emitted principal, such as `account:libera:alice`,
`nick:libera:alice`, or `darkirc:nick:alice`.

Only messages whose sender principal matches the configured owner actor may
replace owner-scoped proactive routing metadata. WeeChat persists the complete
`irc.<network>.<target>` buffer; DarkIRC persists one validated DM nick. Invalid
or ambiguous targets are rejected before persistence. Stored owner routing is
restored after restart. Heartbeat, routine, mission, and message-tool fallbacks
address the LunarWing owner scope; they do not pass the actor principal as a
protocol target. The channel wrapper translates the owner scope through the
stored protocol metadata.

## Migration And Retention

The v2 principal format intentionally changes IRC DM conversation keys. Existing
pre-v2 conversations and messages remain in the database and are not deleted,
rewritten, merged, or automatically attached to a new principal. New IRC
traffic uses the v2 key immediately.

This retention policy avoids silently merging two people after a nick or
account change. Operators that need old context should review and migrate it
explicitly after confirming the old and new principals represent the same
person. Legacy bare-nick pairing entries continue to authorize their normalized
equivalent, but new approvals are stored under the v2 principal.
