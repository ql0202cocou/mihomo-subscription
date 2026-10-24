-- 当前 Mihomo 已移除 relay 分组。停用遗留定义并使仍含它们的输出缓存失效,
-- 让下次生成按当前支持的分组类型重建;引用这些分组的配置需在管理端修正。
UPDATE custom_groups
SET enabled = 0, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')
WHERE group_type = 'relay';

DELETE FROM generated_cache
WHERE profile_id IN (SELECT profile_id FROM custom_groups WHERE group_type = 'relay');
