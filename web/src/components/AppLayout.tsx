// 主框架:侧边导航 + 顶栏(当前页标题、主题切换)+ <Outlet/> 内容区。
import type { ReactNode } from "react";
import { Link, Outlet, useLocation, useNavigate } from "react-router-dom";
import { useTranslation } from "react-i18next";
import {
  ClusterOutlined,
  ControlOutlined,
  DeploymentUnitOutlined,
  FilterOutlined,
  FolderOutlined,
  LogoutOutlined,
} from "@ant-design/icons";
import { useAuth } from "../auth";
import { useTheme } from "../theme";
import "./AppLayout.css";

interface NavItem {
  key: string;
  path: string;
  label: string;
  icon: ReactNode;
  match: (pathname: string) => boolean;
}

export default function AppLayout() {
  const { t } = useTranslation();
  const { user, logout } = useAuth();
  const { mode, toggle } = useTheme();
  const navigate = useNavigate();
  const { pathname } = useLocation();

  const items: NavItem[] = [
    {
      key: "profiles",
      path: "/",
      label: t("nav.profiles"),
      icon: <FolderOutlined />,
      // 列表与详情同属「订阅配置」。
      match: (p) => p === "/" || p.startsWith("/profiles"),
    },
    {
      key: "nodes",
      path: "/nodes",
      label: t("nav.nodes"),
      icon: <ClusterOutlined />,
      match: (p) => p.startsWith("/nodes"),
    },
    {
      key: "rules",
      path: "/rules",
      label: t("nav.rules"),
      icon: <FilterOutlined />,
      match: (p) => p.startsWith("/rules"),
    },
    {
      key: "settings",
      path: "/settings",
      label: t("nav.settings"),
      icon: <ControlOutlined />,
      match: (p) => p.startsWith("/settings"),
    },
  ];

  const title = items.find((it) => it.match(pathname))?.label ?? t("nav.profiles");

  async function onLogout() {
    await logout();
    navigate("/login", { replace: true });
  }

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <span className="brand-tile">
            <DeploymentUnitOutlined />
          </span>
          <span className="brand-text">
            <span className="brand-title">Mihomo</span>
            <span className="brand-sub">{t("app.subtitle")}</span>
          </span>
        </div>

        <nav className="nav">
          {items.map((it) => (
            <Link
              key={it.key}
              to={it.path}
              className={`nav-item${it.match(pathname) ? " active" : ""}`}
            >
              <span className="nav-icon">{it.icon}</span>
              <span className="nav-label">{it.label}</span>
            </Link>
          ))}
        </nav>

        <div className="account">
          <span className="account-avatar">{(user ?? "A").slice(0, 1).toUpperCase()}</span>
          <span className="account-info">
            <span className="account-name">{user ?? "admin"}</span>
            <span className="account-role">{t("nav.role")}</span>
          </span>
          <button
            className="account-logout"
            onClick={onLogout}
            title={t("nav.logout")}
            aria-label={t("nav.logout")}
          >
            <LogoutOutlined />
          </button>
        </div>
      </aside>

      <div className="main">
        <header className="header">
          <div className="header-title">{title}</div>
          <button className="theme-toggle" onClick={toggle}>
            {mode === "light" ? <MoonIcon /> : <SunIcon />}
            <span>{mode === "light" ? t("theme.dark") : t("theme.light")}</span>
          </button>
        </header>
        <main className="content">
          <Outlet />
        </main>
      </div>
    </div>
  );
}

// 主题切换图标(@ant-design/icons 无 sun/moon,用内联 feather 风格 SVG)。
function MoonIcon() {
  return (
    <svg
      width="15"
      height="15"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
    </svg>
  );
}

function SunIcon() {
  return (
    <svg
      width="15"
      height="15"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <circle cx="12" cy="12" r="4" />
      <path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41" />
    </svg>
  );
}
