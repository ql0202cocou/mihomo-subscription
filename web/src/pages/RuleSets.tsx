import { useCallback, useEffect, useState } from "react";
import { App as AntdApp, Button, Input, InputNumber, Modal, Popconfirm, Switch } from "antd";
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
import type { RuleSet } from "../types";
import { useSerialSave } from "../components/useSerialSave";
import { TypeChips } from "../components/fields";
import {
  RULE_SET_BEHAVIORS as BEHAVIORS,
  RULE_SET_MANUAL_FORMATS as MANUAL_FORMATS,
  RULE_SET_REMOTE_FORMATS as REMOTE_FORMATS,
  RULE_SET_SOURCES as SOURCES,
} from "../components/ruleSetConstants";
import "../components/cards.css";
import "./RuleSets.css";

// 规则集名要进 URL 路径并作 RULE-SET 引用名,字符集与后端校验保持一致(见 src/rulelib.rs)。
const NAME_RE = /^[A-Za-z0-9._-]+$/;

interface FormState {
  name: string;
  behavior: string;
  format: string;
  source: "manual" | "remote";
  content: string;
  url: string;
  intervalHours: number;
  cache: boolean;
}

const EMPTY: FormState = {
  name: "",
  behavior: "domain",
  format: "yaml",
  source: "manual",
  content: "",
  url: "",
  intervalHours: 24,
  cache: true,
};

