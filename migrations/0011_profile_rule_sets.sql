-- 0011_profile_rule_sets.sql — per-profile self-contained rule library (③ 托管规则库).
--
-- Each profile owns its rule-set definitions. A `RULE-SET,<name>,<policy>` rule in the
-- profile's rules references one by name; on conversion the panel injects a matching
-- `rule-providers:` entry pointing at a PER-PROFILE hosted link
-- `/<prefix>/api/sub/<token>/r/<name>/<behavior>.<format>` (token-scoped, so two profiles
-- may reuse the same name without colliding). The subscription is now self-contained:
-- generation reads ONLY this table, never the global `rule_sets` library (0008).
--
-- The global `rule_sets` table (②) is kept as a user template/import source only — it no
-- longer participates in generation or public hosting (see src/rule_sets.rs).
--
-- Mirrors the global `rule_sets` schema minus `position` (rule-providers are an unordered
-- map). Two sources:
--   * `manual` — admin payload in `content`, rendered on serve (`yaml` -> `payload:` list,
--     `text` -> verbatim lines).
--   * `remote` — mirror a remote rule-provider from `url`. With `cache=1` the panel lazily
--     re-fetches past `interval_hours`, stores raw bytes in `cached_body` (BLOB, so binary
--     `mrs` survives) and re-hosts them; with `cache=0` the injected rule-provider points at
--     `url` directly (panel does not host).

CREATE TABLE profile_rule_sets (
    id                TEXT    PRIMARY KEY,
    profile_id        TEXT    NOT NULL REFERENCES profiles (id) ON DELETE CASCADE,
    name              TEXT    NOT NULL,
    behavior          TEXT    NOT NULL CHECK (behavior IN ('domain', 'ipcidr', 'classical')),
    format            TEXT    NOT NULL CHECK (format IN ('yaml', 'text', 'mrs')),
    source            TEXT    NOT NULL DEFAULT 'manual' CHECK (source IN ('manual', 'remote')),
    content           TEXT    NOT NULL DEFAULT '',
    -- 规则条数(manual=payload 行数;remote=最近成功镜像的行数,mrs 为 0),列表展示免读 BLOB。
    rule_count        INTEGER NOT NULL DEFAULT 0,
    url               TEXT,
    interval_hours    INTEGER NOT NULL DEFAULT 24,
    cache             INTEGER NOT NULL DEFAULT 1,
    cached_body       BLOB,
    cached_at         TEXT,
    last_fetch_status TEXT,
    enabled           INTEGER NOT NULL DEFAULT 1,
    created_at        TEXT    NOT NULL,
    updated_at        TEXT    NOT NULL,
    UNIQUE (profile_id, name)
);

CREATE INDEX idx_profile_rule_sets_profile ON profile_rule_sets (profile_id);
