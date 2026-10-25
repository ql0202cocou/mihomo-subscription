//! 在构建 Value 的过程中限制不受信任 YAML,包括别名展开、映射键与 tag 的内部值。

mod preflight;

use std::fmt;

use serde::de::{self, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor};
use serde_yaml::{
    value::{Tag, TaggedValue},
    Mapping, Value,
};

const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 10_000;
const MAX_INPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_GENERATED_BYTES: usize = 16 * 1024 * 1024;
const MAX_BYTES: usize = MAX_INPUT_BYTES;
const MAX_EXPANSION: usize = 8;

#[derive(Debug, PartialEq, Eq)]
pub enum YamlError {
    Parse,
    TooComplex,
    NotMapping,
}

struct Budget {
    nodes: usize,
    bytes: usize,
    exceeded: bool,
}

impl Budget {
    fn charge<E: de::Error>(&mut self, depth: usize, bytes: usize) -> Result<(), E> {
        if depth > MAX_DEPTH || self.nodes == 0 || bytes > self.bytes {
            self.exceeded = true;
            return Err(E::custom("YAML resource limit exceeded"));
        }
        self.nodes -= 1;
        self.bytes -= bytes;
        Ok(())
    }
}

/// 原文最多 8 MiB;展开后的标量总字节数最多 8 MiB 且不超过原文的 8 倍(最小预算 1 KiB)。
/// 节点/深度/字节预算在每个值物化之前扣除,而不是先展开整个 Value 再检查。
pub fn parse_limited(text: &str) -> Result<Value, YamlError> {
    parse_with_input_limit(text, MAX_INPUT_BYTES)
}

/// 自生成 YAML 原文最多 16 MiB,物化预算与外部输入相同。
pub fn parse_generated_limited(text: &str) -> Result<Value, YamlError> {
    parse_with_input_limit(text, MAX_GENERATED_BYTES)
}

fn parse_with_input_limit(text: &str, max_input_bytes: usize) -> Result<Value, YamlError> {
    if text.len() > max_input_bytes {
        return Err(YamlError::TooComplex);
    }
    preflight::check(text)?;
    let mut budget = Budget {
        nodes: MAX_NODES,
        bytes: MAX_BYTES.min(text.len().saturating_mul(MAX_EXPANSION).max(1024)),
        exceeded: false,
    };
    let result = Limited {
        budget: &mut budget,
        depth: 1,
    }
    .deserialize(serde_yaml::Deserializer::from_str(text));
    let value = result.map_err(|_| {
        if budget.exceeded {
            YamlError::TooComplex
        } else {
            YamlError::Parse
        }
    })?;
    normalize(value)
}

pub fn parse_mapping(text: &str) -> Result<Mapping, YamlError> {
    match parse_limited(text)? {
        Value::Mapping(map) => Ok(map),
        _ => Err(YamlError::NotMapping),
    }
}

struct Limited<'a> {
    budget: &'a mut Budget,
    depth: usize,
}

impl Limited<'_> {
    /// 下一层的子值:共享同一预算,深度加一。
    fn child(&mut self) -> Limited<'_> {
        Limited {
            budget: self.budget,
            depth: self.depth + 1,
        }
    }
}

