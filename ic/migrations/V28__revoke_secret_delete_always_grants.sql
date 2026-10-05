-- WI#307: secret deletion now requires approval for each invocation. Drop
-- legacy tool-wide grants for every owner, retaining disabled/ask settings.
-- No secret values or other tool permissions are changed. Downgrading does
-- not restore these grants; any future policy change requires fresh consent.
DELETE FROM settings
WHERE key = 'tool_permissions.secret_delete'
  AND value = '"always_allow"'::jsonb;
