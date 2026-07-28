-- Managed Cloudflare, ngrok, and Tailscale tunnel providers were removed.
-- Scrub provider-specific settings, including plaintext credential rows.
-- Keep tunnel.provider so startup can require explicit operator migration
-- instead of silently disabling webhook exposure.
UPDATE settings
SET value = value
    - 'cf_token'
    - 'ngrok_token'
    - 'ngrok_domain'
    - 'ts_funnel'
    - 'ts_hostname'
WHERE key = 'tunnel'
  AND jsonb_typeof(value) = 'object';

DELETE FROM settings
WHERE key IN (
    'tunnel.cf_token',
    'tunnel.ngrok_token',
    'tunnel.ngrok_domain',
    'tunnel.ts_funnel',
    'tunnel.ts_hostname'
);
