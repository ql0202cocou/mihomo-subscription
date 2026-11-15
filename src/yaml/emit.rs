//! 把 `Value` 写成 YAML 文本:直接驱动 libyaml 发射器,自行决定每个字符串是否加引号。
//!
//! serde_norway 的序列化器只按 YAML 1.2 判断字符串是否需要引号,且不提供强制加引号的入口。
//! Mihomo 的 YAML 库(go-yaml v3)在此之外还会把一部分无引号标量读成数字或时间(见
//! [`mihomo_retypes`]),而 `proxies`/`proxy-groups` 的值按任意类型解码:节点名、分组名、密码
//! 一旦被读成别的类型,整份配置拒绝加载或值被悄悄改写。这里的输出除了给这些字符串加引号外,
//! 与 serde_norway 逐字节一致(由测试里的差分对照保证)。

use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::ptr;

use serde_yaml::Value;
use unsafe_libyaml_norway as unsafe_yaml;

use super::{YamlError, MAX_DEPTH};

/// 序列化 `value`,输出超过 `max_bytes` 时报 [`YamlError::TooComplex`]。
pub(super) fn emit(value: &Value, max_bytes: usize) -> Result<String, YamlError> {
    let mut emitter = Emitter::new(max_bytes)?;
    emitter.event(|event| {
        // SAFETY: `event` points to writable storage for one event.
        unsafe {
            unsafe_yaml::yaml_stream_start_event_initialize(event, unsafe_yaml::YAML_UTF8_ENCODING)
        }
        .ok
    })?;
    emitter.event(|event| {
        // SAFETY: as above; null directives and an implicit document start are valid.
        unsafe {
            unsafe_yaml::yaml_document_start_event_initialize(
                event,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                true,
            )
        }
        .ok
    })?;
    emitter.node(value, None, 1)?;
    emitter.event(|event| {
        // SAFETY: `event` points to writable storage for one event.
        unsafe { unsafe_yaml::yaml_document_end_event_initialize(event, true) }.ok
    })?;
    emitter.event(|event| {
        // SAFETY: as above. The stream end flushes the remaining output.
        unsafe { unsafe_yaml::yaml_stream_end_event_initialize(event) }.ok
    })?;
    emitter.finish()
}

/// 发射器的输出缓冲,带字节上限。
struct Sink {
    bytes: Vec<u8>,
    max_bytes: usize,
    exceeded: bool,
}

/// libyaml 的输出回调:把一段输出追加到 [`Sink`];超过上限时返回 0,发射随之失败。
unsafe fn write_handler(data: *mut c_void, buffer: *mut u8, size: u64) -> i32 {
    // SAFETY: `data` is the `Sink` registered in `Emitter::new`, alive until the emitter is
    // dropped and reached only through this pointer while the emitter exists.
    let sink = unsafe { &mut *data.cast::<Sink>() };
    let Ok(size) = usize::try_from(size) else {
        sink.exceeded = true;
        return 0;
    };
    if size > sink.max_bytes - sink.bytes.len() {
        sink.exceeded = true;
        return 0;
    }
    // SAFETY: libyaml hands over `size` initialized bytes at `buffer`.
    sink.bytes
        .extend_from_slice(unsafe { std::slice::from_raw_parts(buffer, size) });
    1
}

struct Emitter {
    inner: Box<unsafe_yaml::yaml_emitter_t>,
    /// 由 `Box::into_raw` 得到;发射器存续期间只经此指针访问,`Drop` 时收回。
    sink: *mut Sink,
}

