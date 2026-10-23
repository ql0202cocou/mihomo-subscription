//! `mihomo`/`clash` -> `mihomo` 转换。
//!
//! 解析机场 YAML,追加启用的自定义节点/分组,用用户的规则替换 `rules`,并按
//! `docs/architecture.md`「API 设计」处理顶层键(rule-providers 与未知键透传,proxy-providers 剥离)。
//! 校验遵循同一节,返回逐条错误列表供生成弹窗使用。

use serde_yaml::{Mapping, Value};

use crate::yaml;

/// 在规则与分组成员里始终有效的内置策略目标。
const BUILTIN_POLICIES: &[&str] = &[
    "DIRECT",
    "REJECT",
    "REJECT-DROP",
    "PASS",
    "COMPATIBLE",
    "GLOBAL",
];

/// Mihomo 在解析配置前创建的内置名(内置代理 DIRECT/REJECT/REJECT-DROP/PASS/PASS-RULE/
/// COMPATIBLE,另有内置分组 GLOBAL):代理与分组共用同一命名空间,自定义节点占用这些名字会让
/// Mihomo 以 duplicate name 拒绝加载(GLOBAL 节点则会被内置分组静默覆盖)。
/// 见 mihomo `config/config.go` 的 proxies 初始化与分组重名检查。
pub(crate) const RESERVED_NODE_NAMES: &[&str] = &[
    "DIRECT",
    "REJECT",
    "REJECT-DROP",
    "PASS",
    "PASS-RULE",
    "COMPATIBLE",
    "GLOBAL",
];

/// 自定义分组不得占用的内置名:同 [`RESERVED_NODE_NAMES`],但不含 GLOBAL——上游仅在用户未定义
/// 时才内置创建 GLOBAL 分组,自定义 GLOBAL 分组是上游支持的用法。
pub(crate) const RESERVED_GROUP_NAMES: &[&str] = &[
    "DIRECT",
    "REJECT",
    "REJECT-DROP",
    "PASS",
    "PASS-RULE",
    "COMPATIBLE",
];

pub struct CustomNode {
    pub name: String,
    /// 完整的 Mihomo proxy 映射(YAML 文本)。
    pub content: String,
}

pub struct CustomGroup {
    pub name: String,
    pub group_type: String,
    pub members: Vec<String>,
    pub options: Option<serde_json::Value>,
}

/// 一个由面板托管、被本 profile 的 `RULE-SET` 规则引用到的自定义规则集。转换时合并进输出的
/// `rule-providers:` map(同名覆盖机场条目),`url` 指向面板的托管链接。
pub struct RuleProvider {
    pub name: String,
    pub behavior: String,
    pub format: String,
    pub url: String,
}

pub struct ConvertInput<'a> {
    pub provider_yaml: &'a str,
    pub rules: &'a str,
    /// 仅启用的自定义节点。
    pub nodes: Vec<CustomNode>,
    /// 仅启用的自定义分组。
    pub groups: Vec<CustomGroup>,
    /// 自定义块内 **自定义** 节点的手动顺序(自定义节点名)。此处列出的名字优先按序输出;未列出
    /// 的(新增的)保持其默认相对位置在末尾。机场块的内部顺序始终是上游序(用户不可排)。
    pub node_order: Vec<String>,
    /// 输出 `proxies` 中两个节点块的顺序:`"provider"` / `"custom"` 的一个排列。空表示默认
    /// `["provider","custom"]`。
    pub node_section_order: Vec<String>,
    /// proxy-group 的手动顺序(分组名)。此处列出的名字优先按序输出;未列出的保持其默认位置在末尾。
    pub group_order: Vec<String>,
    /// 被本 profile 的 `RULE-SET` 规则引用到的、由面板托管的自定义规则集。合并进输出的
    /// `rule-providers:` map(同名覆盖机场透传条目)。未被引用的规则集不注入。
    pub rule_providers: Vec<RuleProvider>,
    /// 是否为未声明 `support-x25519mlkem768` 的 REALITY 节点补 `true`(per-profile
    /// `profiles.reality_mlkem`,见 [`apply_reality_mlkem`])。
    pub reality_mlkem: bool,
}

#[derive(Debug)]
pub enum ConvertError {
    /// 机场 YAML 无法解析(不是用户配置的校验问题)。
    ProviderParse,
    /// 输出 YAML 序列化失败(内部错误,与机场输入和用户配置均无关)。
    OutputSerialize,
    /// 逐条列举的校验失败,以 `400` 暴露(见 `docs/architecture.md`「API 设计」)。
    Validation(Vec<String>),
}

/// 一次成功转换的产出:输出 YAML 与供生成层告警的附带信息(均可为空)。
#[derive(Debug)]
pub struct ConvertOutput {
    pub yaml: String,
    /// 注入的自定义规则集中、覆盖了机场同名 `rule-providers` 条目的名字。
    pub ruleset_conflicts: Vec<String>,
    /// ML-KEM 已开启但 `client-fingerprint` 缺失或非 `chrome` 的 REALITY 节点名(见
    /// [`apply_reality_mlkem`])。
    pub reality_fingerprint_risks: Vec<String>,
}

