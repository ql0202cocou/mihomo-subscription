-- 0008_rule_sets.sql — Global hosted rule-sets (规则集托管 / 规则配置).
--
-- A GLOBAL library of named rule-sets the panel HOSTS at a permanent link
-- `/<prefix>/r/<name>/<behavior>.<format>`. Any profile references one via a
-- `RULE-SET,<name>,<policy>` rule; on conversion the panel injects a matching
-- `rule-providers:` entry pointing at the hosted link (custom overrides a
-- provider entry of the same name), so the client fetches the rule list from
-- this panel. Mirrors the `global_nodes` pool (Model C): a single cross-profile
-- set, `name` UNIQUE (it is both the URL path segment and the RULE-SET ref name),
-- ordered by `position`.
--
-- This re-introduces the rule-set hosting removed in 0.2.3 (the dropped
-- `rule_providers` table was per-profile and only injected map entries; this is a
-- global pool that the panel itself serves).
--
-- Two sources:
--   * `manual` — admin-authored payload in `content`; rendered on serve
--     (`yaml` -> a `payload:` list, `text` -> verbatim lines).
--   * `remote` — mirror a remote rule-provider from `url`. With `cache=1` the
--     panel lazily re-fetches (every pull, refreshed past `interval_hours`),
--     stores the raw bytes in `cached_body` (BLOB, so binary `mrs` survives), and
--     re-hosts them at the stable link, shielding upstream flakiness. With
--     `cache=0` the panel does not host it; the injected rule-provider points at
--     `url` directly. `last_fetch_status` mirrors the profile fetch labels.

CREATE TABLE rule_sets (
    id                TEXT    PRIMARY KEY,
    name              TEXT    NOT NULL UNIQUE,
    behavior          TEXT    NOT NULL CHECK (behavior IN ('domain', 'ipcidr', 'classical')),
    format            TEXT    NOT NULL CHECK (format IN ('yaml', 'text', 'mrs')),
    source            TEXT    NOT NULL DEFAULT 'manual' CHECK (source IN ('manual', 'remote')),
    content           TEXT    NOT NULL DEFAULT '',
    -- 规则条数(manual=payload 行数;remote=最近成功镜像的行数,mrs 为 0),供列表展示免读 BLOB。
    rule_count        INTEGER NOT NULL DEFAULT 0,
    url               TEXT,
    interval_hours    INTEGER NOT NULL DEFAULT 24,
    cache             INTEGER NOT NULL DEFAULT 1,
    cached_body       BLOB,
    cached_at         TEXT,
    last_fetch_status TEXT,
    enabled           INTEGER NOT NULL DEFAULT 1,
    position          INTEGER NOT NULL DEFAULT 0,
    created_at        TEXT    NOT NULL,
    updated_at        TEXT    NOT NULL
);

CREATE INDEX idx_rule_sets_position ON rule_sets (position);