impl Emitter {
    fn new(max_bytes: usize) -> Result<Self, YamlError> {
        let mut inner = Box::<unsafe_yaml::yaml_emitter_t>::new_uninit();
        let ptr = inner.as_mut_ptr();
        // SAFETY: `ptr` is valid for writes; a failed initialization leaves nothing to release.
        if unsafe { unsafe_yaml::yaml_emitter_initialize(ptr) }.fail {
            return Err(YamlError::Parse);
        }
        let sink = Box::into_raw(Box::new(Sink {
            bytes: Vec::new(),
            max_bytes,
            exceeded: false,
        }));
        // SAFETY: the emitter was initialized above. The boxed emitter and the sink keep stable
        // addresses for as long as libyaml holds pointers to them. The settings match
        // serde_norway's emitter: unescaped Unicode and no line folding.
        let inner = unsafe {
            unsafe_yaml::yaml_emitter_set_unicode(ptr, true);
            unsafe_yaml::yaml_emitter_set_width(ptr, -1);
            unsafe_yaml::yaml_emitter_set_output(ptr, write_handler, sink.cast());
            inner.assume_init()
        };
        Ok(Self { inner, sink })
    }

    /// 初始化并发射一个事件。`init` 用 libyaml 的 `*_event_initialize` 填充事件,返回是否成功。
    fn event(
        &mut self,
        init: impl FnOnce(*mut unsafe_yaml::yaml_event_t) -> bool,
    ) -> Result<(), YamlError> {
        let mut event = MaybeUninit::<unsafe_yaml::yaml_event_t>::uninit();
        if !init(event.as_mut_ptr()) {
            return Err(self.error());
        }
        // SAFETY: the event was initialized. The emitter takes ownership of its contents and
        // releases them itself, whether or not emitting succeeds.
        if unsafe { unsafe_yaml::yaml_emitter_emit(self.inner.as_mut(), event.as_mut_ptr()) }.fail {
            return Err(self.error());
        }
        Ok(())
    }

    fn error(&self) -> YamlError {
        // SAFETY: `sink` is valid until drop and no callback is running.
        if unsafe { (*self.sink).exceeded } {
            YamlError::TooComplex
        } else {
            YamlError::Parse
        }
    }

    fn finish(self) -> Result<String, YamlError> {
        // SAFETY: `sink` is valid until drop and no callback is running.
        let bytes = std::mem::take(unsafe { &mut (*self.sink).bytes });
        String::from_utf8(bytes).map_err(|_| YamlError::Parse)
    }

    /// 发射一个节点。`tag` 是外层 `Value::Tagged` 带来的 tag(含前导 `!`)。
    fn node(&mut self, value: &Value, tag: Option<&str>, depth: usize) -> Result<(), YamlError> {
        if depth > MAX_DEPTH {
            return Err(YamlError::TooComplex);
        }
        match value {
            Value::Null => self.scalar(tag, "null", unsafe_yaml::YAML_PLAIN_SCALAR_STYLE),
            Value::Bool(true) => self.scalar(tag, "true", unsafe_yaml::YAML_PLAIN_SCALAR_STYLE),
            Value::Bool(false) => self.scalar(tag, "false", unsafe_yaml::YAML_PLAIN_SCALAR_STYLE),
            Value::Number(number) => self.scalar(
                tag,
                &number.to_string(),
                unsafe_yaml::YAML_PLAIN_SCALAR_STYLE,
            ),
            Value::String(text) => self.scalar(tag, text, string_style(text)),
            Value::Sequence(items) => {
                let tag = c_tag(tag);
                self.event(|event| {
                    let tag = tag_ptr(&tag);
                    // SAFETY: `event` is writable; `tag` is null or NUL-terminated and outlives
                    // the call, which copies it.
                    unsafe {
                        unsafe_yaml::yaml_sequence_start_event_initialize(
                            event,
                            ptr::null(),
                            tag,
                            tag.is_null(),
                            unsafe_yaml::YAML_ANY_SEQUENCE_STYLE,
                        )
                    }
                    .ok
                })?;
                for item in items {
                    self.node(item, None, depth + 1)?;
                }
                self.event(|event| {
                    // SAFETY: `event` points to writable storage for one event.
                    unsafe { unsafe_yaml::yaml_sequence_end_event_initialize(event) }.ok
                })
            }
            Value::Mapping(map) => {
                let tag = c_tag(tag);
                self.event(|event| {
                    let tag = tag_ptr(&tag);
                    // SAFETY: as for the sequence start.
                    unsafe {
                        unsafe_yaml::yaml_mapping_start_event_initialize(
                            event,
                            ptr::null(),
                            tag,
                            tag.is_null(),
                            unsafe_yaml::YAML_ANY_MAPPING_STYLE,
                        )
                    }
                    .ok
                })?;
                for (key, value) in map {
                    self.node(key, None, depth + 1)?;
                    self.node(value, None, depth + 1)?;
                }
                self.event(|event| {
                    // SAFETY: `event` points to writable storage for one event.
                    unsafe { unsafe_yaml::yaml_mapping_end_event_initialize(event) }.ok
                })
            }
            Value::Tagged(tagged) => {
                // 一个节点只能有一个 tag。
                if tag.is_some() {
                    return Err(YamlError::Parse);
                }
                self.node(&tagged.value, Some(&tagged.tag.to_string()), depth + 1)
            }
        }
    }

