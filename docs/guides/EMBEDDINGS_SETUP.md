# Configure Memory Embeddings

Embeddings are optional and disabled by default. When enabled, LunarWing uses them for vector-backed workspace memory search.

**Revalidated against source 2026-09-12 (v2.1.1.0).** Environment knobs
(`EMBEDDING_ENABLED/PROVIDER/MODEL/BASE_URL/DIMENSION/CACHE_SIZE`,
`OLLAMA_BASE_URL`), the 10 000-entry default cache, the `>= 1` cache-size
floor, provider list, and shared outbound URL validation were re-checked
against `ic/src/config/embeddings.rs`; no changes were needed.

## Configure in the Web UI

The authenticated Configuration view exposes:

- `embeddings.enabled`
- `embeddings.provider`
- `embeddings.model`
- `embeddings.base_url` for `openai_compatible`

These values are database-backed settings. LunarWing no longer uses a standalone `settings.json` file for this configuration.

The web UI offers `openai`, `lunarwing_cloud`, and `openai_compatible`. The backend also supports `ollama` when configured through the environment.

## Configure Through the Environment

Environment values override database-backed settings:

```bash
EMBEDDING_ENABLED=true
EMBEDDING_PROVIDER=openai_compatible
EMBEDDING_MODEL=text-embedding-3-small
EMBEDDING_BASE_URL=https://embeddings.example.com/openai
OPENAI_API_KEY=<secret>
```

`EMBEDDING_BASE_URL` is the service root **before** `/v1/embeddings`; LunarWing appends that path. Do not include a trailing `/v1` unless the provider's API intentionally requires a doubled path.

Additional controls:

- `EMBEDDING_DIMENSION` overrides the dimension inferred from known model names.
- `EMBEDDING_CACHE_SIZE` controls the in-memory LRU entry limit and must be at least `1`; the default is `10000`.
- `OLLAMA_BASE_URL` selects the Ollama endpoint and defaults to `http://localhost:11434`.

## Provider Requirements

| Provider | Authentication | Endpoint configuration |
| --- | --- | --- |
| `lunarwing_cloud` | Active LunarWing Cloud session | LunarWing Cloud base URL |
| `openai` | `OPENAI_API_KEY` | OpenAI by default; `EMBEDDING_BASE_URL` can override it |
| `openai_compatible` | `OPENAI_API_KEY` | `EMBEDDING_BASE_URL`, or `https://api.openai.com` if omitted |
| `ollama` | None | `OLLAMA_BASE_URL` |

If an OpenAI-backed provider is enabled without `OPENAI_API_KEY`, LunarWing logs a warning and does not create the embedding provider.

## Network Safety

Embedding and Ollama base URLs pass the shared outbound URL validation. Remote plain HTTP, private ranges, link-local addresses, and cloud metadata targets are rejected by default; localhost HTTP is allowed. `ALLOW_PRIVATE_IPS=1` disables those private-address restrictions and should be used only in an intentionally isolated deployment, such as the committed private-network harness configuration.

After changing startup environment values, restart the service through the appropriate service-management path. For multi-tenant installations, use `ic/scripts/lunarwing-mt-admin.sh` rather than direct tenant `systemctl` or `rc-service` commands.
