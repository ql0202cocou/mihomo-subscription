import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  App as AntdApp,
  AutoComplete,
  Button,
  Checkbox,
  Input,
  InputNumber,
  Modal,
  Popconfirm,
  Segmented,
  Select,
  Switch,
  Typography,
} from "antd";
import {
  ArrowRightOutlined,
  DeleteOutlined,
  DownOutlined,
  EditOutlined,
  HolderOutlined,
  LockOutlined,
  PlusOutlined,
} from "@ant-design/icons";
import {
  DndContext,
  PointerSensor,
  closestCenter,
  useSensor,
  useSensors,
  type DragEndEvent,
} from "@dnd-kit/core";
import {
  SortableContext,
  arrayMove,
  useSortable,
  verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { useTranslation } from "react-i18next";
import { api, errorMessage } from "../../api";
import type {
  CustomGroup,
  CustomNode,
  ProfileRuleSet,
  ProviderRulesResponse,
  ProxiesResponse,
  Regenerate,
  RuleSet,
} from "../../types";
import { BUILTIN_POLICIES } from "./groupSchema";
import { TypeChips } from "../../components/fields";
import { useRegenerateNotice } from "../../components/regenerate";
import { useSerialSave } from "../../components/useSerialSave";
import {
  RULE_SET_BEHAVIORS as RS_BEHAVIORS,
  RULE_SET_MANUAL_FORMATS as RS_MANUAL_FORMATS,
  RULE_SET_REMOTE_FORMATS as RS_REMOTE_FORMATS,
  RULE_SET_SOURCES as RS_SOURCES,
} from "../../components/ruleSetConstants";

interface Props {
  profileId: string;
  initial: string;
  nodes: CustomNode[];
  groups: CustomGroup[];
  generatedAt: string | null;
  errors: string[];
  onSaved: () => void;
  /** 保存带回的离线重生成结果,交给父组件展示校验错误。 */
  onRegenerate: (r: Regenerate) => void;
}

// 全部 Mihomo 规则类型,按分类分组以便选择器可浏览;仍允许自由输入未列出的类型。
const RULE_TYPE_GROUPS: { key: string; types: string[] }[] = [
  { key: "domain", types: ["DOMAIN", "DOMAIN-SUFFIX", "DOMAIN-KEYWORD", "DOMAIN-REGEX", "GEOSITE"] },
  {
    key: "ip",
    types: ["IP-CIDR", "IP-CIDR6", "IP-SUFFIX", "IP-ASN", "GEOIP", "SRC-GEOIP", "SRC-IP-ASN", "SRC-IP-CIDR", "SRC-IP-SUFFIX"],
  },
  { key: "port", types: ["DST-PORT", "SRC-PORT", "IN-PORT"] },
  { key: "process", types: ["PROCESS-NAME", "PROCESS-PATH", "PROCESS-NAME-REGEX", "PROCESS-PATH-REGEX"] },
  { key: "inbound", types: ["IN-TYPE", "IN-USER", "IN-NAME", "UID", "NETWORK", "DSCP", "RULE-SET"] },
  { key: "logical", types: ["AND", "OR", "NOT", "SUB-RULE"] },
  { key: "fallback", types: ["MATCH"] },
];

// 全部已知规则类型集合。用于:输入恰为某完整类型(默认值/已选值)时不收窄下拉,以便浏览切换。
const ALL_RULE_TYPES = new Set(RULE_TYPE_GROUPS.flatMap((g) => g.types));

const NETWORK_OPTIONS = ["tcp", "udp"];
const IN_TYPE_OPTIONS = ["HTTP", "HTTPS", "SOCKS", "SOCKS4", "SOCKS5", "MIXED", "REDIR", "TPROXY", "TUN"];
const LOGICAL_TYPES = new Set(["AND", "OR", "NOT", "SUB-RULE"]);
const RULE_EXAMPLES: Record<string, string> = {
  DOMAIN: "example.com",
  "DOMAIN-SUFFIX": "example.com",
  "DOMAIN-KEYWORD": "google",
  "DOMAIN-REGEX": "^.*\\.example\\.com$",
  GEOSITE: "youtube",
  "IP-CIDR": "192.168.0.0/16",
  "IP-CIDR6": "2620:0:2d0:200::7/32",
  "IP-SUFFIX": "8.8.8.8/24",
  "IP-ASN": "13335",
  GEOIP: "CN",
  "DST-PORT": "443",
  "SRC-PORT": "8080",
  "IN-PORT": "7890",
  "PROCESS-NAME": "curl",
  "PROCESS-PATH": "/usr/bin/curl",
  "IN-TYPE": "SOCKS5",
  UID: "1000",
  AND: "(DOMAIN,example.com),(NETWORK,tcp)",
  OR: "(NETWORK,udp),(DST-PORT,443)",
  NOT: "(DOMAIN,example.com)",
};

interface RuleModel {
  type: string;
  payload: string;
  policy: string;
  noResolve: boolean;
  // RULE-SET 内联 provider 定义(仅 type=RULE-SET 时有意义;payload 即规则集名)。
  rsBehavior: "domain" | "ipcidr" | "classical";
  rsFormat: "yaml" | "text" | "mrs";
  rsSource: "manual" | "remote";
  rsContent: string;
  rsUrl: string;
  rsInterval: number;
}

const RS_DEFAULTS = {
  rsBehavior: "domain",
  rsFormat: "yaml",
  rsSource: "manual",
  rsContent: "",
  rsUrl: "",
  rsInterval: 24,
} as const;

const DEFAULT_RULE: RuleModel = {
  type: "DOMAIN-SUFFIX",
  payload: "",
  policy: "",
  noResolve: false,
  ...RS_DEFAULTS,
};

const isComment = (line: string) => line.startsWith("#");
const isMatchLine = (line: string) =>
  !isComment(line) && parseRule(line).type.trim().toUpperCase() === "MATCH";

// 解析一行 Mihomo 规则文本。payload 本身可含逗号(如逻辑规则 "(A,x),(B,y)"),所以
// 从尾部取:先剥掉结尾的 no-resolve,再取最后一项为 policy,其余整体作为 payload。
function parseRule(line: string): RuleModel {
  const parts = line.split(",").map((p) => p.trim());
  const type = parts[0] ?? "";
  if (type.toUpperCase() === "MATCH") {
    return { type, payload: "", policy: parts[1] ?? "", noResolve: false, ...RS_DEFAULTS };
  }
  let rest = parts.slice(1);
  let noResolve = false;
  if (rest.length > 0 && rest[rest.length - 1] === "no-resolve") {
    noResolve = true;
    rest = rest.slice(0, -1);
  }
  const policy = rest.length > 0 ? rest[rest.length - 1] : "";
  const payload = rest.slice(0, -1).join(",");
  return { type, payload, policy, noResolve, ...RS_DEFAULTS };
}

function serializeRule(r: RuleModel): string {
  if (r.type.toUpperCase() === "MATCH") return `MATCH,${r.policy}`;
  const base = `${r.type},${r.payload},${r.policy}`;
  return r.noResolve ? `${base},no-resolve` : base;
}

// 编辑目标:null = 新增;数字 = 编辑该条非 MATCH 规则;"match" = 编辑钉底的 MATCH。
type EditTarget = number | "match" | null;

export default function RulesCard({
  profileId,
  initial,
  nodes,
  groups,
  generatedAt,
  errors,
  onSaved,
  onRegenerate,
}: Props) {
  const { t } = useTranslation();
  const { message } = AntdApp.useApp();
  const saveInOrder = useSerialSave();
  const notice = useRegenerateNotice();
  // MATCH 钉在底部(锁定)。`rules` 按序保存其余所有行;`matchLine` 保存唯一的 MATCH 兜底
  // (或 null)。
  const [rules, setRules] = useState<string[]>([]);
  const [matchLine, setMatchLine] = useState<string | null>(null);
  const [modalOpen, setModalOpen] = useState(false);
  const [editing, setEditing] = useState<EditTarget>(null);
  const [model, setModel] = useState<RuleModel>(DEFAULT_RULE);
  // 策略候选拆成两类(输出中的代理 / 输出中的分组——生成时机场分组已被自定义分组整体替换),
  // 以便在编辑弹窗按设计稿分组展示。
  const [proxyNames, setProxyNames] = useState<string[]>([]);
  const [outputGroupNames, setOutputGroupNames] = useState<string[]>([]);
  const [ruleSets, setRuleSets] = useState<RuleSet[]>([]);
  // 本订阅自有规则集(③):RULE-SET 编辑器据此预填,保存时 upsert。
  const [profileRuleSets, setProfileRuleSets] = useState<ProfileRuleSet[]>([]);
  const [importing, setImporting] = useState(false);
  // 「导入托管规则」弹窗:勾选全局规则集 → 以 RULE-SET 引用进本订阅。
  const [importHostedOpen, setImportHostedOpen] = useState(false);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [importPolicy, setImportPolicy] = useState("DIRECT");

  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 4 } }));

  // 服务端最近确认的规则(加载或保存成功时更新):保存失败时回滚到它。不用闭包里的 `rules`,
  // 因为 persist 不依赖它(可能已过期),且连续保存时应回到服务端实际持有的状态。
  const saved = useRef<{ rules: string[]; match: string | null }>({ rules: [], match: null });

  useEffect(() => {
    const all = initial.split("\n").map((l) => l.trim()).filter((l) => l !== "");
    // 最后一条 MATCH 作为兜底,钉底保留;其余行原样留在列表。
    let match: string | null = null;
    const rest: string[] = [];
    for (const line of all) {
      if (isMatchLine(line)) match = line;
      else rest.push(line);
    }
    // 本组件保存后 reload 带回的正是刚保存的规则:列表未变、编辑下标仍有效,不打断正在编辑的弹窗。
    const echoed =
      match === saved.current.match && rest.join("\n") === saved.current.rules.join("\n");
    setRules(rest);
    setMatchLine(match);
    saved.current = { rules: rest, match };
    if (echoed) return;
    setModalOpen(false);
    setEditing(null);
    setModel(DEFAULT_RULE);
  }, [initial]);

  const loadPolicies = useCallback(async () => {
    try {
      const res = await api<ProxiesResponse>(`/api/profiles/${profileId}/proxies`);
      setProxyNames(res.proxies.map((p) => p.name));
      setOutputGroupNames(res.groups.map((g) => g.name));
    } catch {
      // 策略仍可手动输入
    }
  }, [profileId]);

  // 全局 ② 库(供「导入托管规则」勾选)。
  const loadRuleSets = useCallback(async () => {
    try {
      setRuleSets(await api<RuleSet[]>("/api/rule-sets"));
    } catch {
      // 取不到全局库不影响其它操作
    }
  }, []);

  // 本订阅 ③ 库(供 RULE-SET 编辑器预填)。
  const loadProfileRuleSets = useCallback(async () => {
    try {
      setProfileRuleSets(await api<ProfileRuleSet[]>(`/api/profiles/${profileId}/rule-sets`));
    } catch {
      // 取不到不影响:保存时按是否存在决定 POST/PUT
    }
  }, [profileId]);

  // 全局 ② 库与 generatedAt 无关,单独加载,避免每次生成都重拉。
  useEffect(() => {
    void loadRuleSets();
  }, [loadRuleSets]);

  // 策略候选(机场代理随生成变化)与 ③ 库随 generatedAt 刷新。
  useEffect(() => {
    void loadPolicies();
    void loadProfileRuleSets();
  }, [loadPolicies, loadProfileRuleSets, generatedAt]);

  // 保存规则 + 钉底的 MATCH(始终在最后)。返回是否成功,供调用方决定后续提示。`appliedText` 为
  // 「已应用到订阅」的成功文案(见 useRegenerateNotice)。
  const persist = useCallback(
    async (nextRules: string[], nextMatch: string | null, appliedText?: string): Promise<boolean> => {
      setRules(nextRules);
      setMatchLine(nextMatch);
      const content = [...nextRules, ...(nextMatch ? [nextMatch] : [])].join("\n");
      const res = await saveInOrder(() =>
        api<{ regenerate: Regenerate }>(`/api/profiles/${profileId}/rules`, {
          method: "PUT",
          body: JSON.stringify({ content }),
        }),
      );
      // 保存串行生效,成功即服务端当前持有的规则。
      if (res.ok) saved.current = { rules: nextRules, match: nextMatch };
      // 之后又有保存:它提交的是完整规则,界面(提示、回滚、reload)交给最后一次决定。
      if (!res.latest) return res.ok;
      if (res.ok) {
        notice(res.value.regenerate, appliedText);
        onRegenerate(res.value.regenerate);
        onSaved();
      } else {
        setRules(saved.current.rules);
        setMatchLine(saved.current.match);
        message.error(errorMessage(res.error, t("common.saveFailed")));
      }
      return res.ok;
    },
    [profileId, onSaved, onRegenerate, saveInOrder, notice, message, t],
  );

  // DnD id / React key 用内容派生的稳定 id(不用数组下标);重复行按出现次序加后缀保证唯一。
  const ruleIds = useMemo(() => {
    const seen = new Map<string, number>();
    return rules.map((line) => {
      const n = seen.get(line) ?? 0;
      seen.set(line, n + 1);
      return n === 0 ? line : `${line}#${n}`;
    });
  }, [rules]);

  async function onDragEnd(event: DragEndEvent) {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const oldIndex = ruleIds.indexOf(String(active.id));
    const newIndex = ruleIds.indexOf(String(over.id));
    if (oldIndex < 0 || newIndex < 0) return;
    const next = arrayMove(rules, oldIndex, newIndex);
    // 提示由 persist 按保存与离线重生成结果给出。
    await persist(next, matchLine, t("rules.orderSaved"));
  }

  function openEdit(index: number) {
    const m = parseRule(rules[index]);
    // RULE-SET:用本订阅已有的 ③ 定义预填(远程 URL 已脱敏不回显,留空表示保持不变)。
    if (m.type.trim().toUpperCase() === "RULE-SET") {
      const rs = profileRuleSets.find((r) => r.name === m.payload.trim());
      if (rs) {
        m.rsBehavior = rs.behavior;
        m.rsFormat = rs.format;
        m.rsSource = rs.source;
        m.rsContent = rs.content;
        m.rsUrl = "";
        m.rsInterval = rs.interval_hours;
      }
    }
    setEditing(index);
    setModel(m);
    setModalOpen(true);
  }
  function openAdd() {
    setEditing(null);
    setModel(DEFAULT_RULE);
    setModalOpen(true);
  }
  function openEditMatch() {
    setEditing("match");
    setModel(matchLine ? parseRule(matchLine) : { ...DEFAULT_RULE, type: "MATCH" });
    setModalOpen(true);
  }
  function closeModal() {
    setModalOpen(false);
    setEditing(null);
    setModel(DEFAULT_RULE);
  }

  // 把 RULE-SET 的内联 provider 定义 upsert 到本订阅 ③ 库(按名 POST 新建 / PUT 更新)。
  // 表单不编辑 cache / enabled:更新时沿用已有值(PUT 整体替换,缺省会被后端当作 true),新建时启用并缓存。
  async function upsertRuleSet(m: RuleModel) {
    const name = m.payload.trim();
    const existing = profileRuleSets.find((r) => r.name === name);
    const base = {
      name,
      behavior: m.rsBehavior,
      format: m.rsFormat,
      source: m.rsSource,
      enabled: existing?.enabled ?? true,
    };
    const body =
      m.rsSource === "remote"
        ? {
            ...base,
            interval_hours: m.rsInterval,
            cache: existing?.cache ?? true,
            // 远程 URL 已脱敏不回显:留空时不传 url 字段,后端保留原 URL(见 submit 的校验)。
            ...(m.rsUrl.trim() ? { url: m.rsUrl.trim() } : {}),
          }
        : { ...base, content: m.rsContent };
    if (existing) {
      await api(`/api/profiles/${profileId}/rule-sets/${existing.id}`, {
        method: "PUT",
        body: JSON.stringify(body),
      });
    } else {
      await api(`/api/profiles/${profileId}/rule-sets`, {
        method: "POST",
        body: JSON.stringify(body),
      });
    }
  }

  async function submit() {
    const upperType = model.type.trim().toUpperCase();
    const isMatch = upperType === "MATCH";
    const isRuleSet = upperType === "RULE-SET";
    if (!model.type.trim() || !model.policy.trim() || (!isMatch && !model.payload.trim())) {
      message.error(t("rules.incomplete"));
      return;
    }
    // RULE-SET:新建远程定义必须给 URL(编辑保留可留空);保存前先 upsert ③ 定义。
    if (isRuleSet) {
      const existing = profileRuleSets.find((r) => r.name === model.payload.trim());
      if (model.rsSource === "remote" && !model.rsUrl.trim() && !existing) {
        message.error(t("rules.ruleSetUrlRequired"));
        return;
      }
      try {
        await upsertRuleSet(model);
        await loadProfileRuleSets();
      } catch (e) {
        message.error(errorMessage(e, t("common.saveFailed")));
        return;
      }
    }
    const line = serializeRule(model);
    closeModal();
    if (isMatch) {
      // 任何 MATCH(新增或编辑)都成为唯一的钉底兜底。
      void persist(rules, line);
      return;
    }
    if (editing === "match") {
      // 在编辑钉底行时把类型改成了非 MATCH:将其落入列表,并清空钉底的 MATCH。
      void persist([...rules, line], null);
      return;
    }
    if (editing === null) {
      // 「添加规则」:新非 MATCH 规则追加到列表末尾(MATCH 仍钉底)。
      void persist([...rules, line], matchLine);
      return;
    }
    // 走到这里 editing 为被编辑行的下标。
    const next = rules.map((l, i) => (i === editing ? line : l));
    void persist(next, matchLine);
  }

  function remove(index: number) {
    void persist(rules.filter((_, i) => i !== index), matchLine);
  }

  async function importProviderRules() {
    setImporting(true);
    try {
      const res = await api<ProviderRulesResponse>(`/api/profiles/${profileId}/provider-rules`);
      const existing = new Set([...rules, ...(matchLine ? [matchLine] : [])]);
      // 丢弃机场规则里的 MATCH(兜底统一由钉底的 matchLine 管理)与本地已有的重复行。
      const incoming = res.rules
        .map((l) => l.trim())
        .filter((l) => l !== "" && !isMatchLine(l) && !existing.has(l));
      if (incoming.length === 0) {
        message.info(t("rules.importNone"));
        return;
      }
      if (await persist([...rules, ...incoming], matchLine)) {
        message.success(t("rules.imported", { count: incoming.length }));
      }
    } catch (e) {
      message.error(errorMessage(e, t("rules.importFailed")));
    } finally {
      setImporting(false);
    }
  }

  // 按设计稿分组的策略候选(规则编辑弹窗的策略下拉用):节点 / 代理分组 / 内置策略。
  const policyGrouped = useMemo(() => {
    const uniq = (xs: string[]) => Array.from(new Set(xs.filter((s) => s.trim() !== "")));
    const nodeNames = uniq([...proxyNames, ...nodes.map((n) => n.name)]);
    const groupNames = uniq([...groups.map((g) => g.name), ...outputGroupNames]);
    return [
      { label: t("rules.policyGroups.nodes"), options: nodeNames.map((value) => ({ value })) },
      { label: t("rules.policyGroups.groups"), options: groupNames.map((value) => ({ value })) },
      { label: t("rules.policyGroups.builtin"), options: BUILTIN_POLICIES.map((value) => ({ value })) },
    ].filter((g) => g.options.length > 0);
  }, [proxyNames, outputGroupNames, nodes, groups, t]);

  // 扁平去重的策略名(「导入托管规则」的引用策略下拉用)——由分组候选展平,单一来源。
  const policyFlat = useMemo(() => {
    const seen = new Set<string>();
    const out: { value: string }[] = [];
    for (const g of policyGrouped) {
      for (const o of g.options) {
        if (!seen.has(o.value)) {
          seen.add(o.value);
          out.push(o);
        }
      }
    }
    return out;
  }, [policyGrouped]);

  // 当前规则里已被 RULE-SET 引用的规则集名(导入弹窗据此标记「已引用」)。
  const referenced = useMemo(() => {
    const set = new Set<string>();
    for (const line of rules) {
      const parts = line.split(",").map((p) => p.trim());
      if (parts[0]?.toUpperCase() === "RULE-SET" && parts[1]) set.add(parts[1]);
    }
    return set;
  }, [rules]);

  function openImportHosted() {
    setPicked(new Set());
    setImportPolicy("DIRECT");
    setImportHostedOpen(true);
  }

  function togglePick(name: string, checked: boolean) {
    setPicked((prev) => {
      const next = new Set(prev);
      if (checked) next.add(name);
      else next.delete(name);
      return next;
    });
  }

  async function doImportHosted() {
    const names = ruleSets
      .map((r) => r.name)
      .filter((n) => picked.has(n) && !referenced.has(n));
    setImportHostedOpen(false);
    if (names.length === 0) {
      message.info(t("rules.importNone"));
      return;
    }
    try {
      // 后端把 ② 定义复制进本订阅 ③(含真实远程 URL)并追加 RULE-SET 规则行,随后离线重生成缓存。
      const res = await api<{ imported: number; regenerate: Regenerate | null }>(
        `/api/profiles/${profileId}/rule-sets/import`,
        { method: "POST", body: JSON.stringify({ names, policy: importPolicy }) },
      );
      await loadProfileRuleSets();
      onSaved();
      message.success(t("rules.importHostedDone", { count: res.imported }));
      notice(res.regenerate);
      if (res.regenerate) onRegenerate(res.regenerate);
    } catch (e) {
      message.error(errorMessage(e, t("rules.importFailed")));
    }
  }

  // 生成错误里与本卡片相关的行(后端 `src/converter.rs` 的校验错误以 "rules line" 开头)。
  const ruleErrors = errors.filter((e) => /rules line/.test(e));
  const total = rules.length + (matchLine ? 1 : 0);

  return (
    <div className="dcard">
      <div className="dcard-head">
        <span className="dcard-title">
          {t("rules.title")} <span className="row-sub">{t("rules.count", { count: total })}</span>
        </span>
        <div className="dcard-actions">
          <Button onClick={openImportHosted}>{t("rules.importHosted")}</Button>
          <Popconfirm title={t("rules.importConfirm")} onConfirm={importProviderRules}>
            <Button loading={importing}>{t("rules.importProvider")}</Button>
          </Popconfirm>
          <Button type="primary" icon={<PlusOutlined />} onClick={openAdd}>
            {t("rules.add")}
          </Button>
        </div>
      </div>

      {ruleErrors.length > 0 && (
        <div className="warn-banner">
          {t("rules.invalid")}
          {ruleErrors.map((e, i) => (
            <div key={i}>{e}</div>
          ))}
        </div>
      )}

      {rules.length === 0 && !matchLine ? (
        <div className="empty-line">{t("rules.emptyHint")}</div>
      ) : (
        <>
          <DndContext sensors={sensors} collisionDetection={closestCenter} onDragEnd={onDragEnd}>
            <SortableContext items={ruleIds} strategy={verticalListSortingStrategy}>
              {rules.map((line, index) => (
                <SortableRuleRow
                  key={ruleIds[index]}
                  id={ruleIds[index]}
                  line={line}
                  onEdit={() => openEdit(index)}
                  onRemove={() => remove(index)}
                />
              ))}
            </SortableContext>
          </DndContext>
          {matchLine && <MatchRow line={matchLine} onEdit={openEditMatch} />}
        </>
      )}
      <div className="dcard-note">{t("rules.dragHint")}</div>

      <Modal
        open={modalOpen}
        title={editing === null ? t("rules.add") : t("rules.edit")}
        onOk={submit}
        onCancel={closeModal}
        okText={t("common.save")}
        cancelText={t("common.cancel")}
        destroyOnClose
      >
        <RuleComposer
          model={model}
          onChange={setModel}
          policyOptions={policyGrouped}
        />
      </Modal>

      <Modal
        open={importHostedOpen}
        title={t("rules.importHosted")}
        onOk={doImportHosted}
        onCancel={() => setImportHostedOpen(false)}
        okText={t("common.save")}
        cancelText={t("common.cancel")}
        width={520}
        destroyOnClose
      >
        <p className="rules-import-desc">{t("rules.importHostedDesc")}</p>
        {ruleSets.length === 0 ? (
          <div className="empty-line">{t("rules.importHostedEmpty")}</div>
        ) : (
          <div className="rules-import-list">
            {ruleSets.map((rs) => {
              const already = referenced.has(rs.name);
              return (
                <div key={rs.name} className="rules-import-item">
                  <Checkbox
                    checked={already || picked.has(rs.name)}
                    disabled={already}
                    onChange={(e) => togglePick(rs.name, e.target.checked)}
                    aria-label={rs.name}
                  />
                  <div className="rules-import-main">
                    <div className="rules-import-name">
                      {rs.name}
                      {already && (
                        <span className="rules-import-tag">{t("rules.importHostedAlready")}</span>
                      )}
                    </div>
                    <div className="rules-import-url">
                      {rs.source === "remote"
                        ? (rs.remote_url_masked ?? t("ruleSets.sourceRemote"))
                        : t("ruleSets.sourceManual")}
                    </div>
                  </div>
                  <span className="tag-mono">{rs.behavior}</span>
                  <span className="rules-import-count">{rs.count}</span>
                </div>
              );
            })}
          </div>
        )}
        <div className="rules-import-policy">
          <Typography.Text type="secondary">{t("rules.importHostedPolicy")}</Typography.Text>
          <Select
            style={{ width: "100%" }}
            value={importPolicy}
            onChange={setImportPolicy}
            showSearch
            options={policyFlat.map((o) => ({ value: o.value, label: o.value }))}
          />
          <div className="rules-import-hint">{t("rules.importHostedPolicyHint")}</div>
        </div>
      </Modal>
    </div>
  );
}

