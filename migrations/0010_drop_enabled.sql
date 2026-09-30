-- 移除订阅的「启用/禁用」概念:所有订阅默认且恒为启用,公开链接不再按 enabled 过滤。
-- 必须先删依赖该列的索引,否则 SQLite 的 DROP COLUMN 会失败。
DROP INDEX IF EXISTS idx_profiles_enabled;
ALTER TABLE profiles DROP COLUMN enabled;
