import { useCallback, useEffect, useRef, useState } from "react";
import { Link, useParams } from "react-router";
import {
  App as AntdApp,
  Button,
  Form,
  Input,
  Modal,
  Popconfirm,
  QRCode,
  Spin,
  Tabs,
} from "antd";
import {
  CopyOutlined,
  LeftOutlined,
  LinkOutlined,
  ReloadOutlined,
} from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { api, ApiError, errorMessage } from "../api";
import type { ProfileDetail as Detail, Regenerate } from "../types";
import NodesCard from "./detail/NodesCard";
import GroupsCard from "./detail/GroupsCard";
import RulesCard from "./detail/RulesCard";
import "../components/cards.css";
import "./detail/detail.css";

export default function ProfileDetail() {
  const { t } = useTranslation();
  const { message } = AntdApp.useApp();
  const { id } = useParams<{ id: string }>();
  const [detail, setDetail] = useState<Detail | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);
  const [generating, setGenerating] = useState(false);
  const [genErrors, setGenErrors] = useState<string[]>([]);
  const [genWarnings, setGenWarnings] = useState<string[]>([]);

  // 并发 reload 可能乱序返回:只采用最后一次发起的结果,避免旧 detail 覆盖新的。
  const reloadSeq = useRef(0);
  const reload = useCallback(async () => {
    if (!id) return;
    const seq = ++reloadSeq.current;
    try {
      const next = await api<Detail>(`/api/profiles/${id}`);
      if (seq === reloadSeq.current) setDetail(next);
    } catch {
      // 首屏失败给错误占位(而非白页);已有内容时保留当前页面。
      if (seq === reloadSeq.current) setLoadFailed(true);
    }
  }, [id]);

  // 保存类接口带回的离线重生成结果:校验错误与「刷新」的错误同处展示;已应用则清掉过时的错误。
  const onRegenerate = useCallback((r: Regenerate) => {
    if (r.status === "invalid") setGenErrors(r.errors ?? []);
    else if (r.status === "applied") setGenErrors([]);
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  if (!detail) {
    return loadFailed ? (
      <div className="page page-detail">
        <div className="empty-line">{t("common.loadFailed")}</div>
      </div>
    ) : null;
  }

  async function generate() {
    if (!id) return;
    setGenerating(true);
    setGenErrors([]);
    setGenWarnings([]);
    try {
      const res = await api<{ ruleset_conflicts?: string[] }>(`/api/profiles/${id}/generate`, {
        method: "POST",
      });
      message.success(t("detail.generateSuccess"));
      // 规则集冲突不阻断生成,后端照常产出,这里只作警告横幅展示。
      setGenWarnings(res.ruleset_conflicts ?? []);
      await reload();
    } catch (e) {
      if (e instanceof ApiError && e.details?.length) {
        setGenErrors(e.details);
        // 规则行错误只在「规则」tab 内展示(未激活的 tab 不渲染),这里必须给出提示,否则在其他 tab
        // 点刷新失败时毫无反馈。
        const ruleErrors = e.details.filter((d) => /rules line/.test(d)).length;
        message.error(
          ruleErrors > 0
            ? t("detail.generateFailedRules", { count: ruleErrors })
            : t("detail.generateFailed"),
        );
      } else message.error(errorMessage(e, t("detail.generateFailed")));
    } finally {
      setGenerating(false);
    }
  }

  // 生成错误分流:规则行级错误(`rules line …`)交给「规则」tab 内的 RulesCard 就地展示,
  // 其余非规则类错误在页面顶部以 banner 列出。
  const nonRuleErrors = genErrors.filter((e) => !/rules line/.test(e));

  const tabs = [
    {
      key: "basic",
      label: t("basic.title"),
      children: (
        <BasicInfo detail={detail} onRefresh={generate} onSaved={reload} refreshing={generating} />
      ),
    },
    {
      key: "nodes",
      label: t("detail.tabNodes"),
      children: (
        <NodesCard
          profileId={detail.id}
          profileName={detail.name}
          nodes={detail.nodes}
          generatedAt={detail.last_generated_at}
          onRegenerate={onRegenerate}
        />
      ),
    },
    {
      key: "groups",
      label: t("detail.tabGroups"),
      children: (
        <GroupsCard
          profileId={detail.id}
          groups={detail.groups}
          nodes={detail.nodes}
          generatedAt={detail.last_generated_at}
          onSaved={reload}
          onRegenerate={onRegenerate}
        />
      ),
    },
    {
      key: "rules",
      label: t("detail.tabRules"),
      children: (
        <RulesCard
          profileId={detail.id}
          initial={detail.rules?.content ?? ""}
          nodes={detail.nodes}
          groups={detail.groups}
          generatedAt={detail.last_generated_at}
          errors={genErrors}
          onSaved={reload}
          onRegenerate={onRegenerate}
        />
      ),
    },
    {
      key: "preview",
      label: t("preview.title"),
      children: <PreviewCard profileId={detail.id} />,
    },
  ];

  return (
    <div className="page page-detail">
      <Link to="/" className="detail-back">
        <LeftOutlined style={{ fontSize: 11 }} />
        {t("detail.back")}
      </Link>
      <div className="detail-head">
        <span className="detail-name">{detail.name}</span>
      </div>
      <div className="detail-context">{t("detail.context")}</div>

      <HostedLink detail={detail} onReset={reload} />

      {nonRuleErrors.length > 0 && (
        <div className="warn-banner error" style={{ marginBottom: 16 }}>
          {t("detail.invalidConfig")}:
          <ul style={{ margin: "4px 0 0", paddingLeft: 18 }}>
            {nonRuleErrors.map((e, i) => (
              <li key={i}>{e}</li>
            ))}
          </ul>
        </div>
      )}

      {genWarnings.length > 0 && (
        <div className="warn-banner" style={{ marginBottom: 16 }}>
          {t("detail.rulesetConflict")}:{genWarnings.join("、")}
        </div>
      )}

      <Tabs items={tabs} />
    </div>
  );
}

function HostedLink({ detail, onReset }: { detail: Detail; onReset: () => void }) {
  const { t } = useTranslation();
  const { message } = AntdApp.useApp();

  async function resetToken() {
    // 重置 token 会更换订阅 URL 并使旧链接立即失效(公开端点按 token 校验),故需 Popconfirm 二次确认。
    try {
      await api(`/api/profiles/${detail.id}/reset-token`, { method: "POST" });
      message.success(t("detail.generateSuccess"));
      onReset();
    } catch (e) {
      message.error(errorMessage(e, t("common.saveFailed")));
    }
  }

  async function copyUrl() {
    try {
      await navigator.clipboard.writeText(detail.subscription_url);
      message.success(t("detail.copied"));
    } catch {
      message.error(t("detail.copyFailed"));
    }
  }

  return (
    <div className="hero">
      <div className="hero-main">
        <div className="hero-title">
          <LinkOutlined />
          {t("detail.hostedLink")}
          <span className="pill">{t("detail.live")}</span>
        </div>
        <p className="hero-desc">{t("detail.alwaysLive")}</p>
        <div className="hero-url">{detail.subscription_url}</div>
        <div className="hero-actions">
          <Button type="primary" icon={<CopyOutlined />} onClick={copyUrl}>
            {t("detail.copy")}
          </Button>
          <Popconfirm title={t("detail.resetTokenConfirm")} onConfirm={resetToken}>
            <Button danger>{t("detail.resetToken")}</Button>
          </Popconfirm>
        </div>
      </div>
      <div className="hero-qr">
        <div className="hero-qr-frame">
          <QRCode value={detail.subscription_url} size={116} bordered={false} />
        </div>
        <span className="hero-qr-cap">{t("detail.scan")}</span>
      </div>
    </div>
  );
}

function BasicInfo({
  detail,
  onRefresh,
  onSaved,
  refreshing,
}: {
  detail: Detail;
  onRefresh: () => void;
  onSaved: () => void;
  refreshing: boolean;
}) {
  const { t } = useTranslation();
  const { message } = AntdApp.useApp();
  const [editOpen, setEditOpen] = useState(false);
  const [form] = Form.useForm();

  async function saveBasic(values: { name: string; source_url?: string }) {
    // 机场订阅 URL 为写敏感字段(响应恒脱敏、不回显):留空表示保持不变,填入则整体替换。
    const body: Record<string, unknown> = { name: values.name };
    const nextUrl = values.source_url?.trim();
    if (nextUrl) body.source_url = nextUrl;
    try {
      await api(`/api/profiles/${detail.id}`, { method: "PUT", body: JSON.stringify(body) });
      setEditOpen(false);
      onSaved();
    } catch (e) {
      message.error(errorMessage(e, t("common.saveFailed")));
    }
  }

  const rows: [string, string, boolean?][] = [
    [t("basic.name"), detail.name],
    [t("source.url"), detail.source_url_masked, true],
    [
      t("source.lastFetch"),
      detail.last_fetch_status
        ? `${detail.last_fetch_status} · ${detail.last_fetch_at ?? ""}`
        : t("source.never"),
    ],
  ];

  return (
    <div className="dcard">
      <div className="dcard-head">
        <span className="dcard-title">{t("basic.title")}</span>
        <div className="dcard-actions">
          <Button
            onClick={() => {
              form.setFieldsValue({
                name: detail.name,
                source_url: "",
              });
              setEditOpen(true);
            }}
          >
            {t("basic.edit")}
          </Button>
          <Button type="primary" icon={<ReloadOutlined />} loading={refreshing} onClick={onRefresh}>
            {t("source.refresh")}
          </Button>
        </div>
      </div>

      <div className="kv">
        {rows.map(([k, v, mono]) => (
          <div className="kv-row" key={k}>
            <div className="kv-key">{k}</div>
            <div className={`kv-val${mono ? " mono" : ""}`}>{v}</div>
          </div>
        ))}
      </div>

      <Modal
        title={t("basic.edit")}
        open={editOpen}
        onCancel={() => setEditOpen(false)}
        onOk={() => form.submit()}
        okText={t("common.save")}
        cancelText={t("common.cancel")}
      >
        <Form form={form} layout="vertical" onFinish={saveBasic}>
          <Form.Item name="name" label={t("basic.name")} rules={[{ required: true }]}>
            <Input />
          </Form.Item>
          <Form.Item name="source_url" label={t("source.newUrl")} extra={t("source.urlHint")}>
            <Input placeholder="https://..." />
          </Form.Item>
        </Form>
      </Modal>
    </div>
  );
}

function PreviewCard({ profileId }: { profileId: string }) {
  const { t } = useTranslation();
  const { message } = AntdApp.useApp();
  const [yaml, setYaml] = useState<string | null>(null);
  const [errors, setErrors] = useState<string[]>([]);
  const [loading, setLoading] = useState(false);

  async function load() {
    // 预览是只读的「试生成」:后端不落缓存、不改拉取状态(对应 src/generate.rs 的 preview)。
    setLoading(true);
    setErrors([]);
    try {
      setYaml(await api<string>(`/api/profiles/${profileId}/preview`));
    } catch (e) {
      // 校验失败时逐条列出(details),不只显示笼统的「Validation failed」。
      if (e instanceof ApiError && e.details?.length) {
        setYaml(null);
        setErrors(e.details);
      }
      message.error(errorMessage(e, t("detail.generateFailed")));
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="dcard">
      <div className="dcard-head">
        <span className="dcard-title">{t("preview.title")}</span>
        <Button type="primary" onClick={load} loading={loading}>
          {t("preview.load")}
        </Button>
      </div>
      {loading ? (
        <div className="preview-loading">
          <Spin />
          {t("preview.loading")}
        </div>
      ) : errors.length > 0 ? (
        <div className="warn-banner error">
          {t("detail.invalidConfig")}:
          <ul style={{ margin: "4px 0 0", paddingLeft: 18 }}>
            {errors.map((e, i) => (
              <li key={i}>{e}</li>
            ))}
          </ul>
        </div>
      ) : yaml ? (
        <pre className="preview-pre">{yaml}</pre>
      ) : (
        <div className="empty-line">{t("preview.load")}</div>
      )}
    </div>
  );
}