function SortableRuleRow({
  id,
  line,
  onEdit,
  onRemove,
}: {
  id: string;
  line: string;
  onEdit: () => void;
  onRemove: () => void;
}) {
  const { t } = useTranslation();
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({ id });
  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
    background: isDragging ? "var(--bg-subtle)" : undefined,
  };
  const comment = isComment(line);
  const r = parseRule(line);
  return (
    <div className="row" ref={setNodeRef} style={style}>
      <span className="row-grab" {...attributes} {...listeners} aria-label={t("common.drag")}>
        <HolderOutlined />
      </span>
      {comment ? (
        <span className="rule-content" style={{ color: "var(--text-4)" }}>
          {line}
        </span>
      ) : (
        <>
          <span className="tag-mono tag-type">{r.type}</span>
          <span className="rule-content">{r.payload}</span>
          {r.noResolve && <span className="tag-mono">no-resolve</span>}
          <ArrowRightOutlined className="rule-arrow" style={{ fontSize: 11 }} />
          <span className="tag-mono tag-policy">{r.policy}</span>
        </>
      )}
      <span className="row-actions">
        {!comment && (
          <button className="icon-btn" onClick={onEdit} aria-label={t("basic.edit")}>
            <EditOutlined />
          </button>
        )}
        <Popconfirm title={t("rules.deleteConfirm")} onConfirm={onRemove}>
          <button className="icon-btn danger" aria-label={t("rules.delete")}>
            <DeleteOutlined />
          </button>
        </Popconfirm>
      </span>
    </div>
  );
}

