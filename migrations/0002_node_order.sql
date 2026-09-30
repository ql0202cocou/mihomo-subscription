-- Persisted manual node ordering for a profile.
--
-- JSON array of proxy names (provider + custom). NULL means "default order":
-- provider proxies in upstream order, then enabled custom nodes by created_at.
-- When set, names present here are emitted first in this order; any node not
-- listed (newly added provider/custom node) falls back to the end in default
-- order. Drives both the generated `proxies` output and the preview list.
ALTER TABLE profiles ADD COLUMN node_order TEXT;
