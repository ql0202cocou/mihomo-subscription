// 规则集(② 用户库 / ③ 订阅库)共用的选项常量。与后端 `src/rulelib.rs` 的
// BEHAVIORS / MANUAL_FORMATS / REMOTE_FORMATS 对应(跨语言副本无法避免,TS 侧只保留这一份)。

export const RULE_SET_BEHAVIORS = ["domain", "ipcidr", "classical"] as const;
export const RULE_SET_MANUAL_FORMATS = ["yaml", "text"] as const;
export const RULE_SET_REMOTE_FORMATS = ["yaml", "text", "mrs"] as const;
export const RULE_SET_SOURCES = ["manual", "remote"] as const;
