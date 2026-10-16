-- 来源版本:更换 source_url 时递增(并清空旧缓存、重置公开刷新时间),使换源前已在途的
-- 旧来源回源结果被识别并丢弃——回源状态记录与缓存回写都绑定该版本,不再写成新鲜缓存。
ALTER TABLE profiles ADD COLUMN source_revision INTEGER NOT NULL DEFAULT 0;
