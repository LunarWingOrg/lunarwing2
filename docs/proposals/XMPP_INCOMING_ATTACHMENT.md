> **Current status (2026-07-20, rev `50c8f99`): SUPERSEDED.** This pasted warning
> is stale: `incoming_attachment_for_url` is test-gated and used by current XMPP
> regression tests.

warning: function `incoming_attachment_for_url` is never used
    --> src/channels/xmpp/mod.rs:2962:4
     |
2962 | fn incoming_attachment_for_url(url: &str) -> IncomingAttachm...
     |    ^^^^^^^^^^^^^^^^^^^^^^^^^^^
     |
     = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by de
fault

    Building [=====================> ] 1053/1055: lunarwing
