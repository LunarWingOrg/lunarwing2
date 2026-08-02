# XMPP known issues

Reconciled 2026-08-02 against `ic/src/channels/xmpp/mod.rs`.

- **OMEMO device trust** — the agent's OMEMO device may need to be trusted in a separate client
  before encrypted messages flow. This is XEP-0384 behavior, not a defect.
- **Inbound file uploads — implemented (incl. encrypted media), live e2e validation pending.** The
  channel answers `disco#info` + advertises entity caps (so clients recognize the agent as a valid
  recipient), extracts OOB (`<x xmlns='jabber:x:oob'>`) and `aesgcm://` (XEP-0454) URLs — the latter
  also from decrypted OMEMO bodies — and downloads them with bounded concurrency, a per-stanza URL
  cap, and a streamed 20 MB size cap; `aesgcm://` media is AES-256-GCM-decrypted locally. This is
  unit-tested, but the full pipeline has **not** yet been exercised end-to-end against a live server
  (Conversations/Gajim → agent). See `docs/architecture/XMPP_FILE_TRANSFERS.md`.
- **Inbound download SSRF guard — fixed.** Sender-supplied OOB and `aesgcm://` fetches go through
  `policy_checked_download_client()`: HTTPS is required by default, embedded credentials are
  rejected, every resolved address must be public, validated addresses are pinned against DNS
  rebinding, redirects are disabled, and environment proxies are ignored. `ALLOW_PRIVATE_IPS=1`
  is the explicit local/private-network opt-out and also permits plain HTTP.
- **OMEMO MUC fallback spam / rare stuck processing loop** — historically observed; appear resolved.
  The original tracker is archived at
  [`../bugs/history/XMPP-OMEMO-BUG-TO-DO.md`](../bugs/history/XMPP-OMEMO-BUG-TO-DO.md);
  reopen if either symptom recurs.
