import { useCallback, useEffect, useState } from "react";
import { App as AntdApp, Button, Modal, Popconfirm } from "antd";
import {
  DeleteOutlined,
  EditOutlined,
  HolderOutlined,
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
import { api, errorMessage } from "../api";
import type { CustomNode } from "../types";
import { useSerialSave } from "../components/useSerialSave";
import NodeForm, { contentToModel, modelToContent, type NodeModel } from "../components/NodeForm";
import { NODE_TYPE_LABELS } from "../components/nodeSchema";
import "../components/cards.css";

const EMPTY_MODEL: NodeModel = { name: "", type: "", fields: {} };

export default function GlobalNodes() {
  const { t } = useTranslation();
  const { message } = AntdApp.useApp();
  const saveInOrder = useSerialSave();
  const [rows, setRows] = useState<CustomNode[]>([]);
  const [open, setOpen] = useState(false);
  const [editing, setEditing] = useState<CustomNode | null>(null);
  const [model, setModel] = useState<NodeModel>(EMPTY_MODEL);
  // NodeForm 内部有派生状态(高级键值行等),每次打开弹窗换 key 强制重挂载复位。
  const [formKey, setFormKey] = useState("new");

  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 4 } }));

  const load = useCallback(async () => {
    try {
      setRows(await api<CustomNode[]>("/api/global-nodes"));
    } catch {
      // 瞬时错误时保留当前列表
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  function startAdd() {
    setEditing(null);
    setModel(EMPTY_MODEL);
    setFormKey(`new-${Date.now()}`);
    setOpen(true);
  }

  function startEdit(node: CustomNode) {
    setEditing(node);
    setModel(contentToModel(node.content));
    setFormKey(node.id);
    setOpen(true);
  }

  async function save() {
    if (!model.name.trim() || !model.type.trim()) {
      message.error(t("nodes.nameTypeRequired"));
      return;
    }
    const body = JSON.stringify({
      name: model.name.trim(),
      node_type: model.type.trim(),
      content: modelToContent(model),
      enabled: editing ? editing.enabled : true,
    });
    try {
      if (editing) await api(`/api/global-nodes/${editing.id}`, { method: "PUT", body });
      else await api("/api/global-nodes", { method: "POST", body });
      setOpen(false);
      await load();
    } catch (e) {
      message.error(errorMessage(e, t("common.saveFailed")));
    }
  }

  async function remove(node: CustomNode) {
    try {
      await api(`/api/global-nodes/${node.id}`, { method: "DELETE" });
      await load();
    } catch (e) {
      message.error(errorMessage(e, t("common.deleteFailed")));
    }
  }

  // 排序以节点名(全局唯一)为键,与 PUT /api/global-nodes/order 的 order 字段对应。
  async function onDragEnd(event: DragEndEvent) {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const oldIndex = rows.findIndex((n) => n.name === active.id);
    const newIndex = rows.findIndex((n) => n.name === over.id);
    if (oldIndex < 0 || newIndex < 0) return;
    const next = arrayMove(rows, oldIndex, newIndex);
    setRows(next); // 乐观更新
    const res = await saveInOrder(() =>
      api("/api/global-nodes/order", {
        method: "PUT",
        body: JSON.stringify({ order: next.map((n) => n.name) }),
      }),
    );
    // 之后又有拖拽:界面交给最后一次保存决定,避免中途 reload 把列表打回旧顺序。
    if (!res.latest) return;
    if (res.ok) message.success(t("nodes.orderSaved"));
    else message.error(errorMessage(res.error, t("nodes.orderSaveFailed")));
    void load(); // 以服务端为准;保存失败时即回到服务端实际持有的顺序
  }

  return (
    <div className="page">
      <p className="detail-context" style={{ marginTop: 4 }}>
        {t("nodes.globalHelper")}
      </p>
      <div className="dcard">
        <div className="dcard-head">
          <span className="dcard-title">
            {t("nav.nodes")} <span className="row-sub">{t("nodes.groupCount", { count: rows.length })}</span>
          </span>
          <Button type="primary" icon={<PlusOutlined />} onClick={startAdd}>
            {t("nodes.add")}
          </Button>
        </div>

        {rows.length === 0 ? (
          <div className="empty-line">{t("nodes.customEmpty")}</div>
        ) : (
          <DndContext sensors={sensors} collisionDetection={closestCenter} onDragEnd={onDragEnd}>
            <SortableContext items={rows.map((n) => n.name)} strategy={verticalListSortingStrategy}>
              {rows.map((node) => (
                <NodeRow key={node.name} node={node} onEdit={startEdit} onRemove={remove} />
              ))}
            </SortableContext>
          </DndContext>
        )}
        <div className="dcard-note">{t("nodes.dragHintGlobal")}</div>
      </div>

      <Modal
        title={editing ? t("nodes.edit") : t("nodes.add")}
        open={open}
        onCancel={() => setOpen(false)}
        onOk={save}
        width={680}
        okText={t("common.save")}
        cancelText={t("common.cancel")}
        destroyOnClose
      >
        <NodeForm key={formKey} value={model} onChange={setModel} />
      </Modal>
    </div>
  );
}

function NodeRow({
  node,
  onEdit,
  onRemove,
}: {
  node: CustomNode;
  onEdit: (n: CustomNode) => void;
  onRemove: (n: CustomNode) => void;
}) {
  const { t } = useTranslation();
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: node.name,
  });
  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
    background: isDragging ? "var(--bg-subtle)" : undefined,
  };
  return (
    <div className="row" ref={setNodeRef} style={style}>
      <span className="row-grab" {...attributes} {...listeners} aria-label={t("common.drag")}>
        <HolderOutlined />
      </span>
      <span className="row-name">{node.name}</span>
      {node.node_type && (
        <span className="tag-mono tag-proto custom">
          {NODE_TYPE_LABELS[node.node_type] ?? node.node_type}
        </span>
      )}
      <span className="row-actions">
        <button className="icon-btn" onClick={() => onEdit(node)} aria-label={t("basic.edit")}>
          <EditOutlined />
        </button>
        <Popconfirm title={t("nodes.deleteConfirm")} onConfirm={() => onRemove(node)}>
          <button className="icon-btn danger" aria-label={t("nodes.delete")}>
            <DeleteOutlined />
          </button>
        </Popconfirm>
      </span>
    </div>
  );
}
