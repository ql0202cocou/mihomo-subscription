// 自定义代理组选项的结构化编辑器 schema。组的选项(url、interval、strategy…)按组
// 类型用带类型的输入框编辑,其余一律落到高级键值行——无需手写 JSON。

import type { FieldDef } from "../../components/nodeSchema";
import type { GroupType } from "../../types";

export const GROUP_TYPES: GroupType[] = [
  "select",
  "url-test",
  "fallback",
  "load-balance",
  "relay",
];

const HEALTH: FieldDef[] = [
  { key: "url", kind: "text", placeholder: "https://www.gstatic.com/generate_204" },
  { key: "interval", kind: "number", placeholder: "300" },
];

/** 各类型专属的常用选项字段;其余一律落到高级键值。 */
export const GROUP_OPTION_FIELDS: Record<string, FieldDef[]> = {
  select: [],
  "url-test": [
    ...HEALTH,
    { key: "tolerance", kind: "number" },
    { key: "lazy", kind: "switch" },
  ],
  fallback: [...HEALTH, { key: "lazy", kind: "switch" }],
  "load-balance": [
    ...HEALTH,
    {
      key: "strategy",
      kind: "select",
      options: ["consistent-hashing", "round-robin", "sticky-sessions"],
    },
  ],
  relay: [],
};

export function groupOptionFields(type: string): FieldDef[] {
  return GROUP_OPTION_FIELDS[type] ?? [];
}

export function groupOptionKeys(type: string): Set<string> {
  return new Set(groupOptionFields(type).map((f) => f.key));
}

/** 始终可作为组成员的内置策略目标。 */
export const BUILTIN_POLICIES = [
  "DIRECT",
  "REJECT",
  "REJECT-DROP",
  "PASS",
  "COMPATIBLE",
  "GLOBAL",
];
