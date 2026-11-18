ALTER TABLE profile_rule_sets ADD COLUMN pending_global_name TEXT
    CHECK (pending_global_name IS NULL OR global_rule_set_id IS NULL);

CREATE INDEX idx_profile_rule_sets_pending_global ON profile_rule_sets(pending_global_name, profile_id)
    WHERE pending_global_name IS NOT NULL;

-- Remember the source name before the foreign key loses it. A profile may use a local alias.
CREATE TRIGGER remember_deleted_global_rule_set BEFORE DELETE ON rule_sets
BEGIN
    UPDATE profile_rule_sets SET pending_global_name = OLD.name, global_rule_set_id = NULL
    WHERE global_rule_set_id = OLD.id;
END;
