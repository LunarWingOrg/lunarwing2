# Configure Memory Embeddings

Embeddings are optional and disabled by default. When enabled, LunarWing uses them for vector-backed workspace memory search.

**Source revalidated 2026-11-20 for v2.2.2.0 (`9c96220bb2`).** Configuration
resolution is in `ic/src/config/embeddings.rs`; provider requests are in
`ic/src/workspace/embeddings.rs`. The full page was checked against the source.

## Configure in the Web UI

In the authenticated web UI, open **Settings → Inference** to expose:

- `embeddings.enabled`
- `embeddings.provider`
- `embeddings.model`
- `embeddings.base_url` for `openai_compatible`

The web UI saves these settings to the database. At startup, LunarWing can also
read legacy `settings.json` and TOML bootstrap config. An environment override
wins over the database value. A TOML value wins only when it differs from the
corresponding `Settings::default()` value; explicitly setting a value equal to
that built-in default does not count as an override. Use the web UI rather than
editing legacy JSON for normal operation.

The web UI offers `openai`, `lunarwing_cloud`, and `openai_compatible`. The backend also supports `ollama` when configured through the environment.

## Configure Through the Environment

Environment values override database-backed settings:

```dotenv
EMBEDDING_ENABLED=true
EMBEDDING_PROVIDER=openai_compatible
EMBEDDING_MODEL=text-embedding-3-small
EMBEDDING_BASE_URL=https://embeddings.example.com/openai
```

Set `OPENAI_API_KEY` through the protected instance environment, not in a
committed config file or shell history.

`EMBEDDING_BASE_URL` is the service root **before** `/v1/embeddings`; LunarWing
appends that path. For an endpoint ending in `/v1/embeddings`, leave off both
`/v1` and `/embeddings`.

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

Embedding and Ollama base URLs are checked when configuration loads. Without
`ALLOW_PRIVATE_IPS=1`, remote plain HTTP is refused and HTTPS resolving to
private, loopback, link-local or metadata addresses is rejected; localhost HTTP
is allowed. The opt-out applies to **mt-admin tenant environments and the
shipped service units as well as the private-network harness**, where
`ALLOW_PRIVATE_IPS=1` is set for access to private-network dependencies. It
permits any parseable HTTP or HTTPS URL in this configuration check; use trusted
endpoints and egress controls. For operators overriding that managed default,
removing the opt-in restores the stricter checks. The check does not pin
addresses at request time.

After changing startup environment values, restart the service through the appropriate service-management path. For multi-tenant installations, use `ic/scripts/lunarwing-mt-admin.sh` rather than direct tenant `systemctl` or `rc-service` commands.
