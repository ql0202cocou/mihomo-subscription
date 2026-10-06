//! 在构建 Value 的过程中限制不受信任 YAML,包括别名展开、映射键与 tag 的内部值。

use std::fmt;

use serde::de::{self, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor};
use serde_yaml::{
    value::{Tag, TaggedValue},
    Mapping, Value,
};

const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 10_000;
const MAX_BYTES: usize = 8 * 1024 * 1024;
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
    if text.len() > MAX_BYTES {
        return Err(YamlError::TooComplex);
    }
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
    result.map_err(|_| {
        if budget.exceeded {
            YamlError::TooComplex
        } else {
            YamlError::Parse
        }
    })
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

    fn visit_seq<A: SeqAccess<'de>>(self, mut data: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(value) = data.next_element_seed(Limited {
            budget: self.budget,
            depth: self.depth + 1,
        })? {
            items.push(value);
        }
        Ok(Value::Sequence(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut data: A) -> Result<Value, A::Error> {
        let mut map = Mapping::new();
        while let Some(key) = data.next_key_seed(Limited {
            budget: self.budget,
            depth: self.depth + 1,
        })? {
            let value = data.next_value_seed(Limited {
                budget: self.budget,
                depth: self.depth + 1,
            })?;
            if map.insert(key, value).is_some() {
                return Err(de::Error::custom("duplicate YAML mapping key"));
            }
        }
        Ok(Value::Mapping(map))
    }

    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> Result<Value, A::Error> {
        // tag 名也走同一个字节预算,tag 包裹的值走同一个深度/节点预算。
        let (tag, contents) = data.variant_seed(Limited {
            budget: self.budget,
            depth: self.depth + 1,
        })?;
        let Value::String(tag) = tag else {
            return Err(de::Error::custom("invalid YAML tag"));
        };
        let value = contents.newtype_variant_seed(Limited {
            budget: self.budget,
            depth: self.depth + 1,
        })?;
        Ok(Value::Tagged(Box::new(TaggedValue {
            tag: Tag::new(tag),
            value,
        })))
    }
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
            if buf.len() > 16 * 1024 * 1024 - self.bytes.len() {
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
        assert_eq!(parse_limited("---\na: 1\n---\nb: 2"), Err(YamlError::Parse));
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