impl<'de> DeserializeSeed<'de> for Limited<'_> {
    type Value = Value;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        self.budget.charge::<D::Error>(self.depth, 0)?;
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Limited<'_> {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a YAML value within resource limits")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }
    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }
    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_none<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        if value.len() > self.budget.bytes {
            self.budget.exceeded = true;
            return Err(E::custom("YAML scalar byte limit exceeded"));
        }
        self.budget.bytes -= value.len();
        Ok(Value::String(value.to_owned()))
    }

    fn visit_seq<A: SeqAccess<'de>>(mut self, mut data: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(value) = data.next_element_seed(self.child())? {
            items.push(value);
        }
        Ok(Value::Sequence(items))
    }

    fn visit_map<A: MapAccess<'de>>(mut self, mut data: A) -> Result<Value, A::Error> {
        let mut map = Mapping::new();
        while let Some(key) = data.next_key_seed(self.child())? {
            let value = data.next_value_seed(self.child())?;
            if map.insert(key, value).is_some() {
                return Err(de::Error::custom("duplicate YAML mapping key"));
            }
        }
        Ok(Value::Mapping(map))
    }

    fn visit_enum<A: EnumAccess<'de>>(mut self, data: A) -> Result<Value, A::Error> {
        // tag 名也走同一个字节预算,tag 包裹的值走同一个深度/节点预算。
        let (tag, contents) = data.variant_seed(self.child())?;
        let Value::String(tag) = tag else {
            return Err(de::Error::custom("invalid YAML tag"));
        };
        let value = contents.newtype_variant_seed(self.child())?;
        Ok(Value::Tagged(Box::new(TaggedValue {
            tag: Tag::new(tag),
            value,
        })))
    }
}

fn normalize(value: Value) -> Result<Value, YamlError> {
    let mut steps = MAX_NODES;
    let value = normalize_value(value, &mut steps)?;
    check_value(&value)?;
    Ok(value)
}

fn normalize_value(value: Value, steps: &mut usize) -> Result<Value, YamlError> {
    take(steps, 1)?;
    match value {
        Value::Sequence(items) => {
            let items = items
                .into_iter()
                .map(|item| normalize_value(item, steps))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Sequence(items))
        }
        Value::Mapping(map) => normalize_mapping(map, steps).map(Value::Mapping),
        Value::Tagged(tagged) => {
            let TaggedValue { tag, value } = *tagged;
            let value = normalize_value(value, steps)?;
            Ok(Value::Tagged(Box::new(TaggedValue { tag, value })))
        }
        _ => Ok(value),
    }
}

fn normalize_mapping(map: Mapping, steps: &mut usize) -> Result<Mapping, YamlError> {
    let mut merge = None;
    let mut normalized = Mapping::new();
    for (key, value) in map {
        if matches!(&key, Value::String(key) if key == "<<") {
            take(steps, 1)?;
            merge = Some(value);
            continue;
        }
        let key = normalize_value(key, steps)?;
        let value = normalize_value(value, steps)?;
        if normalized.insert(key, value).is_some() {
            return Err(YamlError::Parse);
        }
    }
    if let Some(source) = merge {
        let source = normalize_value(source, steps)?;
        merge_source(&mut normalized, source, steps)?;
    }
    Ok(normalized)
}

fn merge_source(target: &mut Mapping, source: Value, steps: &mut usize) -> Result<(), YamlError> {
    match source {
        Value::Mapping(map) => merge_mapping(target, map, steps),
        Value::Sequence(sources) => {
            for source in sources {
                let Value::Mapping(map) = source else {
                    return Err(YamlError::Parse);
                };
                merge_mapping(target, map, steps)?;
            }
            Ok(())
        }
        _ => Err(YamlError::Parse),
    }
}

fn merge_mapping(target: &mut Mapping, map: Mapping, steps: &mut usize) -> Result<(), YamlError> {
    for (key, value) in map {
        take(steps, 1)?;
        target.entry(key).or_insert(value);
    }
    Ok(())
}

/// 从预算中扣除 `amount`;不足时报 [`YamlError::TooComplex`]。预检的事件/字节计数与 merge 展开步数共用。
fn take(budget: &mut usize, amount: usize) -> Result<(), YamlError> {
    *budget = budget.checked_sub(amount).ok_or(YamlError::TooComplex)?;
    Ok(())
}

