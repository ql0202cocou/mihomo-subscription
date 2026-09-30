import { useCallback, useEffect, useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { App as AntdApp, Button, Form, Input, Modal } from "antd";
import { CopyOutlined, PlusOutlined, RightOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { api, errorMessage } from "../api";
import type { ProfileSummary } from "../types";
import "./ProfileList.css";

export default function ProfileList() {
  const { t } = useTranslation();
  const { message } = AntdApp.useApp();
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [creating, setCreating] = useState(false);
  const [form] = Form.useForm();

  const load = useCallback(async () => {
    setLoading(true);
    try {
      setProfiles(await api<ProfileSummary[]>("/api/profiles"));
    } catch {
      message.error(t("common.loadFailed"));
    } finally {
      setLoading(false);
    }
  }, [message, t]);

  useEffect(() => {
    void load();
  }, [load]);

  async function onCreate(values: { name: string; source_url: string }) {
    try {
      await api<ProfileSummary>("/api/profiles", {
        method: "POST",
        body: JSON.stringify(values),
      });
      setCreating(false);
      form.resetFields();
      await load();
    } catch (e) {
      message.error(errorMessage(e, t("profiles.createFailed")));
    }
  }

  async function copyLink(url: string) {
    try {
      await navigator.clipboard.writeText(url);
      message.success(t("detail.copied"));
    } catch {
      message.error(t("detail.copyFailed"));
    }
  }

  const metrics = useMemo(() => {
    const total = profiles.length;
    // 拉取异常:有拉取记录且非 success(从未拉取的 null 不计为异常)。
    const errored = profiles.filter(
      (p) => p.last_fetch_status && p.last_fetch_status !== "success",
    ).length;
    return { total, errored };
  }, [profiles]);

  return (
    <div className="page">
      <div className="list-subhead">
        <span className="list-help">{t("profiles.help")}</span>
        <Button
          type="primary"
          icon={<PlusOutlined />}
          style={{ height: 38 }}
          onClick={() => setCreating(true)}
        >
          {t("profiles.create")}
        </Button>
      </div>

      <div className="metrics">
        <div className="metric">
          <div className="metric-label">{t("profiles.metricTotal")}</div>
          <div className="metric-value">{metrics.total}</div>
        </div>
        <div className="metric">
          <div className="metric-label">{t("profiles.metricError")}</div>
          <div className="metric-value" style={{ color: "var(--danger)" }}>
            {metrics.errored}
          </div>
        </div>
      </div>

      {loading ? (
        <div className="profiles">
          {[0, 1, 2].map((i) => (
            <div className="profile-bar" key={i}>
              <div className="profile-main">
                <div className="shimmer" style={{ height: 16, width: 140 }} />
                <div className="shimmer" style={{ height: 12, width: "60%", marginTop: 8 }} />
              </div>
              <div className="skeleton-right">
                <div className="shimmer" style={{ height: 32, width: 96 }} />
                <div className="shimmer" style={{ height: 32, width: 72 }} />
              </div>
            </div>
          ))}
        </div>
      ) : profiles.length === 0 ? (
        <div className="list-empty">{t("profiles.empty")}</div>
      ) : (
        <div className="profiles">
          {profiles.map((p) => (
            <ProfileBar key={p.id} profile={p} onCopy={() => copyLink(p.subscription_url)} />
          ))}
        </div>
      )}

      <Modal
        title={t("profiles.create")}
        open={creating}
        onCancel={() => setCreating(false)}
        onOk={() => form.submit()}
        okText={t("common.create")}
        cancelText={t("common.cancel")}
        destroyOnClose
      >
        <Form form={form} layout="vertical" onFinish={onCreate}>
          <Form.Item name="name" label={t("profiles.name")} rules={[{ required: true }]}>
            <Input />
          </Form.Item>
          <Form.Item
            name="source_url"
            label={t("profiles.sourceUrl")}
            rules={[{ required: true }]}
          >
            <Input placeholder="https://..." />
          </Form.Item>
        </Form>
      </Modal>
    </div>
  );
}

function ProfileBar({
  profile,
  onCopy,
}: {
  profile: ProfileSummary;
  onCopy: () => void;
}) {
  const { t } = useTranslation();
  // 新建后会自动拉取一次,故订阅恒带真实状态;无「未拉取」中间态(状态为空则不显示徽章)。
  const status = profile.last_fetch_status;
  const badge =
    status === "success"
      ? { label: t("profiles.fetchOk"), cls: "status-ok" }
      : status
        ? { label: t("profiles.fetchFail"), cls: "status-fail" }
        : null;

  return (
    <div className="profile-bar">
      <div className="profile-main">
        <div className="profile-name-row">
          <span className="profile-name">{profile.name}</span>
          {badge && (
            <span className={`profile-status ${badge.cls}`}>{badge.label}</span>
          )}
        </div>
        <div className="profile-url">{profile.source_url_masked}</div>
      </div>
      <div className="profile-actions">
        <button className="btn-ghost" onClick={onCopy}>
          <CopyOutlined />
          {t("profiles.copyLink")}
        </button>
        <Link to={`/profiles/${profile.id}`} className="btn-manage">
          {t("profiles.open")}
          <RightOutlined style={{ fontSize: 11 }} />
        </Link>
      </div>
    </div>
  );
}