// 钉底的兜底行:锁在底部,仅可编辑(不可拖拽、不可删除)。
function MatchRow({ line, onEdit }: { line: string; onEdit: () => void }) {
  const { t } = useTranslation();
  const r = parseRule(line);
  return (
    <div className="row row-match">
      <span className="row-lock">
        <LockOutlined />
      </span>
      <span className="tag-mono tag-type">MATCH</span>
      <span className="rule-content">{t("rules.fallbackNote")}</span>
      <ArrowRightOutlined className="rule-arrow" style={{ fontSize: 11 }} />
      <span className="tag-mono tag-policy">{r.policy}</span>
      <span className="row-actions">
        <button className="icon-btn" onClick={onEdit} aria-label={t("basic.edit")}>
          <EditOutlined />
        </button>
        {/* MATCH 钉底不可删除:灰色禁用的删除图标 + 斜杠,与其它行图标列对齐 */}
        <button className="icon-btn icon-slash" disabled aria-label={t("rules.delete")}>
          <DeleteOutlined />
        </button>
      </span>
    </div>
  );
}

interface PolicyGroup {
  label: string;
  options: { value: string }[];
}

interface ComposerProps {
  model: RuleModel;
  onChange: (m: RuleModel) => void;
  policyOptions: PolicyGroup[];
}

