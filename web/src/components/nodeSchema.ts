// 自定义代理节点的结构化编辑器 schema。每个节点存储的 `content` 是一份完整的
// Mihomo 代理 YAML 映射;编辑器按类型把常用字段暴露为带类型的输入框(含 REALITY /
// ws-opts / grpc-opts 等嵌套选项块),其余 key 一律用高级键值行编辑,这样常规场景下
// 管理员永远不必手写 YAML。

export type FieldKind = "text" | "password" | "number" | "switch" | "select" | "tags";

export interface FieldDef {
  key: string;
  kind: FieldKind;
  /** `select`/`tags` 字段的候选建议(仍允许自由输入)。 */
  options?: string[];
  placeholder?: string;
  /** 仅当谓词对当前字段成立时才显示该字段。 */
  showWhen?: (fields: Record<string, unknown>) => boolean;
}

/** 一个嵌套选项对象(如 `reality-opts`),作为带标题的子区块编辑。 */
export interface GroupDef {
  /** 持有该嵌套对象的代理 key。 */
  key: string;
  fields: FieldDef[];
  showWhen?: (fields: Record<string, unknown>) => boolean;
}

interface TypeSchema {
  fields: FieldDef[];
  groups?: GroupDef[];
}

// ─── 条件显示用的谓词 ─────────────────────────────────────────────────────────
const tlsOn = (f: Record<string, unknown>) => f.tls === true;
const networkIs =
  (...nets: string[]) =>
  (f: Record<string, unknown>) =>
    nets.includes(String(f.network ?? "tcp"));

/** 类型选择器里作为建议的代理类型(允许自由输入)。 */
export const NODE_TYPES = [
  "ss",
  "ssr",
  "vmess",
  "vless",
  "trojan",
  "hysteria2",
  "hysteria",
  "tuic",
  "wireguard",
  "http",
  "socks5",
  "snell",
];

/** 协议类型的全称展示名,仅用于下拉标签;保存值仍是上面的字面量 key(Mihomo 必需)。 */
export const NODE_TYPE_LABELS: Record<string, string> = {
  ss: "Shadowsocks",
  ssr: "ShadowsocksR",
  vmess: "VMess",
  vless: "VLESS",
  trojan: "Trojan",
  hysteria2: "Hysteria2",
  hysteria: "Hysteria",
  tuic: "TUIC",
  wireguard: "WireGuard",
  http: "HTTP",
  socks5: "SOCKS5",
  snell: "Snell",
};

/** 所有类型都显示的字段,排在类型专属字段之前。 */
export const BASE_FIELDS: FieldDef[] = [
  { key: "server", kind: "text", placeholder: "example.com / 1.2.3.4" },
  { key: "port", kind: "number" },
];

const SS_CIPHERS = [
  "aes-128-gcm",
  "aes-256-gcm",
  "chacha20-ietf-poly1305",
  "2022-blake3-aes-256-gcm",
  "none",
];
const VMESS_CIPHERS = ["auto", "none", "zero", "aes-128-gcm", "chacha20-poly1305"];
const NETWORKS = ["tcp", "ws", "grpc", "h2", "http"];
const FINGERPRINTS = ["chrome", "firefox", "safari", "ios", "android", "edge", "random"];
const ALPN = ["h2", "http/1.1", "h3"];

// vmess/vless/trojan 共用的传输层选项块。
const WS_OPTS: GroupDef = {
  key: "ws-opts",
  showWhen: networkIs("ws"),
  fields: [
    { key: "path", kind: "text", placeholder: "/" },
    { key: "headers.Host", kind: "text" },
  ],
};
const GRPC_OPTS: GroupDef = {
  key: "grpc-opts",
  showWhen: networkIs("grpc"),
  fields: [{ key: "grpc-service-name", kind: "text" }],
};
// REALITY:基于服务端提供的 public-key + short-id 的 TLS。
const REALITY_OPTS: GroupDef = {
  key: "reality-opts",
  showWhen: tlsOn,
  fields: [
    { key: "public-key", kind: "text" },
    { key: "short-id", kind: "text" },
  ],
};

