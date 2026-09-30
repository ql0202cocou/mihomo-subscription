import { useState, type FormEvent } from "react";
import { useLocation, useNavigate, type Location } from "react-router-dom";
import { Button, Input } from "antd";
import { DeploymentUnitOutlined, ExclamationCircleFilled } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { useAuth } from "../auth";
import { ApiError } from "../api";
import "./Login.css";

export default function Login() {
  const { t } = useTranslation();
  const { login } = useAuth();
  const navigate = useNavigate();
  const location = useLocation();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  // 登录成功后回到被路由守卫拦截前的目标页(RequireAuth 经 location.state 传入);默认回首页。
  const from = (location.state as { from?: Location } | null)?.from?.pathname ?? "/";

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    setError(null);
    setSubmitting(true);
    try {
      await login(username, password);
      navigate(from, { replace: true });
    } catch (err) {
      // ApiError 带 status(401/429 等);无 status 的是网络层失败(fetch reject)。
      if (err instanceof ApiError) {
        setError(err.status === 429 ? t("login.tooMany") : t("login.failed"));
      } else {
        setError(t("login.networkError"));
      }
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <div className="login-screen">
      <form className="login-card" onSubmit={onSubmit}>
        <div className="login-head">
          <span className="login-logo">
            <DeploymentUnitOutlined />
          </span>
          <span className="login-title">{t("login.title")}</span>
          <span className="login-sub">{t("app.title")}</span>
        </div>

        {error && (
          <div className="login-error">
            <ExclamationCircleFilled />
            <span>{error}</span>
          </div>
        )}

        <div className="login-field">
          <label className="login-label" htmlFor="login-username">{t("login.username")}</label>
          <Input
            id="login-username"
            size="large"
            autoComplete="username"
            value={username}
            onChange={(e) => setUsername(e.target.value)}
          />
        </div>
        <div className="login-field">
          <label className="login-label" htmlFor="login-password">{t("login.password")}</label>
          <Input.Password
            id="login-password"
            size="large"
            autoComplete="current-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
        </div>

        <Button
          type="primary"
          htmlType="submit"
          block
          loading={submitting}
          className="login-submit"
        >
          {t("login.submit")}
        </Button>
      </form>
    </div>
  );
}
