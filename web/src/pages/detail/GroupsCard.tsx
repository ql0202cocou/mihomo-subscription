import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { App as AntdApp, Button, Form, Input, Modal, Popconfirm, Select } from "antd";
import { DeleteOutlined, EditOutlined, HolderOutlined, PlusOutlined } from "@ant-design/icons";
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
import type { CustomGroup, CustomNode, GroupType, ProxiesResponse, Regenerate } from "../../types";
import { AdvancedFields, FieldInput, TypeChips, advancedEntries } from "../../components/fields";
import { useRegenerateNotice } from "../../components/regenerate";
import { useSerialSave } from "../../components/useSerialSave";
import { BUILTIN_POLICIES, GROUP_TYPES, groupOptionFields, groupOptionKeys } from "./groupSchema";

interface Props {
  profileId: string;
  groups: CustomGroup[];
  nodes: CustomNode[];
  /** 生成缓存的刷新信号:变化时重新拉取生成预览(成员候选、分组顺序)。 */
  generatedAt: string | null;
  /** 任一分组变更持久化成功后回调(保存/删除/导入),父组件据此重新加载。 */
  onSaved: () => void;
  /** 排序保存带回的离线重生成结果,交给父组件展示校验错误。 */
  onRegenerate: (r: Regenerate) => void;
}

type Options = Record<string, unknown>;

/** 可排序行:name 即 group.name,单列出来充当 dnd id 与 React key(分组名在订阅内唯一)。 */
interface GroupRow {
  name: string;
  group: CustomGroup;
}

/** 有序的可编辑行:跟随生成输出的顺序,丢弃失效的名字,追加新增的。 */
function buildRows(orderNames: string[], groups: CustomGroup[]): GroupRow[] {
  const byName = new Map(groups.map((g) => [g.name, g]));
  const seen = new Set<string>();
  const result: GroupRow[] = [];
  for (const name of orderNames) {
    const g = byName.get(name);
    if (g && !seen.has(name)) {
      result.push({ name, group: g });
      seen.add(name);
    }
  }
  for (const g of groups) {
    if (!seen.has(g.name)) {
      result.push({ name: g.name, group: g });
      seen.add(g.name);
    }
  }
  return result;
}

/** 对仍存在的行保持当前屏幕顺序;追加新增、丢弃已删,避免乐观拖拽被 reload 冲掉。 */
function reconcileRows(prev: GroupRow[], derived: GroupRow[]): GroupRow[] {
  if (prev.length === 0) return derived;
  const byName = new Map(derived.map((r) => [r.name, r]));
  const result: GroupRow[] = [];
  for (const r of prev) {
    const d = byName.get(r.name);
    if (d) {
      result.push(d);
      byName.delete(r.name);
    }
  }
  for (const d of derived) {
    if (byName.has(d.name)) {
      result.push(d);
      byName.delete(d.name);
    }
  }
  return result;
}

