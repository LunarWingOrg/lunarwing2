# Architecture Docs Overview

Simple index of documents in `docs/architecture/`.

| File | Summary |
|------|---------|
| `ATOMICBOOL_DEEPER_PROPAGATION.md` | Design note on deeper propagation of AtomicBool cancellation flags through the message-handling and tool-loop paths. |
| `ENGINE-V2.md` | Architecture of the V2 engine (`ic/crates/lunarwing_engine/`), a unified thread-capability-CodeAct model replacing v1 abstractions. |
| `IRC-SENDER-IDENTITY.md` | How DarkIRC and WeeChat sender identity is versioned, case-normalized, and used as a routing/continuity hint rather than cryptographic proof. |
| `SELF_HEAL_DEPLOYMENT_WIRING.md` | Reference for infrastructure self-healing deployment and provisioning wiring (health checks, multi-tenant). |
| `SEMANTIC-MEMORY-SEARCH.md` | Persistent memory system with hybrid search (BM25 + vector embeddings, RRF fusion) in `ic/src/workspace/`. |
| `SSH_AGENT_HARNESS.md` | As-built reference for the per-tenant SSH bridge, agent socket forwarding, secrets, and host-key verification. |
| `SSH_DELIVERY_MECHANISMS.md` | The three shipped SSH delivery mechanisms and how they share the SSH agent harness core. |
| `WEECHAT-CHANNEL-ARCHITECTURE.md` | End-to-end IRC connectivity through WeeChat: components, message flow, latency model, config-precedence rules, and known issues. |
| `XMPP_FILE_TRANSFERS.md` | Bidirectional XMPP file transfer support (DMs and group chats, including OMEMO-encrypted). |
