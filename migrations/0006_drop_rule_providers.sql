-- 0006_drop_rule_providers.sql — remove the custom rule-providers (规则集) feature.
--
-- The project no longer hosts/manages custom rule-providers: the converter only
-- passes the provider's own `rule-providers:` map through (so imported provider
-- `RULE-SET` rules keep resolving), and there is no admin CRUD for custom 规则集.
-- Drop the now-unused table. `IF EXISTS` keeps this idempotent and safe whether
-- the table was created by 0005 (existing installs) or never existed.

DROP INDEX IF EXISTS idx_rule_providers_profile;
DROP TABLE IF EXISTS rule_providers;