/** 各类型专属的常用字段 + 嵌套分组。其余一律落到高级键值。 */
const TYPE_SCHEMA: Record<string, TypeSchema> = {
  ss: {
    fields: [
      { key: "cipher", kind: "select", options: SS_CIPHERS },
      { key: "password", kind: "password" },
      { key: "udp", kind: "switch" },
    ],
  },
  vmess: {
    fields: [
      { key: "uuid", kind: "text" },
      { key: "alterId", kind: "number" },
      { key: "cipher", kind: "select", options: VMESS_CIPHERS },
      { key: "udp", kind: "switch" },
      { key: "network", kind: "select", options: NETWORKS },
      { key: "tls", kind: "switch" },
      { key: "servername", kind: "text", showWhen: tlsOn },
      { key: "alpn", kind: "tags", options: ALPN, showWhen: tlsOn },
      { key: "skip-cert-verify", kind: "switch", showWhen: tlsOn },
      { key: "client-fingerprint", kind: "select", options: FINGERPRINTS, showWhen: tlsOn },
    ],
    groups: [WS_OPTS, GRPC_OPTS],
  },
  vless: {
    fields: [
      { key: "uuid", kind: "text" },
      { key: "flow", kind: "select", options: ["xtls-rprx-vision"] },
      { key: "udp", kind: "switch" },
      { key: "network", kind: "select", options: NETWORKS },
      { key: "tls", kind: "switch" },
      { key: "servername", kind: "text", showWhen: tlsOn },
      { key: "alpn", kind: "tags", options: ALPN, showWhen: tlsOn },
      { key: "skip-cert-verify", kind: "switch", showWhen: tlsOn },
      { key: "client-fingerprint", kind: "select", options: FINGERPRINTS, showWhen: tlsOn },
    ],
    // 开启 TLS 时显示 REALITY;ws/grpc 按 network 显示。
    groups: [REALITY_OPTS, WS_OPTS, GRPC_OPTS],
  },
  trojan: {
    fields: [
      { key: "password", kind: "password" },
      { key: "sni", kind: "text" },
      { key: "alpn", kind: "tags", options: ALPN },
      { key: "skip-cert-verify", kind: "switch" },
      { key: "client-fingerprint", kind: "select", options: FINGERPRINTS },
      { key: "network", kind: "select", options: NETWORKS },
    ],
    groups: [WS_OPTS, GRPC_OPTS],
  },
  hysteria2: {
    fields: [
      { key: "password", kind: "password" },
      { key: "sni", kind: "text" },
      { key: "skip-cert-verify", kind: "switch" },
      { key: "up", kind: "text", placeholder: "30 Mbps" },
      { key: "down", kind: "text", placeholder: "200 Mbps" },
      { key: "obfs", kind: "select", options: ["salamander"] },
      { key: "obfs-password", kind: "password" },
      { key: "alpn", kind: "tags", options: ALPN },
    ],
  },
  tuic: {
    fields: [
      { key: "uuid", kind: "text" },
      { key: "password", kind: "password" },
      { key: "sni", kind: "text" },
      { key: "alpn", kind: "tags", options: ALPN },
      { key: "skip-cert-verify", kind: "switch" },
      { key: "congestion-controller", kind: "select", options: ["bbr", "cubic", "new_reno"] },
    ],
  },
  http: {
    fields: [
      { key: "username", kind: "text" },
      { key: "password", kind: "password" },
      { key: "tls", kind: "switch" },
      { key: "skip-cert-verify", kind: "switch", showWhen: tlsOn },
    ],
  },
  socks5: {
    fields: [
      { key: "username", kind: "text" },
      { key: "password", kind: "password" },
      { key: "tls", kind: "switch" },
      { key: "skip-cert-verify", kind: "switch", showWhen: tlsOn },
    ],
  },
};

/** 给定代理类型的常用字段(基础 + 类型专属)。 */
export function commonFields(type: string): FieldDef[] {
  return [...BASE_FIELDS, ...(TYPE_SCHEMA[type]?.fields ?? [])];
}

/** 给定代理类型的嵌套选项分组(reality-opts、ws-opts…)。 */
export function groupsFor(type: string): GroupDef[] {
  return TYPE_SCHEMA[type]?.groups ?? [];
}

/** 由专属输入框/分组管理的 key(高级键值据此排除它们)。 */
export function commonKeys(type: string): Set<string> {
  return new Set([
    "name",
    "type",
    ...commonFields(type).map((f) => f.key),
    ...groupsFor(type).map((g) => g.key),
  ]);
}
