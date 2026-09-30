-- 去掉「原始订阅类型」概念。转换器只按 Clash/Mihomo YAML 解析(`yaml::parse_mapping`),
-- source_type 从不参与转换;surge/loon 是选了即解析失败的假选项。删列彻底移除该字段。
ALTER TABLE profiles DROP COLUMN source_type;