/// 把机场 YAML 转换为 Mihomo 配置字符串(见 [`ConvertOutput`])。
pub fn convert(input: ConvertInput) -> Result<ConvertOutput, ConvertError> {
    let mut root =
        yaml::parse_mapping(input.provider_yaml).map_err(|_| ConvertError::ProviderParse)?;

    let provider_proxies = names_in(root.get("proxies"));
    let provider_rule_providers: Vec<String> = match root.get("rule-providers") {
        Some(Value::Mapping(m)) => m
            .keys()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    };

    // 先解析自定义节点 content;把解析失败收集为校验错误,而非直接中止。
    let mut errors: Vec<String> = Vec::new();
    let mut parsed_nodes: Vec<(String, Mapping)> = Vec::new();
    for node in &input.nodes {
        match yaml::parse_mapping(&node.content) {
            Ok(mut m) => {
                // 输出以库内 name 为准:校验、分组成员与规则都按它引用,content 里的 name 可能缺失
                // 或与之不一致(如末尾多了空格)。
                m.insert(Value::from("name"), Value::from(node.name.clone()));
                parsed_nodes.push((node.name.clone(), m));
            }
            Err(_) => errors.push(format!(
                "custom node `{}` has invalid YAML content",
                node.name
            )),
        }
    }

    let custom_node_names: Vec<String> = input.nodes.iter().map(|n| n.name.clone()).collect();
    let custom_group_names: Vec<String> = input.groups.iter().map(|g| g.name.clone()).collect();

    validate(
        &input,
        &provider_proxies,
        &provider_rule_providers,
        &custom_node_names,
        &custom_group_names,
        &mut errors,
    );

    if !errors.is_empty() {
        return Err(ConvertError::Validation(errors));
    }

    // ── 构建输出配置 ──────────────────────────────────────────────────────────

    // proxies:两个块按 `node_section_order` 拼接。机场块保持上游序(用户不可排);自定义块是
    // 启用的自定义节点,按 `node_order` 重排(新节点落到末尾)。
    let provider_block = sequence_of(root.get("proxies"));
    let mut custom_block: Vec<Value> = parsed_nodes
        .into_iter()
        .map(|(_, node)| Value::Mapping(node))
        .collect();
    reorder_by_name(
        &mut custom_block,
        |item| item.get("name").and_then(Value::as_str),
        &input.node_order,
    );
    let mut proxies = concat_sections(provider_block, custom_block, &input.node_section_order);
    let reality_fingerprint_risks = apply_reality_mlkem(&mut proxies, input.reality_mlkem);
    root.insert(Value::from("proxies"), Value::Sequence(proxies));

    // proxy-groups:整体替换为用户的自定义分组(同 `rules`)。机场分组 *不* 透传——管理员经
    // `import-provider-groups` 把它们导入为可编辑的自定义分组,故机场更新永不改变分组,除非重新
    // 导入。之后再应用手动排序。
    let mut groups: Vec<Value> = input
        .groups
        .into_iter()
        .map(|group| Value::Mapping(build_group(group)))
        .collect();
    reorder_by_name(
        &mut groups,
        |item| item.get("name").and_then(Value::as_str),
        &input.group_order,
    );
    root.insert(Value::from("proxy-groups"), Value::Sequence(groups));

    // rules:整体替换为用户的规则。
    let rules: Vec<Value> = rule_lines(input.rules)
        .map(|(_, line)| Value::from(line))
        .collect();
    root.insert(Value::from("rules"), Value::Sequence(rules));

    // proxy-providers:MVP 阶段剥离(SSRF/缓存绕过风险)。
    root.remove(Value::from("proxy-providers"));

    // rule-providers:机场自己的 map 原样透传,使导入的机场 RULE-SET 规则仍能解析;另把被本
    // profile `RULE-SET` 规则引用到的、面板托管的自定义规则集合并在上(同名覆盖机场条目)。
    // 撞名收集:自定义规则集注入时若覆盖了机场同名 `rule-providers` 条目(`insert` 返回旧值),
    // 记下名字——由生成层据此告警,避免静默替换。撞名与覆盖在同一次插入里判定,无需二次解析。
    let mut rule_provider_conflicts: Vec<String> = Vec::new();
    if !input.rule_providers.is_empty() {
        let mut map = match root.get("rule-providers") {
            Some(Value::Mapping(m)) => m.clone(),
            _ => Mapping::new(),
        };
        for rp in &input.rule_providers {
            if map
                .insert(Value::from(rp.name.clone()), build_rule_provider(rp))
                .is_some()
            {
                rule_provider_conflicts.push(rp.name.clone());
            }
        }
        root.insert(Value::from("rule-providers"), Value::Mapping(map));
    }

    // 其余所有顶层键(dns、tun…)透传。

    let output = Value::Mapping(root);
    let limits_error =
        || ConvertError::Validation(vec!["generated YAML exceeds resource limits".into()]);
    yaml::check_value(&output).map_err(|_| limits_error())?;
    let yaml = yaml::serialize_limited(&output).map_err(|error| match error {
        yaml::YamlError::TooComplex => limits_error(),
        _ => ConvertError::OutputSerialize,
    })?;
    Ok(ConvertOutput {
        yaml,
        ruleset_conflicts: rule_provider_conflicts,
        reality_fingerprint_risks,
    })
}

fn validate(
    input: &ConvertInput,
    provider_proxies: &[String],
    provider_rule_providers: &[String],
    custom_node_names: &[String],
    custom_group_names: &[String],
    errors: &mut Vec<String>,
) {
    // 内置保留名:自定义节点、自定义分组或机场透传代理占用 DIRECT/REJECT 等内置名时,Mihomo
    // 会以 duplicate name 拒绝加载整个配置,须在校验阶段拦截(GLOBAL 分组是上游支持的用法,放行)。
    for name in provider_proxies {
        if RESERVED_NODE_NAMES.contains(&name.as_str()) {
            errors.push(format!(
                "provider proxy `{name}` conflicts with a reserved built-in name"
            ));
        }
    }
    for name in custom_node_names {
        if RESERVED_NODE_NAMES.contains(&name.as_str()) {
            errors.push(format!(
                "custom node `{name}` conflicts with a reserved built-in name"
            ));
        }
    }
    for name in custom_group_names {
        if RESERVED_GROUP_NAMES.contains(&name.as_str()) {
            errors.push(format!(
                "custom group `{name}` conflicts with a reserved built-in name"
            ));
        }
    }

    // 自定义节点名不得与机场代理名冲突(代理仍透传 + 追加)。机场分组已不在输出里(被自定义
    // 分组替换),故不存在分组名冲突。
    for name in custom_node_names {
        if provider_proxies.contains(name) {
            errors.push(format!(
                "custom node `{name}` conflicts with a provider proxy name"
            ));
        }
    }

    // 代理与分组共用同一命名空间:自定义分组不得与任何代理(机场或自定义节点)重名。
    for name in custom_group_names {
        if provider_proxies.contains(name) || custom_node_names.contains(name) {
            errors.push(format!("custom group `{name}` conflicts with a proxy name"));
        }
    }

    // 已知的引用目标:机场代理(透传)、自定义节点(追加)、自定义分组(输出里唯一的分组)与内置
    // 策略。引用一个未导入的机场分组即为未知目标。
    let known = |name: &str| {
        provider_proxies.iter().any(|n| n == name)
            || custom_node_names.iter().any(|n| n == name)
            || custom_group_names.iter().any(|n| n == name)
            || BUILTIN_POLICIES.contains(&name)
    };

    // 自定义分组成员必须引用存在的东西。
    for group in &input.groups {
        for member in &group.members {
            if !known(member) {
                errors.push(format!(
                    "custom group `{}` references unknown member `{member}`",
                    group.name
                ));
            }
        }
    }

    // 分组之间的引用不得成环(Mihomo 拒绝加载含环的分组)。
    for group in &input.groups {
        if in_cycle(&input.groups, &group.name) {
            errors.push(format!(
                "custom group `{}` is part of a reference cycle",
                group.name
            ));
        }
    }

    // 每条规则的策略目标必须存在。无法可靠解析的高级/逻辑规则透传,不做目标校验。
    // `RULE-SET` 引用的规则集必须存在:本订阅注入的规则集(③,仅已启用者)或机场自带的 rule-providers。
    // 逻辑规则(AND/OR/NOT/SUB-RULE)内嵌套的 `RULE-SET` 同样会被 Mihomo 解析,一并检查。
    for (lineno, line) in rule_lines(input.rules) {
        if let Some(target) = rule_target(line) {
            if !known(&target) {
                errors.push(format!(
                    "rules line {lineno} references unknown policy `{target}`"
                ));
            }
        }
        match ruleset_names_in_line(line) {
            Ok(names) => {
                for name in names {
                    let injected = input.rule_providers.iter().any(|rp| rp.name == name);
                    if !injected && !provider_rule_providers.iter().any(|n| n == &name) {
                        errors.push(format!(
                            "rules line {lineno} references unknown rule-set `{name}`"
                        ));
                    }
                }
            }
            Err(()) => errors.push(format!(
                "rules line {lineno} exceeds the supported logic nesting depth"
            )),
        }
    }
}