    fn scalar(
        &mut self,
        tag: Option<&str>,
        value: &str,
        style: unsafe_yaml::yaml_scalar_style_t,
    ) -> Result<(), YamlError> {
        let length = i32::try_from(value.len()).map_err(|_| YamlError::TooComplex)?;
        let tag = c_tag(tag);
        self.event(|event| {
            let tag = tag_ptr(&tag);
            // SAFETY: `event` is writable; `value` is valid for `length` bytes and `tag` is null
            // or NUL-terminated; both outlive the call, which copies them.
            unsafe {
                unsafe_yaml::yaml_scalar_event_initialize(
                    event,
                    ptr::null(),
                    tag,
                    value.as_ptr(),
                    length,
                    tag.is_null(),
                    tag.is_null(),
                    style,
                )
            }
            .ok
        })
    }
}

impl Drop for Emitter {
    fn drop(&mut self) {
        // SAFETY: the emitter was initialized and nothing borrows it; `sink` came from
        // `Box::into_raw` and is released exactly once, after the emitter that points to it.
        unsafe {
            unsafe_yaml::yaml_emitter_delete(self.inner.as_mut());
            drop(Box::from_raw(self.sink));
        }
    }
}

/// tag 的 NUL 结尾副本(libyaml 按 C 字符串读取)。
fn c_tag(tag: Option<&str>) -> Option<Vec<u8>> {
    tag.map(|tag| {
        let mut bytes = Vec::with_capacity(tag.len() + 1);
        bytes.extend_from_slice(tag.as_bytes());
        bytes.push(0);
        bytes
    })
}

fn tag_ptr(tag: &Option<Vec<u8>>) -> *const u8 {
    tag.as_ref().map_or(ptr::null(), |tag| tag.as_ptr())
}

/// 字符串的标量样式:含换行用字面块;会被读成别的类型的加单引号;其余交给 libyaml 自行决定
/// (能无引号就无引号,含特殊字符时它会自己加引号)。
fn string_style(text: &str) -> unsafe_yaml::yaml_scalar_style_t {
    if text.contains('\n') {
        unsafe_yaml::YAML_LITERAL_SCALAR_STYLE
    } else if resolves_as_non_string(text) || mihomo_retypes(text) {
        unsafe_yaml::YAML_SINGLE_QUOTED_SCALAR_STYLE
    } else {
        unsafe_yaml::YAML_ANY_SCALAR_STYLE
    }
}

