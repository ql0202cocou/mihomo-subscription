-- 0005_rule_providers.sql — custom Mihomo rule-providers (规则集) per profile.
--
-- A `RULE-SET,<name>,<policy>` rule references a named entry under the output
-- `rule-providers:` map. Provider rule-providers still pass through; these custom
-- entries are MERGED on top (custom overrides a provider entry of the same name),
-- so the admin can define their own 规则集 without breaking imported provider
-- RULE-SET rules. Mirrors `custom_groups`: typed `provider_type`/`behavior`
-- columns for display/validation plus an `options` JSON blob for the rest
-- (url, path, payload, format, interval, size-limit, proxy, ...).

CREATE TABLE rule_providers (
    id            TEXT    PRIMARY KEY,
    profile_id    TEXT    NOT NULL REFERENCES profiles (id) ON DELETE CASCADE,
    name          TEXT    NOT NULL,
    provider_type TEXT    NOT NULL CHECK (provider_type IN ('http','file','inline')),
    behavior      TEXT    NOT NULL CHECK (behavior IN ('domain','ipcidr','classical')),
    options       TEXT,
    enabled       INTEGER NOT NULL DEFAULT 1,
    created_at    TEXT    NOT NULL,
    updated_at    TEXT    NOT NULL,
    UNIQUE (profile_id, name)
);

CREATE INDEX idx_rule_providers_profile ON rule_providers (profile_id);