/// 分组 `start` 是否在引用环上:沿「成员是自定义分组」的边从其成员出发能否回到它自身。
fn in_cycle(groups: &[CustomGroup], start: &str) -> bool {
    let members_of = |name: &str| {
        groups
            .iter()
            .find(|g| g.name == name)
            .map_or(&[][..], |g| g.members.as_slice())
    };
    let mut stack: Vec<&str> = members_of(start).iter().map(String::as_str).collect();
    let mut seen = std::collections::HashSet::new();
    while let Some(name) = stack.pop() {
        if name == start {
            return true;
        }
        if seen.insert(name) {
            stack.extend(members_of(name).iter().map(String::as_str));
        }
    }
    false
}

/// 从 `proxies`/`proxy-groups` 值中提取各代理/分组的 `name`。
fn names_in(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Sequence(items)) => items
            .iter()
            .filter_map(|item| item.get("name").and_then(Value::as_str).map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// 按期望的名字顺序对具名项做稳定重排。
///
/// 名字出现在 `order` 中的项按 `order` 的次序移到前面;其余保持原相对顺序在后。无法解析出名字
/// 的项,以及 `order` 里不在 `items` 中的条目,均忽略。`order` 为空时是 no-op。由转换器(自定义
/// 节点块、分组)与全局节点池、全局规则集库的排序端点共用。
pub fn reorder_by_name<T, F>(items: &mut Vec<T>, name_of: F, order: &[String])
where
    F: Fn(&T) -> Option<&str>,
{
    if order.is_empty() || items.len() < 2 {
        return;
    }
    let rank: std::collections::HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();
    // 按 (rank, 原始下标) 稳定排序;无 rank 的项排在有 rank 的之后,同时保持其默认顺序。
    let mut indexed: Vec<(usize, T)> = std::mem::take(items).into_iter().enumerate().collect();
    indexed.sort_by_key(|(idx, item)| {
        let r = name_of(item).and_then(|n| rank.get(n)).copied();
        (r.unwrap_or(usize::MAX), *idx)
    });
    *items = indexed.into_iter().map(|(_, item)| item).collect();
}

/// 按 `section_order`(`"provider"`/`"custom"` 的排列)拼接机场块与自定义块。当顺序为空、或没把
/// `"custom"` 排在 `"provider"` 前面时,默认机场块在前。
pub fn concat_sections(
    provider: Vec<Value>,
    custom: Vec<Value>,
    section_order: &[String],
) -> Vec<Value> {
    if section_order.first().map(String::as_str) == Some("custom") {
        let mut out = custom;
        out.extend(provider);
        out
    } else {
        let mut out = provider;
        out.extend(custom);
        out
    }
}

/// 克隆某个值的序列;缺失或不是序列时返回空。
fn sequence_of(value: Option<&Value>) -> Vec<Value> {
    match value {
        Some(Value::Sequence(items)) => items.clone(),
        _ => Vec::new(),
    }
}

const MLKEM_KEY: &str = "support-x25519mlkem768";

/// 新版 Xray 服务端(25.5.16 起)要求 REALITY 握手携带 X25519MLKEM768 密钥协商,否则拒绝;旧版
/// REALITY 服务端却不兼容该协商,故 mihomo 默认关闭,需在 `reality-opts` 里显式声明
/// `support-x25519mlkem768`。`inject`(per-profile `reality_mlkem`)为真时,为未声明(或值为
/// null,mihomo 视同未写)的 REALITY 节点补 `true`;显式值不动。只有 `public-key` 非空的
/// `reality-opts` 才会让 mihomo 启用 REALITY,其余节点不碰。键名按 [`mihomo_key`] 匹配。
///
/// 该协商基于 Chrome 指纹实现:ML-KEM 生效而 `client-fingerprint` 缺失或非 `chrome` 时它不会随
/// ClientHello 发出(缺失时 mihomo 更会直接拒绝 REALITY 节点)——不代为改动用户指纹,仅返回
/// 这些节点名供生成层告警。
fn apply_reality_mlkem(proxies: &mut [Value], inject: bool) -> Vec<String> {
    let mut fingerprint_risks = Vec::new();
    for proxy in proxies {
        let Value::Mapping(m) = proxy else {
            continue;
        };
        let Some(opts_key) = mihomo_key(m, "reality-opts") else {
            continue;
        };
        let Some(Value::Mapping(opts)) = m.get_mut(&opts_key) else {
            continue;
        };
        let has_public_key = mihomo_key(opts, "public-key")
            .and_then(|k| opts.get(&k))
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
        if !has_public_key {
            continue;
        }
        let mlkem_key = mihomo_key(opts, MLKEM_KEY);
        let explicit = mlkem_key
            .as_ref()
            .and_then(|k| opts.get(k))
            .filter(|v| !v.is_null());
        let enabled = match explicit {
            Some(value) => mihomo_bool(value),
            None => {
                if inject {
                    // 值为 null 时原位改写(保留用户的键名写法),缺失时补规范键。
                    let key = mlkem_key.unwrap_or_else(|| Value::from(MLKEM_KEY));
                    opts.insert(key, Value::from(true));
                }
                inject
            }
        };
        if !enabled {
            continue;
        }
        let fingerprint = mihomo_key(m, "client-fingerprint")
            .and_then(|k| m.get(&k))
            .and_then(Value::as_str);
        if fingerprint != Some("chrome") {
            let name = m.get("name").and_then(Value::as_str).unwrap_or("?");
            fingerprint_risks.push(name.to_string());
        }
    }
    fingerprint_risks
}

/// 按 mihomo 解码代理字段的规则在映射里定位键 `name`:先精确匹配,再把 `_` 视作 `-` 后忽略大小写
/// 匹配(见 mihomo `common/structure`)。返回映射里的实际键,供原位读写。
fn mihomo_key(m: &Mapping, name: &str) -> Option<Value> {
    if m.contains_key(name) {
        return Some(Value::from(name));
    }
    m.keys()
        .find(|k| {
            k.as_str()
                .is_some_and(|s| s.replace('_', "-").eq_ignore_ascii_case(name))
        })
        .cloned()
}

/// mihomo 解码 bool 字段的取值:布尔原样,整数非零为真;其余类型 mihomo 会拒绝加载该节点,按假处理。
fn mihomo_bool(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::Number(n) => {
            n.as_i64().is_some_and(|i| i != 0) || n.as_u64().is_some_and(|u| u != 0)
        }
        _ => false,
    }
}