export default function GroupsCard({
  profileId,
  groups,
  nodes,
  generatedAt,
  onSaved,
  onRegenerate,
}: Props) {
  const { t } = useTranslation();
  const { message } = AntdApp.useApp();
  const saveInOrder = useSerialSave();
  const notice = useRegenerateNotice();
  const [editing, setEditing] = useState<CustomGroup | null>(null);
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [groupType, setGroupType] = useState<GroupType>("select");
  const [members, setMembers] = useState<string[]>([]);
  const [options, setOptions] = useState<Options>({});

  const [proxyNames, setProxyNames] = useState<string[]>([]);
  const [orderNames, setOrderNames] = useState<string[]>([]);
  const [rows, setRows] = useState<GroupRow[]>([]);
  const [importing, setImporting] = useState(false);

  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 4 } }));

  const loadGeneratedPreview = useCallback(async () => {
    try {
      const res = await api<ProxiesResponse>(`/api/profiles/${profileId}/proxies`);
      setProxyNames(res.proxies.map((p) => p.name));
      setOrderNames(res.groups.map((g) => g.name));
    } catch {
      // 成员仍可手动输入
    }
  }, [profileId]);

  useEffect(() => {
    void loadGeneratedPreview();
  }, [loadGeneratedPreview, generatedAt]);

  const derived = useMemo(() => buildRows(orderNames, groups), [orderNames, groups]);
  // 排序保存失败后置位:下一次 reload 以服务端顺序整体重建,而非保留屏幕上的失败顺序。
  const resync = useRef(false);
  useEffect(() => {
    if (resync.current) {
      resync.current = false;
      setRows(derived);
    } else {
      setRows((prev) => reconcileRows(prev, derived));
    }
  }, [derived]);

  async function onDragEnd(event: DragEndEvent) {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const oldIndex = rows.findIndex((r) => r.name === active.id);
    const newIndex = rows.findIndex((r) => r.name === over.id);
    if (oldIndex < 0 || newIndex < 0) return;
    const next = arrayMove(rows, oldIndex, newIndex);
    setRows(next);
    const res = await saveInOrder(() =>
      api<{ regenerate: Regenerate }>(`/api/profiles/${profileId}/group-order`, {
        method: "PUT",
        body: JSON.stringify({ order: next.map((r) => r.name) }),
      }),
    );
    // 之后又有拖拽:界面交给最后一次保存决定。
    if (!res.latest) return;
    if (res.ok) {
      notice(res.value.regenerate, t("groups.orderSaved"));
      onRegenerate(res.value.regenerate);
    } else {
      resync.current = true; // 回滚到服务端实际持有的顺序(与 NodesCard 一致)
      message.error(errorMessage(res.error, t("groups.orderSaveFailed")));
    }
    void loadGeneratedPreview();
  }

  async function importProviderGroups() {
    setImporting(true);
    try {
      const res = await api<{ imported: number; skipped: number }>(
        `/api/profiles/${profileId}/import-provider-groups`,
        { method: "POST" },
      );
      if (res.imported === 0) message.info(t("groups.importNone"));
      else message.success(t("groups.imported", { count: res.imported }));
      onSaved();
    } catch (e) {
      message.error(errorMessage(e, t("groups.importFailed")));
    } finally {
      setImporting(false);
    }
  }

  function startAdd() {
    setEditing(null);
    setName("");
    setGroupType("select");
    setMembers([]);
    setOptions({});
    setOpen(true);
  }

  function startEdit(group: CustomGroup) {
    setEditing(group);
    setName(group.name);
    setGroupType(group.group_type);
    setMembers(group.members);
    setOptions(group.options ?? {});
    setOpen(true);
  }

  function setOption(key: string, v: unknown) {
    const next = { ...options };
    if (v === "" || v === undefined || v === null) delete next[key];
    else next[key] = v;
    setOptions(next);
  }

  /** 整体替换高级行:保留当前类型已知字段的已有值,未知键只来自高级行。 */
  function setAdvancedOptions(advRows: [string, unknown][]) {
    const known = groupOptionKeys(groupType);
    const next: Options = {};
    for (const [k, v] of Object.entries(options)) if (known.has(k)) next[k] = v;
    for (const [k, v] of advRows) next[k] = v;
    setOptions(next);
  }

  async function save() {
    if (!name.trim()) {
      message.error(t("groups.nameRequired"));
      return;
    }
    const cleaned: Options = {};
    for (const [k, v] of Object.entries(options)) {
      if (k.trim() === "" || v === "" || v === undefined || v === null) continue;
      cleaned[k] = v;
    }
    const body = JSON.stringify({
      name: name.trim(),
      group_type: groupType,
      members,
      // 空对象按 null 提交,与后端「无选项」的表示一致
      options: Object.keys(cleaned).length ? cleaned : null,
      // 弹窗不暴露 enabled 开关,编辑时原样保留
      enabled: editing ? editing.enabled : true,
    });
    try {
      if (editing) await api(`/api/profiles/${profileId}/groups/${editing.id}`, { method: "PUT", body });
      else await api(`/api/profiles/${profileId}/groups`, { method: "POST", body });
      setOpen(false);
      onSaved();
    } catch (e) {
      message.error(errorMessage(e, t("common.saveFailed")));
    }
  }

  async function remove(group: CustomGroup) {
    try {
      await api(`/api/profiles/${profileId}/groups/${group.id}`, { method: "DELETE" });
      onSaved();
    } catch (e) {
      message.error(errorMessage(e, t("common.deleteFailed")));
    }
  }

  // 成员候选 = 机场节点 + 自定义节点 + 其他分组(排除自身,避免循环引用)+ 内置策略
  const memberOptions = dedupe([
    ...proxyNames,
    ...nodes.map((n) => n.name),
    ...groups.filter((g) => g.id !== editing?.id).map((g) => g.name),
    ...BUILTIN_POLICIES,
  ]).map((value) => ({ value, label: value }));

  const optionFields = groupOptionFields(groupType);
  const advancedOptions = advancedEntries(options, groupOptionKeys(groupType));
  // 表单已直接渲染的选项,加上由分组自身字段决定、后端生成时忽略的结构键。
  const reservedOptionKeys = new Set([...groupOptionKeys(groupType), "name", "type", "proxies"]);

  return (
    <div className="dcard">
      <div className="dcard-head">
        <span className="dcard-title">
          {t("groups.title")} <span className="row-sub">{t("groups.count", { count: rows.length })}</span>
        </span>
        <div className="dcard-actions">
          <Popconfirm title={t("groups.importConfirm")} onConfirm={importProviderGroups}>
            <Button loading={importing}>{t("groups.importProvider")}</Button>
          </Popconfirm>
          <Button type="primary" icon={<PlusOutlined />} onClick={startAdd}>
            {t("groups.add")}
          </Button>
        </div>
      </div>

      {rows.length === 0 ? (
        <div className="empty-line">{t("groups.empty")}</div>
      ) : (
        <DndContext sensors={sensors} collisionDetection={closestCenter} onDragEnd={onDragEnd}>
          <SortableContext items={rows.map((r) => r.name)} strategy={verticalListSortingStrategy}>
            {rows.map((row) => (
              <SortableGroupRow key={row.name} row={row} onEdit={startEdit} onRemove={remove} />
            ))}
          </SortableContext>
        </DndContext>
      )}
      <div className="dcard-note">{t("groups.dragHint")}</div>

      <Modal
        title={editing ? t("groups.edit") : t("groups.add")}
        open={open}
        onCancel={() => setOpen(false)}
        onOk={save}
        width={560}
        okText={t("common.save")}
        cancelText={t("common.cancel")}
        destroyOnClose
      >
        <Form layout="vertical">
          <Form.Item label={t("groups.name")} required>
            <Input value={name} onChange={(e) => setName(e.target.value)} />
          </Form.Item>
          <Form.Item label={t("groups.type")} required>
            <TypeChips
              options={GROUP_TYPES}
              value={groupType}
              onChange={(v) => setGroupType(v as GroupType)}
            />
          </Form.Item>
          <Form.Item label={t("groups.members")} help={t("groups.membersHint")}>
            <Select
              mode="tags"
              value={members}
              onChange={setMembers}
              options={memberOptions}
              tokenSeparators={[","]}
              style={{ width: "100%" }}
              filterOption={(input, opt) =>
                String(opt?.value ?? "").toLowerCase().includes(input.toLowerCase())
              }
            />
          </Form.Item>

          {optionFields.length > 0 && (
            <div className="modal-block">
              <div className="modal-block-title">{t("groups.options")}</div>
              {optionFields.map((def) => (
                <Form.Item
                  key={def.key}
                  label={t(`groupFields.${def.key}`, def.key)}
                  style={{ marginBottom: 12 }}
                >
                  <FieldInput
                    def={def}
                    value={options[def.key]}
                    onChange={(v) => setOption(def.key, v)}
                  />
                </Form.Item>
              ))}
            </div>
          )}

          <AdvancedFields
            entries={advancedOptions}
            reserved={reservedOptionKeys}
            onChange={setAdvancedOptions}
          />
        </Form>
      </Modal>
    </div>
  );
}

