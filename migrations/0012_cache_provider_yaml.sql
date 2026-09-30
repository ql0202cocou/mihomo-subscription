-- 随生成缓存保存机场原文:规则/排序编辑、重置 token/前缀后可离线重跑完整转换(含校验与
-- rule-providers 注入),不必重拉机场。升级前的缓存为 NULL,在下一次回源拉取后补齐。
ALTER TABLE generated_cache ADD COLUMN provider_yaml TEXT;
