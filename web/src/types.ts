export interface ProfileSummary {
  id: string;
  name: string;
  source_url_masked: string;
  output_type: string;
  subscription_url: string;
  last_fetch_at: string | null;
  last_fetch_status: string | null;
  last_generated_at: string | null;
  created_at: string;
  updated_at: string;
}

export type GroupType =
  | "select"
  | "url-test"
  | "fallback"
  | "load-balance"
  | "relay";

/** 订阅的规则内容(③ 规则文本整体)。 */
export interface ProfileRules {
  content: string;
  updated_at: string;
}

/** 规则/排序类保存接口带回的离线重生成结果(`src/generate.rs` 的 `Regenerated`)。 */
export interface Regenerate {
  status: "applied" | "pending" | "invalid";
  /** 仅 `invalid`:与「生成」一致的逐条校验错误。 */
  errors?: string[];
}

export interface ProviderRulesResponse {
  rules: string[];
}

export interface CustomNode {
  id: string;
  name: string;
  node_type: string;
  content: string;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

/** 生成结果中出现的一个代理(来自机场或自定义)。 */
export interface ProxyPreview {
  name: string;
  type: string;
}

export interface ProxiesResponse {
  generated: boolean;
  generated_at: string | null;
  proxies: ProxyPreview[];
  /** 两个节点区块的顺序,如 ["provider","custom"]。 */
  node_section_order: string[];
  /** 最近一次生成结果中的代理分组(名称 + 类型)。生成时机场分组被整体替换,故这些都是自定义分组的快照。 */
  groups: ProxyPreview[];
}

export interface CustomGroup {
  id: string;
  name: string;
  group_type: GroupType;
  members: string[];
  options: Record<string, unknown> | null;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

export interface ProfileDetail extends ProfileSummary {
  rules: ProfileRules | null;
  nodes: CustomNode[];
  groups: CustomGroup[];
}

/** 全局规则集(「规则托管」,② 用户库 / 导入源)。0.4 起 ② 仅作模板:不再公开托管、不参与生成。 */
export interface RuleSet {
  id: string;
  name: string;
  behavior: "domain" | "ipcidr" | "classical";
  format: "yaml" | "text" | "mrs";
  source: "manual" | "remote";
  content: string;
  enabled: boolean;
  /** 规则条数(manual=payload 行数;remote 模板为 0)。 */
  count: number;
  /** 远程来源 URL(已脱敏);manual 为 null。 */
  remote_url_masked: string | null;
  interval_hours: number;
  /** 远程是否本地缓存二次托管(导入到订阅 ③ 后由订阅镜像)。 */
  cache: boolean;
  created_at: string;
  updated_at: string;
}

/** 订阅自有规则集(③ 托管规则库):随订阅自包含,按订阅 token 隔离托管。 */
export interface ProfileRuleSet {
  id: string;
  name: string;
  behavior: "domain" | "ipcidr" | "classical";
  format: "yaml" | "text" | "mrs";
  source: "manual" | "remote";
  content: string;
  enabled: boolean;
  count: number;
  /** 按订阅 token 隔离的面板托管链接。 */
  url: string;
  remote_url_masked: string | null;
  interval_hours: number;
  cache: boolean;
  last_fetch_status: string | null;
  created_at: string;
  updated_at: string;
}

export interface Settings {
  public_path_prefix: string;
}