function SortableGroupRow({
  row,
  onEdit,
  onRemove,
}: {
  row: GroupRow;
  onEdit: (group: CustomGroup) => void;
  onRemove: (group: CustomGroup) => void;
}) {
  const { t } = useTranslation();
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: row.name,
  });
  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
    background: isDragging ? "var(--bg-subtle)" : undefined,
  };
  const { group } = row;
  return (
    <div className="row" ref={setNodeRef} style={style}>
      <span className="row-grab" {...attributes} {...listeners} aria-label={t("common.drag")}>
        <HolderOutlined />
      </span>
      <span className="group-row-name">
        <span className="group-row-title">{row.name}</span>
        <span className="row-sub">{t("groups.membersCount", { count: group.members.length })}</span>
      </span>
      <span className="tag-mono tag-policy">{group.group_type}</span>
      <span className="row-actions">
        <button className="icon-btn" onClick={() => onEdit(group)} aria-label={t("basic.edit")}>
          <EditOutlined />
        </button>
        <Popconfirm title={t("groups.deleteConfirm")} onConfirm={() => onRemove(group)}>
          <button className="icon-btn danger" aria-label={t("groups.delete")}>
            <DeleteOutlined />
          </button>
        </Popconfirm>
      </span>
    </div>
  );
}

function dedupe(items: string[]): string[] {
  return Array.from(new Set(items.filter((s) => s.trim() !== "")));
}