function RuleComposer({ model, onChange, policyOptions }: ComposerProps) {
  const { t } = useTranslation();
  const upperType = model.type.trim().toUpperCase();
  const isMatch = upperType === "MATCH";
  const isRuleSet = upperType === "RULE-SET";
  const isLogical = LOGICAL_TYPES.has(upperType);
  // no-resolve 仅 IP 类规则需要;MATCH / RULE-SET 不显示。
  const showNoResolve = !isMatch && !isRuleSet;

  const typeOptions = RULE_TYPE_GROUPS.map((g) => ({
    label: t(`rules.typeGroups.${g.key}`),
    options: g.types.map((v) => ({ value: v })),
  }));

  // 当前类型所属分类(用于「规则类型」旁的徽标);自由输入的未知类型不显示徽标。
  const categoryKey = RULE_TYPE_GROUPS.find((g) => g.types.includes(upperType))?.key;
  const category = categoryKey ? t(`rules.typeGroups.${categoryKey}`) : "";
  const policyValues = new Set(policyOptions.flatMap((g) => g.options.map((o) => o.value)));

  // 按规则类型选择「匹配内容」的输入控件:枚举型给选项,逻辑规则给多行文本,其余单行输入。
  function contentBlock() {
    if (upperType === "NETWORK") {
      return {
        label: t("rules.networkLabel"),
        input: (
          <Segmented
            options={NETWORK_OPTIONS}
            // 已有内容不在枚举内时保持未选中,不静默改写。
            value={NETWORK_OPTIONS.includes(model.payload) ? model.payload : undefined}
            onChange={(payload) => onChange({ ...model, payload: String(payload) })}
          />
        ),
      };
    }
    if (upperType === "IN-TYPE") {
      return {
        label: t("rules.inTypeLabel"),
        input: (
          <AutoComplete
            style={{ width: "100%" }}
            suffixIcon={<DownOutlined />}
            options={IN_TYPE_OPTIONS.map((value) => ({ value }))}
            value={model.payload}
            onChange={(payload) => onChange({ ...model, payload })}
            placeholder="HTTP / SOCKS5 / TUN ..."
            filterOption={(input, opt) =>
              // 同「规则类型」:输入恰为完整选项时不收窄下拉。
              IN_TYPE_OPTIONS.includes(input) ||
              String(opt?.value ?? "").toLowerCase().includes(input.toLowerCase())
            }
          />
        ),
      };
    }
    if (isLogical) {
      return {
        label: t("rules.payload"),
        hint: t("rules.logicalHint"),
        input: (
          <Input.TextArea
            style={{ width: "100%" }}
            autoSize={{ minRows: 2, maxRows: 6 }}
            value={model.payload}
            onChange={(e) => onChange({ ...model, payload: e.target.value })}
            placeholder={RULE_EXAMPLES[upperType] ?? "(DOMAIN,example.com),(NETWORK,tcp)"}
          />
        ),
      };
    }
    return {
      label: t("rules.payload"),
      hint: RULE_EXAMPLES[upperType] ? t("rules.example", { text: RULE_EXAMPLES[upperType] }) : undefined,
      input: (
        <Input
          style={{ width: "100%" }}
          value={model.payload}
          onChange={(e) => onChange({ ...model, payload: e.target.value })}
          placeholder={RULE_EXAMPLES[upperType] ?? t("rules.payload")}
        />
      ),
    };
  }

  const content = contentBlock();

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 14 }}>
      <div>
        <span className="composer-label-row">
          <Typography.Text type="secondary">{t("rules.ruleType")}</Typography.Text>
          {category && <span className="rule-cat-badge">{category}</span>}
        </span>
        <AutoComplete
          style={{ width: "100%" }}
          suffixIcon={<DownOutlined />}
          options={typeOptions}
          value={model.type}
          onChange={(type) => {
            // 切换到已知类型时,用该类型的示例预填「匹配内容」(对齐设计稿);仅当内容为空或仍是
            // 上一个类型的示例时才覆盖,避免清掉手输内容。
            const next: RuleModel = { ...model, type };
            const newU = type.trim().toUpperCase();
            const oldU = model.type.trim().toUpperCase();
            if (newU !== oldU && ALL_RULE_TYPES.has(type)) {
              const prevExample = RULE_EXAMPLES[oldU] ?? "";
              if (model.payload.trim() === "" || model.payload === prevExample) {
                next.payload = RULE_EXAMPLES[newU] ?? "";
              }
            }
            onChange(next);
          }}
          placeholder={t("rules.ruleType")}
          filterOption={(input, opt) => {
            // 输入恰为某个完整类型(默认的 DOMAIN-SUFFIX 或已选值)时不收窄下拉,展示全部以便浏览
            // 切换;否则按子串过滤。不然预填的默认值会把下拉收成只剩它自己。
            if (ALL_RULE_TYPES.has(input)) return true;
            return String((opt as { value?: string })?.value ?? "")
              .toLowerCase()
              .includes(input.toLowerCase());
          }}
        />
      </div>
      {isMatch ? (
        <div className="rule-match-note">{t("rules.matchHint")}</div>
      ) : isRuleSet ? (
        <RuleSetDefBlock model={model} onChange={onChange} />
      ) : (
        <div>
          <Typography.Text type="secondary">{content.label}</Typography.Text>
          {content.input}
          {content.hint && <span className="composer-hint">{content.hint}</span>}
        </div>
      )}
      {showNoResolve && (
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <Switch
            size="small"
            checked={model.noResolve}
            onChange={(noResolve) => onChange({ ...model, noResolve })}
          />
          <Typography.Text type="secondary">{t("rules.noResolve")}</Typography.Text>
        </div>
      )}
      <div>
        <Typography.Text type="secondary">{t("rules.policy")}</Typography.Text>
        <AutoComplete
          style={{ width: "100%" }}
          suffixIcon={<DownOutlined />}
          options={policyOptions}
          value={model.policy}
          onChange={(policy) => onChange({ ...model, policy })}
          placeholder={t("rules.policy")}
          filterOption={(input, opt) =>
            // 输入恰为某个完整策略(编辑时预填的原值)时不收窄下拉,同「规则类型」;否则按子串过滤。
            // 分组选项:仅叶子项带 value;分组标题无 value 不参与。
            policyValues.has(input) ||
            String((opt as { value?: string })?.value ?? "")
              .toLowerCase()
              .includes(input.toLowerCase())
          }
        />
      </div>
    </div>
  );
}

