// 节点编辑器和代理组选项编辑器共用的结构化编辑控件:由 `FieldDef` 驱动的带类型
// 输入框,以及一个「高级」自由键值行区块(标量给类型化输入框;嵌套对象用一小段
// YAML 编辑),让管理员永远不必手写原始配置。

import { useState } from "react";
import {
  AutoComplete,
  Button,
  Divider,
  Input,
  InputNumber,
  Select,
  Space,
  Switch,
  Typography,
} from "antd";
import { DownOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { parse as parseYaml, stringify as stringifyYaml } from "yaml";
import type { FieldDef } from "./nodeSchema";
import "./modal.css";

/** 可点选的类型 chip 行(节点/分组类型选择)。自由输入仍由调用方旁边的控件处理;chip 是快捷选择。 */
export function TypeChips({
  options,
  value,
  onChange,
  labels,
}: {
  options: readonly string[];
  value: string;
  onChange: (value: string) => void;
  /** 可选:把选项值映射为展示文案(如 manual→手动);缺省直接显示值。 */
  labels?: Record<string, string>;
}) {
  return (
    <div className="type-chips">
      {options.map((o) => (
        <button
          key={o}
          type="button"
          className={`type-chip${value === o ? " active" : ""}`}
          onClick={() => onChange(o)}
        >
          {labels?.[o] ?? o}
        </button>
      ))}
    </div>
  );
}

/** 单个已知字段的带类型输入框。标签由调用方负责。 */
export function FieldInput({
  def,
  value,
  onChange,
}: {
  def: FieldDef;
  value: unknown;
  onChange: (v: unknown) => void;
}) {
  switch (def.kind) {
    case "number":
      return (
        <InputNumber
          style={{ width: "100%" }}
          value={typeof value === "number" ? value : undefined}
          onChange={(n) => onChange(n)}
          placeholder={def.placeholder}
        />
      );
    case "switch":
      return <Switch checked={value === true} onChange={(c) => onChange(c)} />;
    case "password":
      return (
        <Input.Password
          value={value == null ? "" : String(value)}
          onChange={(e) => onChange(e.target.value)}
        />
      );
    case "select":
      return (
        <AutoComplete
          style={{ width: "100%" }}
          suffixIcon={<DownOutlined />}
          options={(def.options ?? []).map((o) => ({ value: o }))}
          value={value == null ? "" : String(value)}
          onChange={(s) => onChange(s)}
          placeholder={def.placeholder}
          allowClear
          filterOption={(input, opt) =>
            (opt?.value ?? "").toLowerCase().includes(input.toLowerCase())
          }
        />
      );
    case "tags":
      return (
        <Select
          mode="tags"
          style={{ width: "100%" }}
          value={Array.isArray(value) ? (value as string[]) : []}
          onChange={(v) => onChange(v)}
          options={(def.options ?? []).map((o) => ({ value: o }))}
          tokenSeparators={[","]}
          placeholder={def.placeholder}
        />
      );
    default:
      return (
        <Input
          value={value == null ? "" : String(value)}
          placeholder={def.placeholder}
          onChange={(e) => onChange(e.target.value)}
        />
      );
  }
}

/** 高级行的值类型,决定该行用哪种编辑器;按行固定,不随输入中途的值变化。 */
type AdvancedKind = "text" | "number" | "bool" | "yaml";

interface AdvancedRow {
  /** 稳定的行 id,作 React key:删除前面的行不会让后面的行错用别行的编辑器状态。 */
  id: number;
  key: string;
  kind: AdvancedKind;
  value: unknown;
}

function kindOf(v: unknown): AdvancedKind {
  if (typeof v === "boolean") return "bool";
  if (typeof v === "number") return "number";
  if (v !== null && typeof v === "object") return "yaml";
  return "text";
}

let nextRowId = 0;

function toRows(entries: [string, unknown][]): AdvancedRow[] {
  return entries.map(([key, value]) => ({ id: nextRowId++, key, kind: kindOf(value), value }));
}

/** 行在写回对象时是否被跳过:key 为空,或与保留字段、前面的行重名。 */
function skippedRows(rows: AdvancedRow[], reserved: Set<string>): Set<number> {
  const seen = new Set<string>();
  const skipped = new Set<number>();
  for (const r of rows) {
    const k = r.key.trim();
    if (!k || reserved.has(k) || seen.has(k)) skipped.add(r.id);
    else seen.add(k);
  }
  return skipped;
}

/** 切换行类型时尽量保留已填的值。 */
function convertValue(value: unknown, kind: AdvancedKind): unknown {
  switch (kind) {
    case "yaml":
      // 先填文本再切到 YAML 时,用户的本意是把这段文本当 YAML;解析失败则保留原文交给用户修正。
      if (typeof value !== "string") return value;
      if (value.trim() === "") return null;
      try {
        return parseYaml(value);
      } catch {
        return value;
      }
    case "bool":
      return value === true || value === "true";
    case "number": {
      const n = Number(value);
      return value !== "" && value != null && Number.isFinite(n) ? n : null;
    }
    case "text":
      return value !== null && typeof value === "object"
        ? stringifyYaml(value).trimEnd()
        : value == null
          ? ""
          : String(value);
    default:
      return value;
  }
}

/**
 * 可编辑的高级键值行列表,保持文档顺序。行在组件内保存:输入中途 key 与保留字段(表单已直接渲染的
 * 字段)或前面的行重名时,该行标红且不写回,不会覆盖已有值。只有有效行经 `onChange` 写回。
 */
export function AdvancedFields({
  entries,
  reserved,
  onChange,
}: {
  entries: [string, unknown][];
  reserved: Set<string>;
  onChange: (next: [string, unknown][]) => void;
}) {
  const { t } = useTranslation();
  const [rows, setRows] = useState(() => toRows(entries));
  // 最近一次写回的有效行。外部值与之不同(如切换节点类型改变了已知字段)时,按外部值重建行。
  const [synced, setSynced] = useState(() => JSON.stringify(entries));
  const incoming = JSON.stringify(entries);
  if (incoming !== synced) {
    setSynced(incoming);
    setRows(toRows(entries));
  }

  const skipped = skippedRows(rows, reserved);

  function update(next: AdvancedRow[]) {
    const skip = skippedRows(next, reserved);
    const valid: [string, unknown][] = next
      .filter((r) => !skip.has(r.id))
      .map((r) => [r.key.trim(), r.value]);
    setRows(next);
    setSynced(JSON.stringify(valid));
    onChange(valid);
  }

  function patch(id: number, change: Partial<AdvancedRow>) {
    update(rows.map((r) => (r.id === id ? { ...r, ...change } : r)));
  }

  const kindOptions = (["text", "number", "bool", "yaml"] as const).map((k) => ({
    value: k,
    label: t(`fields.kind.${k}`),
  }));

  return (
    <>
      <Divider titlePlacement="start" plain>
        {t("fields.advanced")}
      </Divider>
      <Typography.Paragraph type="secondary" style={{ marginTop: -8 }}>
        {t("fields.advancedHint")}
      </Typography.Paragraph>
      {rows.map((r) => {
        const conflict = r.key.trim() !== "" && skipped.has(r.id);
        return (
          <div key={r.id} style={{ marginBottom: 8 }}>
            <Space align="start" style={{ display: "flex" }}>
              <Input
                style={{ width: 160 }}
                placeholder={t("fields.key")}
                status={conflict ? "error" : undefined}
                value={r.key}
                onChange={(e) => patch(r.id, { key: e.target.value })}
              />
              <Select
                style={{ width: 84 }}
                value={r.kind}
                options={kindOptions}
                onChange={(kind) => patch(r.id, { kind, value: convertValue(r.value, kind) })}
              />
              <AdvancedValue
                kind={r.kind}
                value={r.value}
                onChange={(value) => patch(r.id, { value })}
              />
              <Button danger onClick={() => update(rows.filter((x) => x.id !== r.id))}>
                {t("fields.remove")}
              </Button>
            </Space>
            {conflict && (
              <Typography.Text type="danger" style={{ fontSize: 12 }}>
                {t("fields.keyConflict")}
              </Typography.Text>
            )}
          </div>
        );
      })}
      <Button
        onClick={() => update([...rows, { id: nextRowId++, key: "", kind: "text", value: "" }])}
        style={{ marginTop: 4 }}
      >
        {t("fields.addField")}
      </Button>
    </>
  );
}

/** 高级行的值编辑器,按行固定的类型渲染。 */
function AdvancedValue({
  kind,
  value,
  onChange,
}: {
  kind: AdvancedKind;
  value: unknown;
  onChange: (v: unknown) => void;
}) {
  switch (kind) {
    case "bool":
      return <Switch checked={value === true} onChange={onChange} />;
    case "number":
      return (
        <InputNumber
          style={{ width: 220 }}
          value={typeof value === "number" ? value : null}
          onChange={(n) => onChange(n)}
        />
      );
    case "yaml":
      return <ObjectField value={value} onChange={onChange} />;
    default:
      return (
        <Input
          style={{ width: 220 }}
          value={value == null ? "" : String(value)}
          onChange={(e) => onChange(e.target.value)}
        />
      );
  }
}

/** 嵌套对象/数组的高级值,用一小段 YAML 编辑。 */
function ObjectField({
  value,
  onChange,
}: {
  value: unknown;
  onChange: (v: unknown) => void;
}) {
  // 字符串只来自「文本切到 YAML 且解析失败」:原样显示,交给用户修正;其余值序列化为 YAML。
  const [text, setText] = useState(() =>
    typeof value === "string" ? value : value == null ? "" : stringifyYaml(value).trimEnd(),
  );
  return (
    <Input.TextArea
      style={{ width: 220, fontFamily: "monospace" }}
      autoSize={{ minRows: 2, maxRows: 8 }}
      value={text}
      onChange={(e) => {
        setText(e.target.value);
        try {
          onChange(parseYaml(e.target.value));
        } catch {
          // YAML 编辑中途解析失败时,保留上一次有效的解析结果。
        }
      }}
    />
  );
}

/** 返回对象中不属于已知字段的有序高级 [key, value] 对(已知字段由各表单直接渲染)。 */
export function advancedEntries(
  obj: Record<string, unknown>,
  known: Set<string>,
): [string, unknown][] {
  return Object.entries(obj).filter(([k]) => !known.has(k));
}

function isObject(v: unknown): v is Record<string, unknown> {
  return v != null && typeof v === "object" && !Array.isArray(v);
}

export function isEmptyValue(v: unknown): boolean {
  return (
    v === "" ||
    v === undefined ||
    v === null ||
    (Array.isArray(v) && v.length === 0) ||
    (isObject(v) && Object.keys(v).length === 0)
  );
}

/** 从嵌套对象里读取可能带点的路径(如 `headers.Host`)。 */
export function getPath(obj: Record<string, unknown>, path: string): unknown {
  return path.split(".").reduce<unknown>((acc, k) => (isObject(acc) ? acc[k] : undefined), obj);
}

/**
 * 在嵌套对象里以不可变方式设置/清除带点路径,并剪除因此变空的父对象。
 * 返回新的根对象(全部清空时为空对象)。
 */
export function setPath(
  obj: Record<string, unknown>,
  path: string,
  value: unknown,
): Record<string, unknown> {
  const keys = path.split(".");
  const root: Record<string, unknown> = { ...obj };
  if (keys.length === 1) {
    if (isEmptyValue(value)) delete root[keys[0]];
    else root[keys[0]] = value;
    return root;
  }
  const [head, ...rest] = keys;
  const child = isObject(root[head]) ? (root[head] as Record<string, unknown>) : {};
  const nextChild = setPath(child, rest.join("."), value);
  if (Object.keys(nextChild).length === 0) delete root[head];
  else root[head] = nextChild;
  return root;
}
