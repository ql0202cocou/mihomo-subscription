-- Older copies have no recorded origin. The administrator can link them explicitly.
ALTER TABLE profile_rule_sets ADD COLUMN global_rule_set_id TEXT REFERENCES rule_sets(id) ON DELETE SET NULL;
CREATE INDEX idx_profile_rule_sets_global ON profile_rule_sets(global_rule_set_id, profile_id)
    WHERE global_rule_set_id IS NOT NULL;
