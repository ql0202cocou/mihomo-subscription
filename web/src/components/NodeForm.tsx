import { Form, Input, Select } from "antd";
import { useTranslation } from "react-i18next";
import { parse as parseYaml, stringify as stringifyYaml } from "yaml";
import {
  commonFields,
  commonKeys,
  groupsFor,
  NODE_TYPES,
  nodeTypeLabel,
  type FieldDef,
} from "./nodeSchema";
import { AdvancedFields, FieldInput, getPath, isEmptyValue, setPath, advancedEntries } from "./fields";

export interface NodeModel {
  name: string;
  type: string;
  /** 除 `name`/`type` 外的全部代理字段,保持文档顺序。 */
  fields: Record<string, unknown>;
}

/** 把存储的代理 YAML 解析成编辑器模型;容忍非法输入。 */
export function contentToModel(content: string): NodeModel {
  let parsed: Record<string, unknown> = {};
  try {
    const p = parseYaml(content);
    if (p && typeof p === "object" && !Array.isArray(p)) {
      parsed = p as Record<string, unknown>;
    }
  } catch {
    // content 无法解析时按空白节点处理,而不是卡住管理员。
  }
  let name = "";
  let type = "";
  const fields: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(parsed)) {
    if (k === "name") name = typeof v === "string" ? v : String(v ?? "");
    else if (k === "type") type = typeof v === "string" ? v : String(v ?? "");
    else fields[k] = v;
  }
  return { name, type, fields };
}

/** 把编辑器模型序列化回 Mihomo 代理 YAML 映射。 */
export function modelToContent(m: NodeModel): string {
  const obj: Record<string, unknown> = {};
  if (m.name) obj.name = m.name;
  if (m.type) obj.type = m.type;
  for (const [k, v] of Object.entries(m.fields)) {
    if (k.trim() === "" || isEmptyValue(v)) continue;
    obj[k] = v;
  }
  return stringifyYaml(obj);
}

/** 由节点 content 取 `server:port`,经 model 解析(容忍任意 YAML 形态;无 server 时返回空)。
 *  供只读节点行展示使用。 */
export function nodeAddr(content: string): string {
  const { fields } = contentToModel(content);
  const server = fields.server;
  if (server == null || server === "") return "";
  const port = fields.port;
  return port != null && port !== "" ? `${server}:${port}` : String(server);
}

interface Props {
  value: NodeModel;
  onChange: (next: NodeModel) => void;
}

/** 自定义代理节点的结构化编辑器:按类型给出带类型约束的常用字段,其余一律用
 *  高级键值行编辑——无需手写 YAML。 */
export default function NodeForm({ value, onChange }: Props) {
  const { t } = useTranslation();
  const known = commonKeys(value.type);

  function setField(key: string, v: unknown) {
    const fields = { ...value.fields };
    if (isEmptyValue(v)) delete fields[key];
    else fields[key] = v;
    onChange({ ...value, fields });
  }

  // 编辑嵌套选项块里的某个子字段(如 `reality-opts.public-key`)。
  // 当该分组对象不再有任何条目时,整个分组会被剪除删掉。
  function setGroupField(groupKey: string, path: string, v: unknown) {
    const current = value.fields[groupKey];
    const nested = current && typeof current === "object" && !Array.isArray(current)
      ? (current as Record<string, unknown>)
      : {};
    const next = setPath(nested, path, v);
    const fields = { ...value.fields };
    if (Object.keys(next).length === 0) delete fields[groupKey];
    else fields[groupKey] = next;
    onChange({ ...value, fields });
  }

  // 高级行用有序数组保存,这样重命名某个 key 不会打乱周围字段的顺序。
  // 每次编辑都会把它们合并回 `fields`。
  const advanced = advancedEntries(value.fields, known);

  function setAdvanced(next: [string, unknown][]) {
    const fields: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(value.fields)) {
      if (known.has(k)) fields[k] = v;
    }
    for (const [k, v] of next) fields[k] = v;
    onChange({ ...value, fields });
  }

  function fieldLabel(def: FieldDef) {
    return t(`nodeFields.${def.key}`, def.key);
  }

  return (
    <Form layout="vertical">
      <Form.Item label={t("nodes.name")} required>
        <Input
          value={value.name}
          onChange={(e) => onChange({ ...value, name: e.target.value })}
        />
      </Form.Item>
      <Form.Item label={t("nodes.type")} required>
        <Select
          showSearch
          style={{ width: "100%" }}
          options={NODE_TYPES.map((o) => ({ value: o, label: nodeTypeLabel(o) }))}
          value={value.type || undefined}
          onChange={(s) => onChange({ ...value, type: s })}
          placeholder={t("nodes.typePlaceholder")}
          filterOption={(input, opt) => {
            // 按全称或字面量过滤,输入「shadow」或「ss」都能搜到;底层值仍是字面量。
            const q = input.toLowerCase();
            return (
              String(opt?.label ?? "").toLowerCase().includes(q) ||
              String(opt?.value ?? "").toLowerCase().includes(q)
            );
          }}
        />
      </Form.Item>

      {commonFields(value.type)
        .filter((def) => !def.showWhen || def.showWhen(value.fields))
        .map((def) => (
          <Form.Item key={def.key} label={fieldLabel(def)}>
            <FieldInput
              def={def}
              value={value.fields[def.key]}
              onChange={(v) => setField(def.key, v)}
            />
          </Form.Item>
        ))}

      {groupsFor(value.type)
        .filter((group) => !group.showWhen || group.showWhen(value.fields))
        .map((group) => {
          const nested = (value.fields[group.key] ?? {}) as Record<string, unknown>;
          return (
            <div className="modal-block" key={group.key}>
              <div className="modal-block-title">{t(`nodeGroups.${group.key}`, group.key)}</div>
              {group.fields.map((sub) => (
                <Form.Item key={sub.key} label={fieldLabel(sub)} style={{ marginBottom: 12 }}>
                  <FieldInput
                    def={sub}
                    value={getPath(nested, sub.key)}
                    onChange={(v) => setGroupField(group.key, sub.key, v)}
                  />
                </Form.Item>
              ))}
            </div>
          );
        })}

      <AdvancedFields entries={advanced} reserved={known} onChange={setAdvanced} />
    </Form>
  );
}
