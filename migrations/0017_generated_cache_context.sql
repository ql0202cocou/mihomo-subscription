-- 生成缓存记录构建时的来源版本与公开能力(token + 公共前缀)摘要。
-- NULL 表示升级前缓存或已被编辑标脏;不得把未知上下文回填成当前版本。
ALTER TABLE generated_cache ADD COLUMN source_revision INTEGER;
ALTER TABLE generated_cache ADD COLUMN capability_hash TEXT;