export default function RuleSets() {
  const { t } = useTranslation();
  const { message } = AntdApp.useApp();
  const saveInOrder = useSerialSave();
  const [rows, setRows] = useState<RuleSet[]>([]);
  const [open, setOpen] = useState(false);
  const [editing, setEditing] = useState<RuleSet | null>(null);
  const [form, setForm] = useState<FormState>(EMPTY);

  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 4 } }));

  const load = useCallback(async () => {
    try {
      setRows(await api<RuleSet[]>("/api/rule-sets"));
    } catch {
      // 瞬时错误时保留当前列表
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  function startAdd() {
    setEditing(null);
    setForm(EMPTY);
    setOpen(true);
  }

  function startEdit(rs: RuleSet) {
    setEditing(rs);
    setForm({
      name: rs.name,
      behavior: rs.behavior,
      format: rs.format,
      source: rs.source,
      content: rs.content,
      url: "", // 远程 URL 已脱敏不回显;留空保持不变
      intervalHours: rs.interval_hours,
      cache: rs.cache,
    });
    setOpen(true);
  }

  // 切来源时纠正不兼容的 format(手动不支持 mrs)。
  function setSource(source: "manual" | "remote") {
    setForm((f) => ({
      ...f,
      source,
      format: source === "manual" && f.format === "mrs" ? "yaml" : f.format,
    }));
  }

  async function save() {
    const name = form.name.trim();
    if (!name) return message.error(t("ruleSets.nameRequired"));
    if (!NAME_RE.test(name)) return message.error(t("ruleSets.nameInvalid"));
    const base = {
      name,
      behavior: form.behavior,
      format: form.format,
      source: form.source,
      enabled: editing ? editing.enabled : true,
    };
    // 远程 URL 留空则不放进 body:编辑时表示保持不变,新建时由后端校验必填。
    const body =
      form.source === "remote"
        ? {
            ...base,
            interval_hours: form.intervalHours,
            cache: form.cache,
            ...(form.url.trim() ? { url: form.url.trim() } : {}),
          }
        : { ...base, content: form.content };
    try {
      if (editing) await api(`/api/rule-sets/${editing.id}`, { method: "PUT", body: JSON.stringify(body) });
      else await api("/api/rule-sets", { method: "POST", body: JSON.stringify(body) });
      setOpen(false);
      await load();
    } catch (e) {
      message.error(errorMessage(e, t("common.saveFailed")));
    }
  }

  async function remove(rs: RuleSet) {
    try {
      await api(`/api/rule-sets/${rs.id}`, { method: "DELETE" });
      await load();
    } catch (e) {
      message.error(errorMessage(e, t("common.deleteFailed")));
    }
  }

  // 排序以规则集名(全局唯一)为键,与 PUT /api/rule-sets/order 的 order 字段对应。
  async function onDragEnd(event: DragEndEvent) {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const oldIndex = rows.findIndex((n) => n.name === active.id);
    const newIndex = rows.findIndex((n) => n.name === over.id);
    if (oldIndex < 0 || newIndex < 0) return;
    const next = arrayMove(rows, oldIndex, newIndex);
    setRows(next); // 乐观更新
    const res = await saveInOrder(() =>
      api("/api/rule-sets/order", {
        method: "PUT",
        body: JSON.stringify({ order: next.map((n) => n.name) }),
      }),
    );
    // 之后又有拖拽:界面交给最后一次保存决定,避免中途 reload 把列表打回旧顺序。
    if (!res.latest) return;
    if (res.ok) message.success(t("ruleSets.orderSaved"));
    else message.error(errorMessage(res.error, t("ruleSets.orderSaveFailed")));
    void load(); // 以服务端为准;保存失败时即回到服务端实际持有的顺序
  }

  const formatOptions = form.source === "remote" ? REMOTE_FORMATS : MANUAL_FORMATS;

  return (
    <div className="page">
      <p className="detail-context" style={{ marginTop: 4 }}>
        {t("ruleSets.help")}
      </p>
      <div className="dcard">
        <div className="dcard-head">
          <span className="dcard-title">
            {t("nav.rules")}{" "}
            <span className="row-sub">{t("ruleSets.groupCount", { count: rows.length })}</span>
          </span>
          <Button type="primary" icon={<PlusOutlined />} onClick={startAdd}>
            {t("ruleSets.add")}
          </Button>
        </div>

        {rows.length === 0 ? (
          <div className="empty-line">{t("ruleSets.empty")}</div>
        ) : (
          <DndContext sensors={sensors} collisionDetection={closestCenter} onDragEnd={onDragEnd}>
            <SortableContext items={rows.map((n) => n.name)} strategy={verticalListSortingStrategy}>
              {rows.map((rs) => (
                <RuleSetRow key={rs.name} rs={rs} onEdit={startEdit} onRemove={remove} />
              ))}
            </SortableContext>
          </DndContext>
        )}
        <div className="dcard-note">{t("ruleSets.dragHint")}</div>
      </div>

      <Modal
        title={editing ? t("ruleSets.edit") : t("ruleSets.add")}
        open={open}
        onCancel={() => setOpen(false)}
        onOk={save}
        width={600}
        okText={t("common.save")}
        cancelText={t("common.cancel")}
        destroyOnHidden
      >
        <div className="rs-form">
          <label className="rs-label">{t("ruleSets.name")}</label>
          <Input
            value={form.name}
            placeholder={t("ruleSets.namePlaceholder")}
            onChange={(e) => setForm({ ...form, name: e.target.value })}
          />

          <div className="rs-grid2">
            <div>
              <label className="rs-label">{t("ruleSets.behavior")}</label>
              <TypeChips
                options={BEHAVIORS}
                value={form.behavior}
                onChange={(v) => setForm({ ...form, behavior: v })}
              />
            </div>
            <div>
              <label className="rs-label">{t("ruleSets.format")}</label>
              <TypeChips
                options={formatOptions}
                value={form.format}
                onChange={(v) => setForm({ ...form, format: v })}
              />
            </div>
          </div>

          <label className="rs-label">{t("ruleSets.source")}</label>
          <TypeChips
            options={SOURCES}
            value={form.source}
            onChange={(v) => setSource(v as "manual" | "remote")}
            labels={{ manual: t("ruleSets.sourceManual"), remote: t("ruleSets.sourceRemote") }}
          />

          {form.source === "manual" ? (
            <div className="rs-section">
              <div className="rs-section-title">{t("ruleSets.content")}</div>
              <Input.TextArea
                value={form.content}
                placeholder={t("ruleSets.contentPlaceholder")}
                autoSize={{ minRows: 6, maxRows: 14 }}
                style={{ fontFamily: "var(--font-mono)" }}
                onChange={(e) => setForm({ ...form, content: e.target.value })}
              />
              <div className="rs-hint">{t("ruleSets.contentHint")}</div>
            </div>
          ) : (
            <div className="rs-section">
              <div className="rs-section-title">{t("ruleSets.sourceRemote")}</div>
              <label className="rs-label">{t("ruleSets.remoteUrl")}</label>
              <Input
                value={form.url}
                placeholder={
                  editing ? t("ruleSets.remoteUrlKeep") : t("ruleSets.remoteUrlPlaceholder")
                }
                style={{ fontFamily: "var(--font-mono)" }}
                onChange={(e) => setForm({ ...form, url: e.target.value })}
              />
              {editing && editing.remote_url_masked && (
                <div className="rs-hint">
                  {t("ruleSets.mirrorFrom")} {editing.remote_url_masked}
                </div>
              )}
              <div className="rs-remote-row">
                <div>
                  <label className="rs-label">{t("ruleSets.interval")}</label>
                  <InputNumber
                    min={1}
                    precision={0}
                    value={form.intervalHours}
                    onChange={(v) => setForm({ ...form, intervalHours: v ?? 24 })}
                  />
                </div>
                <div className="rs-cache">
                  <span>{t("ruleSets.cache")}</span>
                  <Switch
                    checked={form.cache}
                    onChange={(cache) => setForm({ ...form, cache })}
                  />
                </div>
              </div>
              <div className="rs-hint">{t("ruleSets.cacheHint")}</div>
            </div>
          )}
        </div>
      </Modal>
    </div>
  );
}

function RuleSetRow({
  rs,
  onEdit,
  onRemove,
}: {
  rs: RuleSet;
  onEdit: (rs: RuleSet) => void;
  onRemove: (rs: RuleSet) => void;
}) {
  const { t } = useTranslation();
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: rs.name,
  });
  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
    background: isDragging ? "var(--bg-subtle)" : undefined,
  };
  return (
    <div className="rs-row" ref={setNodeRef} style={style}>
      <span className="row-grab" {...attributes} {...listeners} aria-label={t("common.drag")}>
        <HolderOutlined />
      </span>
      <div className="rs-main">
        <div className="rs-line1">
          <span className="rs-name">{rs.name}</span>
          <span className="rs-count">{t("ruleSets.count", { count: rs.count })}</span>
        </div>
        {rs.source === "remote" && rs.remote_url_masked && (
          <div className="rs-line2">
            <code className="rs-url">
              {t("ruleSets.mirrorFrom")} {rs.remote_url_masked}
            </code>
          </div>
        )}
      </div>
      <span className="row-actions">
        <span className="tag-mono tag-proto custom">
          {rs.behavior}/{rs.format}
        </span>
        <button className="icon-btn" onClick={() => onEdit(rs)} aria-label={t("ruleSets.manage")}>
          <EditOutlined />
        </button>
        <Popconfirm title={t("ruleSets.deleteConfirm")} onConfirm={() => onRemove(rs)}>
          <button className="icon-btn danger" aria-label={t("ruleSets.delete")}>
            <DeleteOutlined />
          </button>
        </Popconfirm>
      </span>
    </div>
  );
}