/// Mihomo 的 YAML 库(go-yaml v3)会把这个无引号标量读成数字或时间,而 [`resolves_as_non_string`]
/// 认为它是字符串。go-yaml 只对以数字、正负号或小数点开头的标量尝试数字与时间,且比 YAML 1.2
/// 多认三种写法:数字里的下划线(`1_000`)、大写进制前缀(`0X1F`)和日期时间(`2026-12-31`)。
fn mihomo_retypes(text: &str) -> bool {
    let bytes = text.as_bytes();
    if !matches!(bytes.first(), Some(b'0'..=b'9' | b'+' | b'-' | b'.')) {
        return false;
    }
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    text.contains('_')
        || matches!(
            unsigned.as_bytes(),
            [b'0', b'x' | b'X' | b'o' | b'O' | b'b' | b'B', ..]
        )
        || matches!(bytes, [y1, y2, y3, y4, b'-', ..]
            if [y1, y2, y3, y4].iter().all(|b| b.is_ascii_digit()))
}

/// 无引号时 serde_norway 是否会把这段文本读成空值、布尔或数字(与它为字符串选择引号的判定
/// 一致):这类字符串必须加引号才能原样读回。
fn resolves_as_non_string(text: &str) -> bool {
    text.is_empty()
        || matches!(text, "null" | "Null" | "NULL" | "~")
        || matches!(text, "true" | "True" | "TRUE" | "false" | "False" | "FALSE")
        || is_unsigned_int(text)
        || is_negative_int(text)
        || digits_but_not_number(text)
        || is_float(text)
}

/// 前导零后全是数字(`007`):YAML 1.2 里是字符串,但多数解析器当数字,一律加引号。
fn digits_but_not_number(text: &str) -> bool {
    let text = text.strip_prefix(['-', '+']).unwrap_or(text);
    text.len() > 1 && text.starts_with('0') && text[1..].bytes().all(|b| b.is_ascii_digit())
}

fn is_unsigned_int(text: &str) -> bool {
    let unpositive = text.strip_prefix('+').unwrap_or(text);
    for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
        if let Some(rest) = unpositive.strip_prefix(prefix) {
            if rest.starts_with(['+', '-']) {
                return false;
            }
            if u128::from_str_radix(rest, radix).is_ok() {
                return true;
            }
        }
    }
    if unpositive.starts_with(['+', '-']) || digits_but_not_number(text) {
        return false;
    }
    unpositive.parse::<u128>().is_ok()
}

fn is_negative_int(text: &str) -> bool {
    for (prefix, radix) in [("-0x", 16), ("-0o", 8), ("-0b", 2)] {
        if let Some(rest) = text.strip_prefix(prefix) {
            if i128::from_str_radix(&format!("-{rest}"), radix).is_ok() {
                return true;
            }
        }
    }
    !digits_but_not_number(text) && text.parse::<i128>().is_ok()
}

