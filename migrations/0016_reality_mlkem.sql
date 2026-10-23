-- 按订阅控制是否为 REALITY 节点缺省补 `support-x25519mlkem768: true`:新版 Xray 服务端要求该密钥
-- 协商,旧版 REALITY 服务端却不兼容(mihomo 默认关闭的原因),机场节点无法逐个编辑,只能按订阅
-- (= 按机场)取舍。默认开启;节点里显式写的值不受影响。
ALTER TABLE profiles ADD COLUMN reality_mlkem INTEGER NOT NULL DEFAULT 1;
-- 随生成缓存保存 ML-KEM 已开启但 `client-fingerprint` 不是 chrome 的 REALITY 节点名(JSON 数组,
-- 无则 NULL),供详情页常驻告警;回源与离线重生成都会刷新。升级前的缓存为 NULL,下次写缓存后补齐。
ALTER TABLE generated_cache ADD COLUMN reality_fingerprint_risks TEXT;