fn build_group(group: CustomGroup) -> Mapping {
    let mut m = Mapping::new();
    m.insert(Value::from("name"), Value::from(group.name));
    m.insert(Value::from("type"), Value::from(group.group_type));
    // 在成员列表之前合并分组特有的选项(url、interval…)。结构键由分组自身字段决定,选项不得
    // 覆盖,否则输出会与已校验的名字/类型/成员不一致。
    if let Some(opts) = group.options {
        if let Ok(Value::Mapping(opt_map)) = serde_yaml::to_value(&opts) {
            for (k, v) in opt_map {
                if !matches!(k.as_str(), Some("name" | "type" | "proxies")) {
                    m.insert(k, v);
                }
            }
        }
    }
    let proxies = group.members.into_iter().map(Value::from).collect();
    m.insert(Value::from("proxies"), Value::Sequence(proxies));
    m
}

/// rule-provider 默认刷新间隔(秒):客户端按此间隔回面板托管链接拉取规则集内容。
const RULE_PROVIDER_INTERVAL: i64 = 86400;

/// 构建一个指向面板托管链接的 http 型 rule-provider 条目。
fn build_rule_provider(rp: &RuleProvider) -> Value {
    let mut m = Mapping::new();
    m.insert(Value::from("type"), Value::from("http"));
    m.insert(Value::from("behavior"), Value::from(rp.behavior.clone()));
    m.insert(Value::from("format"), Value::from(rp.format.clone()));
    m.insert(Value::from("url"), Value::from(rp.url.clone()));
    m.insert(
        Value::from("path"),
        Value::from(format!("./ruleset/{}.{}", rp.name, rp.format)),
    );
    m.insert(Value::from("interval"), Value::from(RULE_PROVIDER_INTERVAL));
    Value::Mapping(m)
}

/// 提取 rules 文本中 `RULE-SET` 规则引用的规则集名(去重、保序),含逻辑规则内嵌套的引用。
/// 供 generate 决定要把哪些面板托管的规则集注入输出的 `rule-providers:`。
pub fn ruleset_refs(rules: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (_, line) in rule_lines(rules) {
        let Ok(names) = ruleset_names_in_line(line) else {
            continue;
        };
        for name in names {
            if !out.iter().any(|n| n == &name) {
                out.push(name);
            }
        }
    }
    out
}

/// 逻辑规则(AND/OR/NOT/SUB-RULE)括号嵌套的最大解析深度。Mihomo 本身不限深度;超过此深度
/// 时面板无法可靠收集内嵌的规则集引用,宁可在校验阶段拒绝也不静默漏注入。
const MAX_LOGIC_DEPTH: usize = 16;

/// 一行规则引用到的规则集名,含逻辑规则内嵌套的引用;嵌套深度超限返回 `Err(())`。
///
/// 镜像 Mihomo 的解析(`rules/common/base.go` 的 `ParseRulePayload` 与 `rules/logic/logic.go`):
/// 行级按 `,` 朴素切分(每项去空格),逻辑类型的末项是策略目标、中间部分 join 成 payload;
/// 逻辑 payload 必须是 `(…)` 包裹,括号平衡扫描后只下钻未被其他区间包含的顶层区间,区间内
/// 按「无目标」格式再解析(`RULE-SET` 的名字在第 2 项);逻辑内部不支持 SUB-RULE/MATCH。
/// 无法可靠解析的行(括号不平衡、payload 非 `(…)` 包裹等,Mihomo 同样会拒绝)不收集,保持透传。
fn ruleset_names_in_line(line: &str) -> Result<Vec<String>, ()> {
    let mut names = Vec::new();
    let items: Vec<&str> = line.split(',').map(str::trim).collect();
    let tp = items
        .first()
        .map(|s| s.to_ascii_uppercase())
        .unwrap_or_default();
    match tp.as_str() {
        "RULE-SET" => {
            if let Some(name) = items.get(1).filter(|n| !n.is_empty()) {
                names.push(name.to_string());
            }
        }
        "AND" | "OR" | "NOT" | "SUB-RULE" => {
            // 不足 3 项时分不出 payload 与目标,Mihomo 同样拒绝;透传不收集。
            if items.len() < 3 {
                return Ok(names);
            }
            let payload = items[1..items.len() - 1].join(",");
            // SUB-RULE 的行级 payload 会被 Mihomo 再包一层括号后按逻辑 payload 解析。
            let payload = if tp == "SUB-RULE" {
                format!("({payload})")
            } else {
                payload
            };
            collect_logic_payload(&payload, 1, &mut names)?;
        }
        _ => {}
    }
    Ok(names)
}

/// 逻辑 payload(`(…)` 包裹):对每个顶层区间内的子规则递归收集规则集引用。
/// payload 非括号包裹或括号不平衡时该行无法可靠解析,放弃下钻(已收集的保留)。
fn collect_logic_payload(payload: &str, depth: usize, names: &mut Vec<String>) -> Result<(), ()> {
    if depth > MAX_LOGIC_DEPTH {
        return Err(());
    }
    let Some(ranges) = next_level_ranges(payload) else {
        return Ok(());
    };
    for (start, end) in ranges {
        let sub = &payload[start + 1..end];
        let items: Vec<&str> = sub.split(',').map(str::trim).collect();
        let tp = items
            .first()
            .map(|s| s.to_ascii_uppercase())
            .unwrap_or_default();
        match tp.as_str() {
            "RULE-SET" => {
                if let Some(name) = items.get(1).filter(|n| !n.is_empty()) {
                    names.push(name.to_string());
                }
            }
            // 逻辑内的逻辑规则没有目标项,payload 是第 2 项起的全部。
            "AND" | "OR" | "NOT" if items.len() >= 2 => {
                collect_logic_payload(&items[1..].join(","), depth + 1, names)?;
            }
            // 逻辑内部不支持 SUB-RULE/MATCH;普通类型与未知类型不含规则集引用。
            _ => {}
        }
    }
    Ok(())
}

