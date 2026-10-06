import { useCallback, useEffect, useMemo, useState } from "react";
import { App as AntdApp } from "antd";
import { HolderOutlined, LockOutlined } from "@ant-design/icons";
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
import type { CustomNode, ProxiesResponse, ProxyPreview, Regenerate } from "../../types";
import { nodeTypeLabel } from "../../components/nodeSchema";
import { useRegenerateNotice } from "../../components/regenerate";
import { useSerialSave } from "../../components/useSerialSave";

interface Props {
  profileId: string;
  /** 机场名,用作机场块的标题。 */
  profileName: string;
  /** 全局自定义节点池快照(此处只读;编辑在「节点配置」页)。 */
  nodes: CustomNode[];
  /** 生成缓存的刷新信号:变化时重新拉取节点预览。 */
  generatedAt: string | null;
  /** 保存带回的离线重生成结果,交给父组件展示校验错误。 */
  onRegenerate: (r: Regenerate) => void;
}

const DEFAULT_SECTIONS = ["provider", "custom"];

export default function NodesCard({ profileId, profileName, nodes, generatedAt, onRegenerate }: Props) {
  const { t } = useTranslation();
  const { message } = AntdApp.useApp();
  const saveInOrder = useSerialSave();
  const notice = useRegenerateNotice();
  const [proxies, setProxies] = useState<ProxyPreview[]>([]);
  /** 是否已生成过:区分「尚未生成」与「机场无节点」,仅影响空态文案。 */
  const [generated, setGenerated] = useState(true);
  const [sectionOrder, setSectionOrder] = useState<string[]>(DEFAULT_SECTIONS);

  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 4 } }));

  const loadGeneratedPreview = useCallback(async () => {
    try {
      const res = await api<ProxiesResponse>(`/api/profiles/${profileId}/proxies`);
      setProxies(res.proxies);
      setGenerated(res.generated);
      // 后端持久化的顺序异常(长度不为 2)时回退默认,避免渲染出未知块
      setSectionOrder(
        res.node_section_order.length === 2 ? res.node_section_order : DEFAULT_SECTIONS,
      );
    } catch {
      // 非致命:拿不到机场预览不影响只读展示。
    }
  }, [profileId]);

  useEffect(() => {
    void loadGeneratedPreview();
  }, [loadGeneratedPreview, generatedAt]);

  const customNames = useMemo(() => new Set(nodes.map((n) => n.name)), [nodes]);
  // 生成预览包含全部节点,按名字剔除自定义节点后剩下的即机场节点
  const providerNodes = useMemo(
    () => proxies.filter((p) => !customNames.has(p.name)),
    [proxies, customNames],
  );

  async function onSectionDragEnd(event: DragEndEvent) {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const next = arrayMove(
      sectionOrder,
      sectionOrder.indexOf(String(active.id)),
      sectionOrder.indexOf(String(over.id)),
    );
    setSectionOrder(next);
    const res = await saveInOrder(() =>
      api<{ regenerate: Regenerate }>(`/api/profiles/${profileId}/node-section-order`, {
        method: "PUT",
        body: JSON.stringify({ order: next }),
      }),
    );
    // 之后又有拖拽:界面交给最后一次保存决定。
    if (!res.latest) return;
    if (res.ok) {
      notice(res.value.regenerate, t("nodes.orderSaved"));
      onRegenerate(res.value.regenerate);
    } else {
      message.error(errorMessage(res.error, t("nodes.orderSaveFailed")));
    }
    // 以服务端为准重载;保存失败时即回到服务端实际持有的顺序。
    void loadGeneratedPreview();
  }

  const total = providerNodes.length + nodes.length;

  return (
    <div className="dcard">
      <div className="dcard-head">
        <span className="dcard-title">
          {t("nodes.title")} <span className="row-sub">{t("nodes.groupCount", { count: total })}</span>
        </span>
      </div>

      <DndContext sensors={sensors} collisionDetection={closestCenter} onDragEnd={onSectionDragEnd}>
        <SortableContext items={sectionOrder} strategy={verticalListSortingStrategy}>
          {sectionOrder.map((key) =>
            key === "provider" ? (
              <Section
                key="provider"
                id="provider"
                title={profileName || t("nodes.providerTitle")}
                sub={t("nodes.providerReadonly")}
                count={providerNodes.length}
              >
                {providerNodes.length === 0 ? (
                  <div className="empty-line">
                    {generated ? t("nodes.providerEmpty") : t("nodes.providerNotGenerated")}
                  </div>
                ) : (
                  providerNodes.map((p) => (
                    <div className="row" key={p.name}>
                      <span className="row-lock">
                        <LockOutlined />
                      </span>
                      <span className="row-name">{p.name}</span>
                      {p.type && (
                        <span className="tag-mono tag-proto">
                          {nodeTypeLabel(p.type)}
                        </span>
                      )}
                    </div>
                  ))
                )}
              </Section>
            ) : (
              <Section
                key="custom"
                id="custom"
                title={t("nodes.customGroup")}
                sub={t("nodes.customReadonly")}
                count={nodes.length}
              >
                {nodes.length === 0 ? (
                  <div className="empty-line">{t("nodes.customEmpty")}</div>
                ) : (
                  nodes.map((n) => (
                    <div className="row" key={n.name}>
                      <span className="row-lock">
                        <LockOutlined />
                      </span>
                      <span className="row-name">{n.name}</span>
                      {n.node_type && (
                        <span className="tag-mono tag-proto custom">
                          {nodeTypeLabel(n.node_type)}
                        </span>
                      )}
                    </div>
                  ))
                )}
              </Section>
            ),
          )}
        </SortableContext>
      </DndContext>
    </div>
  );
}

interface SectionProps {
  id: string;
  title: string;
  sub: string;
  count: number;
  extra?: React.ReactNode;
  children: React.ReactNode;
}

/** 可拖拽的分块(机场 / 自定义)。块内的行只读。 */
function Section({ id, title, sub, count, extra, children }: SectionProps) {
  const { t } = useTranslation();
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id,
  });
  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
    background: isDragging ? "var(--bg-subtle)" : undefined,
  };
  return (
    <div className="node-section" ref={setNodeRef} style={style}>
      <div className="node-section-head">
        <span
          className="row-grab"
          {...attributes}
          {...listeners}
          aria-label={t("common.drag")}
        >
          <HolderOutlined />
        </span>
        <span className="node-section-title">{title}</span>
        <span className="node-section-sub">
          {t("nodes.groupCount", { count })} · {sub}
        </span>
        <span style={{ flex: 1 }} />
        {extra}
      </div>
      <div className="node-section-body">{children}</div>
    </div>
  );
}
