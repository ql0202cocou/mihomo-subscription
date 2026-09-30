//! 对不受信任/管理员提交的文档做限界 YAML 解析。
//!
//! 按 `docs/architecture.md`「安全设计」,来自信任边界之外的 YAML(此处是管理员的节点/分组 content;
//! 转换器里是机场内容)必须带资源限制解析:解析后限制嵌套深度与节点数,并在解析 *之前* 限制
//! 锚点/别名数。
//!
//! 锚点/别名上限是对别名扩展(「billion laughs」)的防御:这类输入极小,故大小上限与解析后的检查
//! 都帮不上忙——`serde_yaml` 已在 `from_str` 内把炸弹展开(并 OOM)了。因此先扫原始文本,拒绝
//! 锚点/别名数量离谱的文档,把最坏情况的展开规模限制到解析后节点数检查能安全拒绝的大小。

use serde_yaml::Value;

const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 10_000;
/// `&anchor` 定义与 `*alias` 引用的合计上限。倍增的别名链每层约用 3 个 token,故 32 把展开
/// 限制到 ~2^10 个节点。合法的 Mihomo 配置很少或不用锚点。
const MAX_ANCHORS_ALIASES: usize = 32;

#[derive(Debug, PartialEq, Eq)]
pub enum YamlError {
    Parse,
    TooComplex,
    NotMapping,
}

/// 把 `text` 解析为 YAML 值:解析前强制锚点/别名上限,解析后限制深度/节点数。
pub fn parse_limited(text: &str) -> Result<Value, YamlError> {
    if count_anchors_aliases(text) > MAX_ANCHORS_ALIASES {
        return Err(YamlError::TooComplex);
    }
    let value: Value = serde_yaml::from_str(text).map_err(|_| YamlError::Parse)?;
    let mut nodes = 0usize;
    check(&value, 1, &mut nodes)?;
    Ok(value)
}

/// 统计原始文本中的 YAML 锚点(`&name`)与别名(`*name`)token。为避免把标量内部的 `&`/`*`
/// 也算进去,只统计前面是空白或流式指示符、后面是名字字符的符号。
fn count_anchors_aliases(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut count = 0;
    for i in 0..bytes.len() {
        let c = bytes[i];
        if c != b'&' && c != b'*' {
            continue;
        }
        let prev_ok = i == 0
            || matches!(
                bytes[i - 1],
                b' ' | b'\t' | b'\n' | b'\r' | b'[' | b'{' | b','
            );
        let next_ok = bytes
            .get(i + 1)
            .is_some_and(|n| n.is_ascii_alphanumeric() || *n == b'_' || *n == b'-');
        if prev_ok && next_ok {
            count += 1;
        }
    }
    count
}

/// 解析 `text` 并要求顶层是一个映射(如单个 Mihomo proxy 定义)。
pub fn parse_mapping(text: &str) -> Result<serde_yaml::Mapping, YamlError> {
    match parse_limited(text)? {
        Value::Mapping(map) => Ok(map),
        _ => Err(YamlError::NotMapping),
    }
}

fn check(value: &Value, depth: usize, nodes: &mut usize) -> Result<(), YamlError> {
    if depth > MAX_DEPTH {
        return Err(YamlError::TooComplex);
    }
    *nodes += 1;
    if *nodes > MAX_NODES {
        return Err(YamlError::TooComplex);
    }
    match value {
        Value::Sequence(seq) => {
            for item in seq {
                check(item, depth + 1, nodes)?;
            }
        }
        Value::Mapping(map) => {
            for (_, v) in map {
                check(v, depth + 1, nodes)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_simple_proxy_mapping() {
        let yaml = "name: my-ss\ntype: ss\nserver: 1.2.3.4\nport: 8388";
        assert!(parse_mapping(yaml).is_ok());
    }

    #[test]
    fn rejects_non_mapping_top_level() {
        assert_eq!(parse_mapping("- a\n- b"), Err(YamlError::NotMapping));
    }

    #[test]
    fn rejects_invalid_yaml() {
        assert_eq!(parse_limited(":\n  - ["), Err(YamlError::Parse));
    }

    #[test]
    fn allows_light_anchor_use() {
        let yaml = "defaults: &d { type: ss, port: 8388 }\nnode: { name: a, <<: *d }";
        assert!(parse_limited(yaml).is_ok());
    }

    #[test]
    fn rejects_billion_laughs_before_parsing() {
        // 一个会指数膨胀的小输入;在 serde_yaml 能将其物化之前就被锚点/别名上限拒绝。
        let mut yaml = String::from("a: &a [x, x, x, x, x, x, x, x, x, x]\n");
        for (level, prev) in [('b', 'a'), ('c', 'b'), ('d', 'c'), ('e', 'd')] {
            yaml.push_str(&format!(
                "{level}: &{level} [*{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}]\n"
            ));
        }
        assert_eq!(parse_limited(&yaml), Err(YamlError::TooComplex));
    }
}