/// 逻辑 payload 括号平衡扫描后的顶层区间(字节索引,含两端括号)。镜像 Mihomo 的
/// `format`+`findSubRuleRange`:括号不平衡返回 `None`;跳过恰好覆盖整个 payload 的
/// 最外层区间(如 `AND,(RULE-SET,ads),DIRECT` 的单括号,Mihomo 同样不下钻),并丢弃
/// 被已选区间包含的区间。
fn next_level_ranges(payload: &str) -> Option<Vec<(usize, usize)>> {
    if !payload.starts_with('(') || !payload.ends_with(')') {
        return None;
    }
    let mut stack: Vec<usize> = Vec::new();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for (i, c) in payload.char_indices() {
        match c {
            '(' => stack.push(i),
            ')' => ranges.push((stack.pop()?, i)),
            _ => {}
        }
    }
    if !stack.is_empty() {
        return None;
    }
    ranges.sort_by_key(|r| r.0);
    let mut top: Vec<(usize, usize)> = Vec::new();
    for r in ranges {
        if r.0 == 0 && r.1 == payload.len() - 1 {
            continue;
        }
        if !top.iter().any(|t| t.0 < r.0 && t.1 > r.1) {
            top.push(r);
        }
    }
    Some(top)
}

/// 规则行是否是 MATCH 兜底规则(忽略大小写)。导入规则行时据此把新行插到兜底之前:
/// 客户端按规则顺序匹配,追加在 MATCH 后的规则永远不会命中。
pub(crate) fn is_match_line(line: &str) -> bool {
    line.split(',')
        .next()
        .is_some_and(|k| k.trim().eq_ignore_ascii_case("MATCH"))
}

/// 遍历非空、非注释的规则行,带其 1-based 行号(按原始文本编号,使消息与编辑器一致)。
fn rule_lines(rules: &str) -> impl Iterator<Item = (usize, &str)> {
    rules.lines().enumerate().filter_map(|(i, raw)| {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            None
        } else {
            Some((i + 1, line))
        }
    })
}

