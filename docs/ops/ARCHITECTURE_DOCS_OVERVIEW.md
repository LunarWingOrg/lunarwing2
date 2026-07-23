# docs/architecture — Document Overview

Simple index of architecture documents in `docs/architecture/`.

| File | Summary |
|------|---------|
| `ATOMICBOOL_DEEPER_PROPAGATION.md` | Design note for deeper AtomicBool propagation of cancellation/response-suppression signals. Not yet implemented. |
| `ENGINE-V2.md` | Engine V2 architecture: unified thread-capability-CodeAct execution model replacing ~10 v1 abstractions with 5 core primitives. |
| `IRC-SENDER-IDENTITY.md` | How IRC sender identity is treated as a routing/continuity hint rather than cryptographic proof, with versioned case-normalized principals. |
| `SELF_HEAL_DEPLOYMENT_WIRING.md` | Infrastructure self-healing deployment and provisioning wiring as-built reference. |
| `SEMANTIC-MEMORY-SEARCH.md` | Hybrid (BM25 + vector) persistent memory search with Reciprocal Rank Fusion, exposed via memory tools. |
| `SSH_AGENT_HARNESS.md` | As-built per-tenant SSH bridge: agent socket, secrets, and host-key verifier. |
| `SSH_DELIVERY_MECHANISMS.md` | Three SSH delivery mechanisms built on top of the SSH agent harness. |
| `WEECHAT-CHANNEL-ARCHITECTURE.md` | WeeChat channel architecture: components, message flow, latency model, config precedence, and known issues. |
| `XMPP_FILE_TRANSFERS.md` | XMPP file transfer support in both directions, including OMEMO-encrypted transfers in DMs and group chats. |