fn is_float(text: &str) -> bool {
    let unpositive = match text.strip_prefix('+') {
        Some(rest) if rest.starts_with(['+', '-']) => return false,
        Some(rest) => rest,
        None => text,
    };
    matches!(unpositive, ".inf" | ".Inf" | ".INF")
        || matches!(text, "-.inf" | "-.Inf" | "-.INF" | ".nan" | ".NaN" | ".NAN")
        || unpositive.parse::<f64>().is_ok_and(f64::is_finite)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_yaml::value::{Tag, TaggedValue};
    use serde_yaml::Mapping;

    const LIMIT: usize = 16 * 1024 * 1024;

    fn tagged(tag: &str, value: Value) -> Value {
        Value::Tagged(Box::new(TaggedValue {
            tag: Tag::new(tag),
            value,
        }))
    }

    /// 字符串在序列里、作映射值、作映射键三种位置各放一份。
    fn placements(strings: &[String]) -> Vec<Value> {
        let values: Vec<Value> = strings.iter().map(|s| Value::from(s.as_str())).collect();
        let mut as_values = Mapping::new();
        let mut as_keys = Mapping::new();
        for (index, value) in values.iter().enumerate() {
            as_values.insert(Value::from(format!("k{index}")), value.clone());
            as_keys.insert(value.clone(), Value::from(index as u64));
        }
        vec![
            Value::Sequence(values),
            Value::Mapping(as_values),
            Value::Mapping(as_keys),
        ]
    }

    /// 由数字、符号、进制与时间相关字符组成的全部 1 到 3 字符串。
    fn number_like_strings() -> Vec<String> {
        let alphabet: Vec<char> = "0123789_+-.eExXoObBaf:TZ ".chars().collect();
        let mut all = Vec::new();
        let mut layer = vec![String::new()];
        for _ in 0..3 {
            layer = layer
                .iter()
                .flat_map(|prefix| alphabet.iter().map(move |c| format!("{prefix}{c}")))
                .collect();
            all.extend(layer.iter().cloned());
        }
        all
    }

    fn special_strings() -> Vec<String> {
        let mut strings: Vec<String> = [
            "",
            " ",
            "a: b",
            "- a",
            "#c",
            "a #b",
            "'q'",
            "\"dq\"",
            "tab\tx",
            "multi\nline",
            "trail ",
            " lead",
            "é中文节点",
            "!tag",
            "&a",
            "*a",
            "?",
            "? x",
            "@",
            "`",
            "%",
            "|",
            ">",
            "{a}",
            "[a]",
            ",",
            "a,b",
            "~",
            "null",
            "Null",
            "true",
            "FALSE",
            "key: 'v'",
            "\u{1}",
            "\u{7f}",
            "🚀 香港 01",
            "\r",
            "a\r\nb",
            "x\n",
            "\nx",
            "  \n",
            "a\n\nb\n",
            ":",
            "a:",
            ":a",
            "-",
            "--",
            "---",
            "...",
            "a\u{a0}b",
            "\u{feff}x",
            "it's",
            "say \"hi\"",
            "back\\slash",
            "DOMAIN-SUFFIX,example.com,Proxy",
            "AND,((DOMAIN,a.com),(NETWORK,udp)),DIRECT",
            "https://example.com/a?b=c#d",
            "=",
            "yes",
            "No",
            "on",
            "OFF",
            "007",
            "-007",
            "+08",
            "1.2.3.4",
            "8388",
            "-1",
            "+1",
            "1e3",
            "1.",
            ".5",
            "+.5",
            ".inf",
            "-.INF",
            ".NaN",
            "0x1f",
            "-0x1f",
            "0o17",
            "0b101",
            "123456789012345678901234567890",
            "-123456789012345678901234567890",
            "1e999",
            "inf",
            "nan",
            "infinity",
            "0f2c1a3e-1111-2222-3333-444455556666",
            "0123abcd",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        strings.push("x".repeat(5000));
        strings
    }

    #[test]
    fn output_matches_serde_except_for_strings_mihomo_would_retype() {
        let mut cases: Vec<Value> = Vec::new();
        cases.extend(placements(&special_strings()));
        let (retyped, plain): (Vec<String>, Vec<String>) = number_like_strings()
            .into_iter()
            .partition(|s| mihomo_retypes(s));
        assert!(!retyped.is_empty() && !plain.is_empty());
        cases.extend(placements(&plain));
        for text in [
            "[0, -1, 8388, 18446744073709551615, -9223372036854775808, 1.0, 1e30, -0.0, 0.1, \
             1.5e-7, .nan, .inf, -.inf, true, false, null, ~]",
            "{a: {}, b: [], c: [[], {}], d: {e: {f: [1, {g: []}]}}}",
            "{}",
            "[]",
            "port: 7890\ndns: {enable: true, nameserver: [1.1.1.1, 'tls://8.8.8.8']}\nproxies:\n  \
             - {name: 香港 01, type: vless, server: 1.2.3.4, port: 443, \
             uuid: 0f2c1a3e-1111-2222-3333-444455556666, tls: true, \
             reality-opts: {public-key: pk, short-id: '0123abcd'}, \
             ws-opts: {path: /ws, headers: {Host: a.example}}}\nproxy-groups:\n  \
             - {name: Proxy, type: select, proxies: [香港 01, DIRECT]}\nrules:\n  \
             - DOMAIN-SUFFIX,example.com,Proxy\n  - MATCH,DIRECT\n",
        ] {
            cases.push(serde_yaml::from_str(text).unwrap());
        }
        cases.push(Value::Sequence(vec![
            tagged("x", Value::from("v")),
            tagged("!y", Value::from("true")),
            tagged("seq", Value::Sequence(vec![Value::from(1u64)])),
            tagged("map", serde_yaml::from_str("{a: 1}").unwrap()),
            tagged("empty", Value::Sequence(vec![])),
        ]));

        for (index, case) in cases.iter().enumerate() {
            let expected = serde_yaml::to_string(case).unwrap();
            let actual = emit(case, LIMIT).unwrap();
            assert!(actual == expected, "case {index} differs from serde output");
        }

        // 会被 Mihomo 读成别的类型的字符串:一律带单引号,且仍能原样读回。
        for case in placements(&retyped) {
            let text = emit(&case, LIMIT).unwrap();
            let back: Value = serde_yaml::from_str(&text).unwrap();
            assert_eq!(back, case);
        }
        for text in &retyped {
            let line = emit(&Value::Sequence(vec![Value::from(text.as_str())]), LIMIT).unwrap();
            assert_eq!(line, format!("- '{text}'\n"));
        }
    }

    #[test]
    fn strings_mihomo_reads_as_numbers_or_timestamps_are_quoted() {
        let doc: Value = serde_yaml::from_str(
            "proxies:\n  - {name: '2026-12-31', type: ss, password: '1_000', sni: '0X1F'}\n\
             proxy-groups:\n  - {name: '2001-12-14T21:59:43Z', proxies: ['2026-12-31', DIRECT]}\n",
        )
        .unwrap();
        assert_eq!(
            emit(&doc, LIMIT).unwrap(),
            "proxies:\n- name: '2026-12-31'\n  type: ss\n  password: '1_000'\n  sni: '0X1F'\n\
             proxy-groups:\n- name: '2001-12-14T21:59:43Z'\n  proxies:\n  - '2026-12-31'\n  - DIRECT\n"
        );

        for text in [
            "1_000",
            "+1_0",
            "-1_0",
            ".0_1",
            "0x1_f",
            "0X1F",
            "-0O17",
            "0B01",
            "0b+1",
            "1e1_0",
            "2026-12-31",
            "2001-1-2",
            "2001-12-14 21:59:43",
            "2001-12-14t21:59:43.10-05:00",
        ] {
            assert!(mihomo_retypes(text), "{text}");
        }
        // 普通的地址、标识与规则行不受影响。
        for text in [
            "1.2.3.4",
            "0f2c1a3e-1111-2222-3333-444455556666",
            "-",
            "+",
            "8.8.8.8:53",
            "20261231",
            "202-12-31",
            "a_1",
            "DOMAIN,1_0.example,DIRECT",
            "香港 01",
        ] {
            assert!(!mihomo_retypes(text), "{text}");
        }
    }

    #[test]
    fn output_and_depth_limits_are_enforced() {
        let value = Value::Sequence(vec![Value::from("x".repeat(1000)); 4]);
        let text = emit(&value, LIMIT).unwrap();
        assert_eq!(emit(&value, text.len()).as_deref(), Ok(text.as_str()));
        assert_eq!(emit(&value, text.len() - 1), Err(YamlError::TooComplex));

        let mut nested = Value::Sequence(vec![]);
        for _ in 1..MAX_DEPTH {
            nested = Value::Sequence(vec![nested]);
        }
        assert!(emit(&nested, LIMIT).is_ok());
        assert_eq!(
            emit(&Value::Sequence(vec![nested]), LIMIT),
            Err(YamlError::TooComplex)
        );
    }

    #[test]
    fn nested_tags_are_rejected() {
        let nested = tagged("a", tagged("b", Value::from("v")));
        assert_eq!(emit(&nested, LIMIT), Err(YamlError::Parse));
    }
}
