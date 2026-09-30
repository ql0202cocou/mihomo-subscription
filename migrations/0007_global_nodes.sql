-- 0007_global_nodes.sql — Global custom nodes (cross-profile pool).
--
-- Custom proxy nodes become GLOBAL: a single pool appended to EVERY profile's
-- output, edited and ordered in one place ("节点配置"). This replaces the
-- per-profile `custom_nodes` table. `position` is the global custom-block order
-- (read with `ORDER BY position, name`; `name` breaks ties deterministically).
-- A profile's detail page shows the custom block read-only; the live public link
-- always re-fetches + reconverts, so a global-node change reaches every profile
-- on its next pull without per-profile regeneration.
--
-- Existing per-profile `custom_nodes` are migrated into the pool, de-duplicated
-- by name (a name may have existed under several profiles): the most recently
-- updated row wins. Migrated rows all start at `position = 0`, so their initial
-- effective order is alphabetical by name until the admin reorders; new nodes
-- created afterwards get an incrementing `position` (creation order).
--
-- `profiles.node_order` (the old per-profile custom-block order) is now unused
-- and left in place (always NULL going forward) — node ordering lives in
-- `global_nodes.position`. `profiles.node_section_order` is kept per profile
-- (each profile still chooses provider-first vs custom-first placement).

CREATE TABLE global_nodes (
    id          TEXT    PRIMARY KEY,
    name        TEXT    NOT NULL UNIQUE,
    node_type   TEXT    NOT NULL,
    content     TEXT    NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 1,
    position    INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT    NOT NULL,
    updated_at  TEXT    NOT NULL
);

CREATE INDEX idx_global_nodes_position ON global_nodes (position);

-- Migrate per-profile custom nodes into the global pool, keeping one row per
-- name (the most recently updated). `GROUP BY name` collapses duplicates; the
-- `updated_at = MAX(...)` filter picks the freshest row for each name.
INSERT INTO global_nodes (id, name, node_type, content, enabled, position, created_at, updated_at)
SELECT cn.id, cn.name, cn.node_type, cn.content, cn.enabled, 0, cn.created_at, cn.updated_at
FROM custom_nodes cn
WHERE cn.updated_at = (
    SELECT MAX(cn2.updated_at) FROM custom_nodes cn2 WHERE cn2.name = cn.name
)
GROUP BY cn.name;

DROP TABLE custom_nodes;