/// 对转换后合并的 Value 应用同等预算,限制多个管理员节点/分组合并后的总规模。
pub fn check_value(value: &Value) -> Result<(), YamlError> {
    fn check(value: &Value, depth: usize, budget: &mut Budget) -> Result<(), serde_yaml::Error> {
        budget.charge::<serde_yaml::Error>(
            depth,
            match value {
                Value::String(s) => s.len(),
                _ => 0,
            },
        )?;
        match value {
            Value::Sequence(items) => {
                for item in items {
                    check(item, depth + 1, budget)?;
                }
            }
            Value::Mapping(map) => {
                for (key, value) in map {
                    check(key, depth + 1, budget)?;
                    check(value, depth + 1, budget)?;
                }
            }
            Value::Tagged(tagged) => {
                budget.charge::<serde_yaml::Error>(depth + 1, tagged.tag.to_string().len())?;
                check(&tagged.value, depth + 1, budget)?;
            }
            _ => {}
        }
        Ok(())
    }
    check(
        value,
        1,
        &mut Budget {
            nodes: MAX_NODES,
            bytes: MAX_BYTES,
            exceeded: false,
        },
    )
    .map_err(|_| YamlError::TooComplex)
}

/// 序列化时限制输出字节数,避免转义/格式化把小 Value 放大成巨大字符串。
pub fn serialize_limited(value: &Value) -> Result<String, YamlError> {
    use std::io::{self, Write};
    struct LimitedWriter {
        bytes: Vec<u8>,
        exceeded: bool,
    }
    impl Write for LimitedWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if buf.len() > MAX_GENERATED_BYTES - self.bytes.len() {
                self.exceeded = true;
                return Err(io::Error::other("YAML output byte limit exceeded"));
            }
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut writer = LimitedWriter {
        bytes: Vec::new(),
        exceeded: false,
    };
    serde_yaml::to_writer(&mut writer, value).map_err(|_| {
        if writer.exceeded {
            YamlError::TooComplex
        } else {
            YamlError::Parse
        }
    })?;
    String::from_utf8(writer.bytes).map_err(|_| YamlError::Parse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_simple_proxy_mapping() {
        assert!(parse_mapping("name: my-ss\ntype: ss\nserver: 1.2.3.4\nport: 8388").is_ok());
    }
    #[test]
    fn rejects_non_mapping_top_level() {
        assert_eq!(parse_mapping("- a\n- b"), Err(YamlError::NotMapping));
    }
    #[test]
    fn rejects_invalid_yaml() {
        assert_eq!(parse_limited(":\n  - ["), Err(YamlError::Parse));
        assert_eq!(parse_limited("a: 1\na: 2"), Err(YamlError::Parse));
    }
    #[test]
    fn rejects_a_second_document_during_preflight() {
        assert_eq!(
            parse_limited("---\na: 1\n---\nb: 2"),
            Err(YamlError::TooComplex)
        );
    }
    #[test]
    fn loader_event_budget_includes_collection_end_events() {
        let accepted = "- {}\n".repeat(MAX_NODES - 1);
        assert!(parse_limited(&accepted).is_ok());
        let rejected = format!("{accepted}- {{}}\n");
        assert_eq!(parse_limited(&rejected), Err(YamlError::TooComplex));
    }
    #[test]
    fn raw_depth_budget_matches_materialized_depth() {
        let empty = format!("{}{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        assert!(parse_limited(&empty).is_ok());
        let scalar = format!("{}x{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        assert_eq!(parse_limited(&scalar), Err(YamlError::TooComplex));
        let deep = format!("{}{}", "[".repeat(MAX_DEPTH + 1), "]".repeat(MAX_DEPTH + 1));
        assert_eq!(parse_limited(&deep), Err(YamlError::TooComplex));
    }
    #[test]
    fn rejects_excess_tag_and_anchor_bytes_during_preflight() {
        let tag = format!("!{} value", "t".repeat(MAX_BYTES));
        assert_eq!(preflight::check(&tag), Err(YamlError::TooComplex));
        let anchor = format!("&{} value", "a".repeat(MAX_BYTES));
        assert_eq!(preflight::check(&anchor), Err(YamlError::TooComplex));
    }
    #[test]
    fn allows_light_anchor_use() {
        assert!(
            parse_limited("defaults: &d { type: ss, port: 8388 }\nnode: { name: a, <<: *d }")
                .is_ok()
        );
    }
    #[test]
    fn preserves_small_tags_and_complex_keys() {
        let input = "payload: !opaque [x, y]\n? [a, b]\n: z";
        assert_eq!(
            parse_limited(input).unwrap(),
            serde_yaml::from_str::<Value>(input).unwrap()
        );
    }
    #[test]
    fn merges_mapping_with_explicit_key_priority() {
        let input =
            "defaults: &d { name: base, type: ss, port: 8388 }\nnode: { name: explicit, <<: *d }";
        let value = parse_limited(input).unwrap();
        assert_eq!(value["node"]["name"], "explicit");
        assert_eq!(value["node"]["type"], "ss");
        assert_eq!(value["node"]["port"], 8388);
        assert!(value["node"].get("<<").is_none());
    }
    #[test]
    fn merge_sequence_uses_first_mapping_priority() {
        let input = "first: &f { shared: first, first: 1 }\nsecond: &s { shared: second, second: 2 }\nnode: { <<: [*f, *s] }";
        let value = parse_limited(input).unwrap();
        assert_eq!(value["node"]["shared"], "first");
        assert_eq!(value["node"]["first"], 1);
        assert_eq!(value["node"]["second"], 2);
    }
    #[test]
    fn normalizes_nested_merge_sources_and_values() {
        let input = "node:\n  <<: {<<: {server: example.com}, port: 443}\n  nested:\n    <<: {enabled: true}";
        let value = parse_limited(input).unwrap();
        assert_eq!(value["node"]["server"], "example.com");
        assert_eq!(value["node"]["port"], 443);
        assert_eq!(value["node"]["nested"]["enabled"], true);
        assert!(value["node"].get("<<").is_none());
        assert!(value["node"]["nested"].get("<<").is_none());
    }
    #[test]
    fn rejects_invalid_merge_sources() {
        for input in [
            "node: {<<: scalar}",
            "node: {<<: [{ok: 1}, scalar]}",
            "node: {<<: !opaque {ok: 1}}",
        ] {
            assert_eq!(parse_limited(input), Err(YamlError::Parse));
        }
    }
    #[test]
    fn merge_normalization_obeys_step_budget() {
        let value = serde_yaml::from_str::<Value>("node: {<<: {server: example.com}}").unwrap();
        assert_eq!(normalize_value(value, &mut 4), Err(YamlError::TooComplex));
    }
    #[test]
    fn output_escaping_obeys_byte_budget() {
        // 控制字符序列会被 YAML 转义,最终 Value 在解析字节预算内但输出仍必须有限。
        let value = Value::String("\u{1}".repeat(5 * 1024 * 1024));
        assert!(check_value(&value).is_ok());
        assert!(matches!(
            serialize_limited(&value),
            Err(YamlError::TooComplex)
        ));
    }
    #[test]
    fn generated_yaml_between_input_limits_round_trips() {
        let value = Value::String("\u{1}".repeat(2 * 1024 * 1024));
        let text = serialize_limited(&value).unwrap();
        assert!(text.len() > MAX_INPUT_BYTES);
        assert!(text.len() <= MAX_GENERATED_BYTES);
        assert_eq!(parse_limited(&text), Err(YamlError::TooComplex));
        assert_eq!(parse_generated_limited(&text), Ok(value));
    }
    #[test]
    fn many_small_aliases_are_allowed_within_budget() {
        let input = format!("a: &a x\nb: [{}]", ["*a"; 40].join(","));
        assert!(parse_limited(&input).is_ok());
    }

    #[test]
    fn rejects_billion_laughs_during_expansion() {
        let mut yaml = String::from("a: &a [x, x, x, x, x, x, x, x, x, x]\n");
        for (level, prev) in [('b', 'a'), ('c', 'b'), ('d', 'c'), ('e', 'd')] {
            yaml.push_str(&format!("{level}: &{level} [*{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}]\n"));
        }
        assert_eq!(parse_limited(&yaml), Err(YamlError::TooComplex));
    }
}
