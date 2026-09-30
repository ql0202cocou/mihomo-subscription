-- Persisted manual proxy-group ordering for a profile.
--
-- JSON array of proxy-group names (provider + custom). NULL means "default
-- order": provider groups in upstream order, then enabled custom groups by
-- created_at. When set, names present here are emitted first in this order; any
-- group not listed (newly added provider/custom group) falls back to the end in
-- default order. Mirrors `node_order` (0002) but for `proxy-groups`.
ALTER TABLE profiles ADD COLUMN group_order TEXT;
