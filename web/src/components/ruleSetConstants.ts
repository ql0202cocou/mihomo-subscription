// 规则集(② 用户库 / ③ 订阅库)共用的选项常量。与后端 `src/rulelib.rs` 的
// BEHAVIORS / MANUAL_FORMATS / REMOTE_FORMATS 对应(跨语言副本无法避免,TS 侧只保留这一份)。

export const RULE_SET_BEHAVIORS = ["domain", "ipcidr", "classical"] as const;
export const RULE_SET_MANUAL_FORMATS = ["yaml", "text"] as const;
export const RULE_SET_REMOTE_FORMATS = ["yaml", "text", "mrs"] as const;
export const RULE_SET_SOURCES = ["manual", "remote"] as const;

export type RuleSetSource = (typeof RULE_SET_SOURCES)[number];
export type RuleSetFormat = (typeof RULE_SET_REMOTE_FORMATS)[number];

/** 来源与行为下可选的 format:手动来源不支持 mrs,classical 行为也不支持 mrs。 */
export function ruleSetFormatOptions(source: RuleSetSource, behavior: string): readonly RuleSetFormat[] {
  const formats = source === "remote" ? RULE_SET_REMOTE_FORMATS : RULE_SET_MANUAL_FORMATS;
  return formats.filter((format) => behavior !== "classical" || format !== "mrs");
}

/** 切换来源或行为后,不再可选的 mrs 回落为 yaml。 */
export function coerceRuleSetFormat<F extends string>(source: RuleSetSource, behavior: string, format: F): F | "yaml" {
  return format === "mrs" && (source === "manual" || behavior === "classical") ? "yaml" : format;
}
