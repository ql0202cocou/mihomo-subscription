-- 成功和失败的公开刷新都记录完成时间,以共享失败结果并限制重试。
ALTER TABLE profiles ADD COLUMN public_refresh_at TEXT;
ALTER TABLE profile_rule_sets ADD COLUMN last_fetch_at TEXT;
-- 定义更新递增版本,防止旧在途回源回写新定义的缓存。
ALTER TABLE profile_rule_sets ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;
