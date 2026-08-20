> **Current status (reverified 2026-08-20): SUPERSEDED.** The pasted warning is
> stale and its 2026-07-20 explanation is now stale too:
> `incoming_attachment_for_url` no longer exists in the tree. Inbound XMPP
> attachments are handled by the OOB/aesgcm pipeline in
> `ic/src/channels/xmpp/mod.rs` (`collect_inbound_attachment_urls`,
> `extract_inbound_attachments`, `download_oob_file`,
> `download_aesgcm_file`), with egress/SSRF policy checks
> (`policy_checked_download_client`) added in v2.0.8.0 hardening.

warning: function `incoming_attachment_for_url` is never used
    --> src/channels/xmpp/mod.rs:2962:4
     |
2962 | fn incoming_attachment_for_url(url: &str) -> IncomingAttachm...
     |    ^^^^^^^^^^^^^^^^^^^^^^^^^^^
     |
     = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by de
fault

    Building [=====================> ] 1053/1055: lunarwing
