ALTER TABLE tool_capabilities
ADD COLUMN approval_policy TEXT NOT NULL DEFAULT 'never'
CHECK (approval_policy IN ('never', 'unless_auto_approved', 'always'));