/// 规则行的策略目标;当无法可靠解析(逻辑/嵌套规则)、应原样透传不检查时返回 `None`。
fn rule_target(line: &str) -> Option<String> {
    // 逻辑/嵌套规则含括号;跳过目标校验。
    if line.contains('(') {
        return None;
    }
    let parts: Vec<&str> = line.split(',').map(str::trim).collect();
    let kind = parts.first()?.to_ascii_uppercase();
    let target = if kind == "MATCH" {
        parts.get(1)
    } else {
        parts.get(2)
    };
    target.map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_reports_rule_provider_name_collisions() {
        // PROVIDER 的 rule-providers 含 `ads`;注入同名自定义规则集即撞名(覆盖机场版),应在
        // convert 返回的 conflicts 中报出;名字不同则不报。
        let rp = |name: &str| RuleProvider {
            name: name.into(),
            behavior: "domain".into(),
            format: "yaml".into(),
            url: "https://panel.example/ruleset".into(),
        };

        let mut collide = input("MATCH,DIRECT", vec![], vec![]);
        collide.rule_providers = vec![rp("ads")];
        let conflicts = convert(collide)
            .expect("conversion succeeds")
            .ruleset_conflicts;
        assert_eq!(conflicts, vec!["ads".to_string()]);

        let mut distinct = input("MATCH,DIRECT", vec![], vec![]);
        distinct.rule_providers = vec![rp("MyAdBlock")];
        let conflicts = convert(distinct)
            .expect("conversion succeeds")
            .ruleset_conflicts;
        assert!(conflicts.is_empty());
    }

    const PROVIDER: &str = r#"
port: 7890
proxy-providers:
  remote:
    url: https://example.com/list
rule-providers:
  ads:
    type: http
    url: https://example.com/ads.yaml
dns:
  enable: true
proxies:
  - { name: hk-1, type: ss, server: 1.2.3.4, port: 8388 }
proxy-groups:
  - { name: Proxy, type: select, proxies: [hk-1] }
rules:
  - MATCH,DIRECT
"#;

    fn input<'a>(
        rules: &'a str,
        nodes: Vec<CustomNode>,
        groups: Vec<CustomGroup>,
    ) -> ConvertInput<'a> {
        ConvertInput {
            provider_yaml: PROVIDER,
            rules,
            nodes,
            groups,
            node_order: Vec::new(),
            node_section_order: Vec::new(),
            group_order: Vec::new(),
            rule_providers: Vec::new(),
            reality_mlkem: true,
        }
    }

    fn out(input: ConvertInput) -> Mapping {
        let yaml = convert(input).expect("conversion succeeds").yaml;
        serde_yaml::from_str(&yaml).unwrap()
    }

    #[test]
    fn appends_nodes_replaces_groups_and_rules() {
        let nodes = vec![CustomNode {
            name: "my-ss".into(),
            content: "{ name: my-ss, type: ss, server: 9.9.9.9, port: 1080 }".into(),
        }];
        let groups = vec![CustomGroup {
            name: "MyGroup".into(),
            group_type: "select".into(),
            members: vec!["my-ss".into(), "hk-1".into(), "DIRECT".into()],
            options: None,
        }];
        let root = out(input(
            "DOMAIN-SUFFIX,example.com,MyGroup\nMATCH,DIRECT",
            nodes,
            groups,
        ));

        // Proxies: provider entries + appended custom nodes.
        let proxies = root.get("proxies").unwrap().as_sequence().unwrap();
        assert_eq!(names_in(root.get("proxies")), vec!["hk-1", "my-ss"]);
        assert_eq!(proxies.len(), 2);

        // proxy-groups: replaced with custom groups only — the provider's `Proxy`
        // group is dropped (not passed through).
        assert_eq!(names_in(root.get("proxy-groups")), vec!["MyGroup"]);

        let rules = root.get("rules").unwrap().as_sequence().unwrap();
        assert_eq!(rules.len(), 2);
        assert_eq!(
            rules[0].as_str().unwrap(),
            "DOMAIN-SUFFIX,example.com,MyGroup"
        );
    }

    #[test]
    fn strips_proxy_providers_and_passes_through_others() {
        let root = out(input("MATCH,DIRECT", vec![], vec![]));
        assert!(
            root.get("proxy-providers").is_none(),
            "proxy-providers stripped"
        );
        assert!(
            root.get("rule-providers").is_some(),
            "rule-providers passed through"
        );
        assert!(root.get("dns").is_some(), "unknown keys passed through");
        assert!(root.get("port").is_some());
    }

    #[test]
    fn custom_group_may_reuse_a_provider_group_name() {
        // Provider groups are replaced, so a custom group named `Proxy` (the
        // provider's group name) is allowed — this is exactly what importing a
        // provider group produces.
        let groups = vec![CustomGroup {
            name: "Proxy".into(),
            group_type: "select".into(),
            members: vec!["hk-1".into(), "DIRECT".into()],
            options: None,
        }];
        let root = out(input("MATCH,Proxy", vec![], groups));
        assert_eq!(names_in(root.get("proxy-groups")), vec!["Proxy"]);
    }

    #[test]
    fn rule_referencing_unknown_group_is_rejected() {
        let err = convert(input("DOMAIN,x.com,Ghost\nMATCH,DIRECT", vec![], vec![])).unwrap_err();
        match err {
            ConvertError::Validation(errs) => {
                assert_eq!(errs.len(), 1);
                assert!(errs[0].contains("rules line 1"));
                assert!(errs[0].contains("Ghost"));
            }
            _ => panic!("expected validation error"),
        }
    }

    #[test]
    fn group_member_referencing_unknown_is_rejected() {
        let groups = vec![CustomGroup {
            name: "G".into(),
            group_type: "select".into(),
            members: vec!["does-not-exist".into()],
            options: None,
        }];
        let err = convert(input("MATCH,DIRECT", vec![], groups)).unwrap_err();
        match err {
            ConvertError::Validation(errs) => {
                assert!(errs
                    .iter()
                    .any(|e| e.contains("unknown member `does-not-exist`")));
            }
            _ => panic!("expected validation error"),
        }
    }

    #[test]
    fn rules_can_target_provider_proxies_and_builtins() {
        // hk-1 is a provider proxy; DIRECT is a builtin — both valid targets.
        let root = out(input(
            "DOMAIN,a.com,hk-1\nIP-CIDR,1.2.3.4/32,DIRECT,no-resolve\nMATCH,hk-1",
            vec![],
            vec![],
        ));
        assert_eq!(root.get("rules").unwrap().as_sequence().unwrap().len(), 3);
    }

    #[test]
    fn rule_targeting_unimported_provider_group_is_rejected() {
        // `Proxy` is a provider group; since provider groups are no longer in the
        // output, referencing one that wasn't imported is an unknown target.
        let err = convert(input("MATCH,Proxy", vec![], vec![])).unwrap_err();
        match err {
            ConvertError::Validation(errs) => {
                assert!(errs.iter().any(|e| e.contains("Proxy")));
            }
            _ => panic!("expected validation error"),
        }
    }

    #[test]
    fn logical_rules_pass_through_without_target_validation() {
        // Contains parentheses: target not parseable, must not error.
        let rules = "AND,((DOMAIN,x.com),(NETWORK,udp)),Proxy\nMATCH,DIRECT";
        assert!(convert(input(rules, vec![], vec![])).is_ok());
    }

    #[test]
    fn group_options_are_merged() {
        let groups = vec![CustomGroup {
            name: "Auto".into(),
            group_type: "url-test".into(),
            members: vec!["hk-1".into()],
            options: Some(serde_json::json!({"url": "http://x/generate_204", "interval": 300})),
        }];
        let root = out(input("MATCH,Auto", vec![], groups));
        let groups = root.get("proxy-groups").unwrap().as_sequence().unwrap();
        let auto = groups
            .iter()
            .find(|g| g.get("name").and_then(Value::as_str) == Some("Auto"))
            .unwrap();
        assert_eq!(auto.get("interval").unwrap().as_u64().unwrap(), 300);
        assert_eq!(auto.get("type").unwrap().as_str().unwrap(), "url-test");
    }

    #[test]
    fn invalid_provider_yaml_is_provider_parse_error() {
        let bad = ConvertInput {
            provider_yaml: "this: is: not: valid",
            rules: "MATCH,DIRECT",
            nodes: vec![],
            groups: vec![],
            node_order: Vec::new(),
            node_section_order: Vec::new(),
            group_order: Vec::new(),
            rule_providers: Vec::new(),
            reality_mlkem: true,
        };
        assert!(matches!(convert(bad), Err(ConvertError::ProviderParse)));
    }

    #[test]
    fn group_order_reorders_groups_and_appends_unlisted() {
        let groups = vec![
            CustomGroup {
                name: "G1".into(),
                group_type: "select".into(),
                members: vec!["hk-1".into()],
                options: None,
            },
            CustomGroup {
                name: "G2".into(),
                group_type: "select".into(),
                members: vec!["hk-1".into()],
                options: None,
            },
        ];
        // Default custom groups are [G1, G2] (provider groups are not output).
        // Ask for [G2]; `G1` is unlisted and must fall to the end.
        let mut inp = input("MATCH,DIRECT", vec![], groups);
        inp.group_order = vec!["G2".into()];
        let root = out(inp);
        assert_eq!(names_in(root.get("proxy-groups")), vec!["G2", "G1"]);
    }

    fn node(name: &str) -> CustomNode {
        CustomNode {
            name: name.into(),
            content: format!("{{ name: {name}, type: ss, server: 9.9.9.9, port: 1080 }}"),
        }
    }

    #[test]
    fn node_order_reorders_only_the_custom_block() {
        // Provider block [hk-1] stays upstream; custom block [a, b] is reordered
        // by `node_order` (b first, a unlisted -> end). Default section order is
        // provider-first, so output = [hk-1] ++ [b, a].
        let mut inp = input("MATCH,DIRECT", vec![node("a"), node("b")], vec![]);
        inp.node_order = vec!["b".into()];
        let root = out(inp);
        assert_eq!(names_in(root.get("proxies")), vec!["hk-1", "b", "a"]);
    }

    #[test]
    fn node_section_order_puts_custom_block_first() {
        let mut inp = input("MATCH,DIRECT", vec![node("a"), node("b")], vec![]);
        inp.node_section_order = vec!["custom".into(), "provider".into()];
        let root = out(inp);
        assert_eq!(names_in(root.get("proxies")), vec!["a", "b", "hk-1"]);
    }

    #[test]
    fn empty_orders_keep_provider_first_then_custom() {
        let root = out(input("MATCH,DIRECT", vec![node("z")], vec![]));
        assert_eq!(names_in(root.get("proxies")), vec!["hk-1", "z"]);
    }

    fn group(name: &str, members: &[&str]) -> CustomGroup {
        CustomGroup {
            name: name.into(),
            group_type: "select".into(),
            members: members.iter().map(|m| m.to_string()).collect(),
            options: None,
        }
    }

    fn validation_errors(input: ConvertInput) -> Vec<String> {
        match convert(input) {
            Err(ConvertError::Validation(errs)) => errs,
            _ => panic!("expected validation error"),
        }
    }

    #[test]
    fn rule_set_reference_must_exist() {
        let errs = validation_errors(input("RULE-SET,ghost,DIRECT\nMATCH,DIRECT", vec![], vec![]));
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("rules line 1"));
        assert!(errs[0].contains("unknown rule-set `ghost`"));
    }

    #[test]
    fn rule_set_may_reference_provider_or_injected_rule_providers() {
        // `ads` 来自机场 rule-providers,`mine` 来自本订阅注入的规则集。
        let mut inp = input(
            "RULE-SET,ads,DIRECT\nRULE-SET,mine,DIRECT\nMATCH,DIRECT",
            vec![],
            vec![],
        );
        inp.rule_providers = vec![RuleProvider {
            name: "mine".into(),
            behavior: "domain".into(),
            format: "yaml".into(),
            url: "https://panel.example/ruleset".into(),
        }];
        assert!(convert(inp).is_ok());
    }

    #[test]
    fn logic_nested_rule_set_reference_must_exist() {
        // 逻辑规则内嵌套的 RULE-SET 同样被 Mihomo 解析,引用不存在的规则集须报错。
        let errs = validation_errors(input(
            "AND,((RULE-SET,ghost),(NETWORK,tcp)),DIRECT\nMATCH,DIRECT",
            vec![],
            vec![],
        ));
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("rules line 1"));
        assert!(errs[0].contains("unknown rule-set `ghost`"));
    }

    #[test]
    fn logic_nested_rule_set_may_reference_provider_or_injected() {
        // `ads` 来自机场 rule-providers,`mine` 来自本订阅注入;多层嵌套也能找到。
        let mut inp = input(
            "AND,((OR,((RULE-SET,ads),(RULE-SET,mine))),(NETWORK,tcp)),DIRECT\nMATCH,DIRECT",
            vec![],
            vec![],
        );
        inp.rule_providers = vec![RuleProvider {
            name: "mine".into(),
            behavior: "domain".into(),
            format: "yaml".into(),
            url: "https://panel.example/ruleset".into(),
        }];
        assert!(convert(inp).is_ok());
    }

    #[test]
    fn sub_rule_line_nested_rule_set_reference_must_exist() {
        let errs = validation_errors(input(
            "SUB-RULE,(RULE-SET,ghost,DIRECT),sub\nMATCH,DIRECT",
            vec![],
            vec![],
        ));
        assert!(errs.iter().any(|e| e.contains("unknown rule-set `ghost`")));
    }

    #[test]
    fn single_paren_logic_payload_is_not_collected() {
        // `AND,(RULE-SET,ads),DIRECT`:单括号恰好覆盖整个 payload,Mihomo 不下钻(rules 为空),
        // 面板同样不收集——不校验、不注入,原样透传。
        assert!(convert(input(
            "AND,(RULE-SET,ghost),DIRECT\nMATCH,DIRECT",
            vec![],
            vec![]
        ))
        .is_ok());
    }

    #[test]
    fn unbalanced_logic_line_passes_through_without_collection() {
        // 括号不平衡:Mihomo 拒绝该行,面板无法可靠解析则透传,不收集其引用。
        assert!(convert(input(
            "AND,((RULE-SET,ghost),(NETWORK,tcp),DIRECT\nMATCH,DIRECT",
            vec![],
            vec![]
        ))
        .is_ok());
    }

    #[test]
    fn logic_nesting_depth_over_limit_is_rejected() {
        // 16 层嵌套 AND(逻辑内子规则不带 target,与 Mihomo 的 needTarget=false 解析一致)
        // 超过解析上限:宁可校验拒绝也不静默漏注入。
        let mut inner = String::from("RULE-SET,ghost");
        for _ in 0..16 {
            inner = format!("AND,(({inner}))");
        }
        let line = format!("AND,(({inner})),DIRECT");
        let errs = validation_errors(input(&format!("{line}\nMATCH,DIRECT"), vec![], vec![]));
        assert!(errs
            .iter()
            .any(|e| e.contains("exceeds the supported logic nesting depth")));
    }

    #[test]
    fn ruleset_refs_collects_logic_nested_names() {
        let rules = "AND,((RULE-SET,ads),(NETWORK,tcp)),DIRECT\n\
                     SUB-RULE,(RULE-SET,sub1,DIRECT),s\n\
                     RULE-SET,top,DIRECT\n\
                     AND,(RULE-SET,not-collected),DIRECT\n\
                     MATCH,DIRECT";
        assert_eq!(ruleset_refs(rules), vec!["ads", "sub1", "top"]);
    }

    #[test]
    fn group_name_conflicting_with_a_proxy_is_rejected() {
        let groups = vec![group("hk-1", &["DIRECT"]), group("a", &["DIRECT"])];
        let errs = validation_errors(input("MATCH,DIRECT", vec![node("a")], groups));
        assert!(errs
            .iter()
            .any(|e| e.contains("custom group `hk-1` conflicts")));
        assert!(errs
            .iter()
            .any(|e| e.contains("custom group `a` conflicts")));
    }

    #[test]
    fn group_reference_cycle_is_rejected() {
        let groups = vec![
            group("A", &["B"]),
            group("B", &["A"]),
            group("Self", &["Self"]),
            group("Ok", &["A", "hk-1"]),
        ];
        let errs = validation_errors(input("MATCH,DIRECT", vec![], groups));
        let cyclic: Vec<&String> = errs.iter().filter(|e| e.contains("cycle")).collect();
        assert_eq!(cyclic.len(), 3, "{errs:?}");
        assert!(
            !errs.iter().any(|e| e.contains("`Ok`")),
            "引用环但不在环上的分组不报"
        );
    }

    #[test]
    fn reserved_builtin_names_are_rejected_for_nodes_and_groups() {
        // 自定义节点/分组占用内置名(DIRECT/REJECT/…):Mihomo 会以 duplicate name 拒绝加载。
        let errs = validation_errors(input("MATCH,hk-1", vec![node("DIRECT")], vec![]));
        assert!(errs
            .iter()
            .any(|e| e.contains("custom node `DIRECT` conflicts with a reserved built-in name")));
        let errs = validation_errors(input(
            "MATCH,hk-1",
            vec![],
            vec![group("REJECT", &["hk-1"])],
        ));
        assert!(errs
            .iter()
            .any(|e| e.contains("custom group `REJECT` conflicts with a reserved built-in name")));
    }

    #[test]
    fn global_is_allowed_as_a_custom_group_name() {
        // GLOBAL 分组是上游支持的用法:上游仅在用户未定义时才内置创建 GLOBAL。
        let root = out(input(
            "MATCH,GLOBAL",
            vec![],
            vec![group("GLOBAL", &["hk-1"])],
        ));
        assert_eq!(names_in(root.get("proxy-groups")), vec!["GLOBAL"]);
    }

    #[test]
    fn provider_proxy_with_reserved_name_is_rejected() {
        let mut inp = input("MATCH,hk-1", vec![], vec![]);
        inp.provider_yaml =
            "proxies:\n  - { name: DIRECT, type: ss, server: 1.2.3.4, port: 8388 }\n";
        let errs = validation_errors(inp);
        assert!(
            errs.iter()
                .any(|e| e
                    .contains("provider proxy `DIRECT` conflicts with a reserved built-in name"))
        );
    }

    #[test]
    fn nested_groups_without_cycle_are_allowed() {
        let groups = vec![
            group("Outer", &["Inner", "DIRECT"]),
            group("Inner", &["hk-1"]),
        ];
        assert!(convert(input("MATCH,Outer", vec![], groups)).is_ok());
    }

    #[test]
    fn custom_node_output_name_is_the_stored_name() {
        // content 中的 name 与库内 name 不一致(末尾多了空格)或缺失时,输出以库内 name 为准,
        // 使校验(按库内 name)与输出一致。
        let nodes = vec![
            CustomNode {
                name: "a".into(),
                content: "{ name: 'a ', type: ss, server: 9.9.9.9, port: 1080 }".into(),
            },
            CustomNode {
                name: "b".into(),
                content: "{ type: ss, server: 9.9.9.9, port: 1081 }".into(),
            },
        ];
        let root = out(input("MATCH,G", nodes, vec![group("G", &["a", "b"])]));
        assert_eq!(names_in(root.get("proxies")), vec!["hk-1", "a", "b"]);
    }

    #[test]
    fn group_options_cannot_override_structural_keys() {
        let mut g = group("G", &["hk-1"]);
        g.options = Some(serde_json::json!({
            "name": "Hijack",
            "type": "url-test",
            "proxies": ["DIRECT"],
            "url": "http://x/generate_204",
        }));
        let root = out(input("MATCH,G", vec![], vec![g]));
        let out_group = &root.get("proxy-groups").unwrap().as_sequence().unwrap()[0];
        assert_eq!(out_group.get("name").and_then(Value::as_str), Some("G"));
        assert_eq!(
            out_group.get("type").and_then(Value::as_str),
            Some("select")
        );
        assert_eq!(
            out_group.get("proxies").unwrap().as_sequence().unwrap(),
            &vec![Value::from("hk-1")]
        );
        assert_eq!(
            out_group.get("url").and_then(Value::as_str),
            Some("http://x/generate_204"),
            "其余选项照常合并"
        );
    }

    /// 转换后按节点名取其 `reality-opts`。
    fn reality_opts_of(output: &ConvertOutput, name: &str) -> Option<Value> {
        let root: Mapping = serde_yaml::from_str(&output.yaml).unwrap();
        root.get("proxies")?
            .as_sequence()?
            .iter()
            .find(|p| p.get("name").and_then(Value::as_str) == Some(name))?
            .get("reality-opts")
            .cloned()
    }

    #[test]
    fn reality_nodes_get_mlkem_support_injected() {
        // 机场与自定义 REALITY 节点缺省 support-x25519mlkem768 → 补 true;值为 null 视同缺省;
        // 显式值(含 mihomo 认可的下划线/大小写变体键)尊重不动。无 reality-opts、reality-opts
        // 不是映射或没有 public-key(mihomo 不会启用 REALITY)的节点不动。ML-KEM 生效且指纹
        // 缺失或非 chrome 的节点按输出顺序报出;显式关闭 ML-KEM 的不报。
        let nodes = vec![CustomNode {
            name: "my-reality".into(),
            content: "{ type: vless, server: 9.9.9.9, port: 443, tls: true, reality-opts: { public-key: pk } }".into(),
        }];
        let mut inp = input("MATCH,DIRECT", nodes, vec![]);
        inp.provider_yaml = "proxies:
  - { name: vless-new, type: vless, server: 1.2.3.4, port: 443, tls: true, client-fingerprint: chrome, reality-opts: { public-key: pk, short-id: ab } }
  - { name: vless-off, type: vless, server: 1.2.3.5, port: 443, tls: true, client-fingerprint: firefox, reality-opts: { public-key: pk, support-x25519mlkem768: false } }
  - { name: vless-null, type: vless, server: 1.2.3.8, port: 443, tls: true, client-fingerprint: chrome, reality-opts: { public-key: pk, support-x25519mlkem768: ~ } }
  - { name: vless-variant, type: vless, server: 1.2.3.9, port: 443, tls: true, client_fingerprint: firefox, reality_opts: { Public-Key: pk, support_x25519mlkem768: false } }
  - { name: tls-only, type: vless, server: 1.2.3.10, port: 443, tls: true, client-fingerprint: firefox, reality-opts: { short-id: ab } }
  - { name: plain-ss, type: ss, server: 1.2.3.6, port: 8388 }
  - { name: weird, type: vless, server: 1.2.3.7, port: 443, reality-opts: '' }
";
        let output = convert(inp).expect("conversion succeeds");
        assert_eq!(
            output.reality_fingerprint_risks,
            vec!["my-reality".to_string()],
            "仅 ML-KEM 生效且指纹非 chrome 的 REALITY 节点报出"
        );
        let opts = |name: &str| reality_opts_of(&output, name);
        let mlkem = |name: &str| opts(name).unwrap().get(MLKEM_KEY).cloned();
        assert_eq!(mlkem("vless-new"), Some(Value::from(true)));
        assert_eq!(
            opts("vless-new")
                .unwrap()
                .get("public-key")
                .and_then(Value::as_str),
            Some("pk"),
            "原有 reality-opts 键保留"
        );
        assert_eq!(
            mlkem("vless-off"),
            Some(Value::from(false)),
            "显式 false 尊重不动"
        );
        assert_eq!(
            mlkem("vless-null"),
            Some(Value::from(true)),
            "null 视同缺省"
        );
        assert_eq!(
            mlkem("my-reality"),
            Some(Value::from(true)),
            "自定义节点同样注入"
        );
        assert_eq!(mlkem("tls-only"), None, "无 public-key 的不是 REALITY 节点");
        assert!(opts("plain-ss").is_none(), "无 reality-opts 的节点不动");
        assert_eq!(
            opts("weird"),
            Some(Value::from("")),
            "reality-opts 不是映射时不动"
        );

        let root: Mapping = serde_yaml::from_str(&output.yaml).unwrap();
        let variant = root.get("proxies").unwrap().as_sequence().unwrap()[3]
            .get("reality_opts")
            .unwrap()
            .clone();
        assert_eq!(
            variant,
            serde_yaml::from_str::<Value>("{ Public-Key: pk, support_x25519mlkem768: false }")
                .unwrap(),
            "变体键上的显式值不被规范键覆盖"
        );
    }

    #[test]
    fn reality_mlkem_off_leaves_unset_nodes_alone() {
        // 订阅关闭自动开启时不补键;显式开启的节点仍按指纹报出。
        let mut inp = input("MATCH,DIRECT", vec![], vec![]);
        inp.reality_mlkem = false;
        inp.provider_yaml = "proxies:
  - { name: unset, type: vless, server: 1.2.3.4, port: 443, tls: true, client-fingerprint: firefox, reality-opts: { public-key: pk } }
  - { name: forced-on, type: vless, server: 1.2.3.5, port: 443, tls: true, client-fingerprint: firefox, reality-opts: { public-key: pk, support-x25519mlkem768: true } }
";
        let output = convert(inp).expect("conversion succeeds");
        assert_eq!(
            output.reality_fingerprint_risks,
            vec!["forced-on".to_string()]
        );
        assert_eq!(
            reality_opts_of(&output, "unset").unwrap().get(MLKEM_KEY),
            None
        );
    }
}