// RULE-SET 内联 rule-provider 定义(设计稿):规则集名 + behavior/format/来源 + 远程 URL/间隔 或
// 手动 payload。保存进本订阅 ③ 库。
function RuleSetDefBlock({
  model,
  onChange,
}: {
  model: RuleModel;
  onChange: (m: RuleModel) => void;
}) {
  const { t } = useTranslation();
  const formats = model.rsSource === "remote" ? RS_REMOTE_FORMATS : RS_MANUAL_FORMATS;
  // 切来源时纠正不兼容 format(手动不支持 mrs)。
  function setSource(rsSource: "manual" | "remote") {
    const rsFormat = rsSource === "manual" && model.rsFormat === "mrs" ? "yaml" : model.rsFormat;
    onChange({ ...model, rsSource, rsFormat });
  }
  return (
    <div className="modal-block">
      <div className="modal-block-title">{t("rules.ruleSetDefTitle")}</div>
      <Typography.Text type="secondary">{t("rules.ruleSetName")}</Typography.Text>
      <Input
        style={{ width: "100%", marginBottom: 12 }}
        value={model.payload}
        onChange={(e) => onChange({ ...model, payload: e.target.value })}
        placeholder={t("rules.ruleSetPayloadHint")}
      />
      <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 12, marginBottom: 12 }}>
        <div>
          <Typography.Text type="secondary">{t("ruleSets.behavior")}</Typography.Text>
          <TypeChips
            options={RS_BEHAVIORS}
            value={model.rsBehavior}
            onChange={(v) => onChange({ ...model, rsBehavior: v as RuleModel["rsBehavior"] })}
          />
        </div>
        <div>
          <Typography.Text type="secondary">{t("ruleSets.format")}</Typography.Text>
          <TypeChips
            options={formats}
            value={model.rsFormat}
            onChange={(v) => onChange({ ...model, rsFormat: v as RuleModel["rsFormat"] })}
          />
        </div>
      </div>
      <Typography.Text type="secondary">{t("ruleSets.source")}</Typography.Text>
      <TypeChips
        options={RS_SOURCES}
        value={model.rsSource}
        onChange={(v) => setSource(v as "manual" | "remote")}
        labels={{ manual: t("ruleSets.sourceManual"), remote: t("ruleSets.sourceRemote") }}
      />
      {model.rsSource === "remote" ? (
        <div style={{ marginTop: 12 }}>
          <Typography.Text type="secondary">{t("ruleSets.remoteUrl")}</Typography.Text>
          <Input
            style={{ width: "100%", fontFamily: "var(--font-mono)" }}
            value={model.rsUrl}
            onChange={(e) => onChange({ ...model, rsUrl: e.target.value })}
            placeholder={t("rules.ruleSetUrlPlaceholder")}
          />
          <div style={{ marginTop: 12, width: 180 }}>
            <Typography.Text type="secondary">{t("rules.ruleSetInterval")}</Typography.Text>
            <InputNumber
              min={1}
              precision={0}
              style={{ width: "100%" }}
              value={model.rsInterval}
              onChange={(v) => onChange({ ...model, rsInterval: v ?? 24 })}
            />
          </div>
          <span className="composer-hint">{t("rules.ruleSetRemoteHint")}</span>
        </div>
      ) : (
        <div style={{ marginTop: 12 }}>
          <Typography.Text type="secondary">{t("ruleSets.content")}</Typography.Text>
          <Input.TextArea
            style={{ width: "100%", fontFamily: "var(--font-mono)" }}
            autoSize={{ minRows: 4, maxRows: 12 }}
            value={model.rsContent}
            onChange={(e) => onChange({ ...model, rsContent: e.target.value })}
            placeholder={t("ruleSets.contentPlaceholder")}
          />
          <span className="composer-hint">{t("rules.ruleSetManualHint")}</span>
        </div>
      )}
    </div>
  );
}
