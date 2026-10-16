-- 随生成缓存保存产出该缓存所用的规则文本:重置 token/公共前缀时,若当前规则草稿未通过
-- 校验,改用这份最近合法规则离线重生成,把缓存中的规则集托管链接迁移到新值(否则新链接
-- 仍指向已撤销的旧托管地址)。升级前的缓存为 NULL,在下一次成功写缓存后补齐。
ALTER TABLE generated_cache ADD COLUMN rules_snapshot TEXT;
