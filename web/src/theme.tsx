import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { App as AntdApp, ConfigProvider, theme as antdTheme } from "antd";
import type { ThemeConfig } from "antd";

// 暗色仅手动切换(不跟随系统);选择持久化到 localStorage,默认亮色。
export type ThemeMode = "light" | "dark";
const STORAGE_KEY = "mihomo-theme";

const FONT_FAMILY =
  '-apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", "Hiragino Sans GB", ' +
  '"Microsoft YaHei", Roboto, Helvetica, Arial, sans-serif';

// 品牌/语义色与 theme.css 的 CSS 变量保持一致;中性面色在暗色下显式给设计稿实测值,
// 让 AntD 组件与自定义元素的底色一致。
const SHARED = {
  colorSuccess: "#3F9E63",
  colorWarning: "#D9A036",
  colorError: "#D9534F",
  colorInfo: "#4C6EF5",
  borderRadius: 8,
  borderRadiusSM: 6,
  borderRadiusLG: 12,
  fontSize: 14,
  fontFamily: FONT_FAMILY,
};

const LIGHT_TOKEN = {
  ...SHARED,
  colorPrimary: "#D97757",
  colorBgLayout: "#F4F5F7",
  colorBgContainer: "#FFFFFF",
  colorBgElevated: "#FFFFFF",
  colorBorder: "#E6E6E6",
  colorBorderSecondary: "#F0F0F0",
};

const DARK_TOKEN = {
  ...SHARED,
  colorPrimary: "#E08C70",
  colorBgLayout: "#121316",
  colorBgContainer: "#1B1C1F",
  colorBgElevated: "#232427",
  colorBorder: "#303236",
  colorBorderSecondary: "#26282C",
};

const COMPONENTS: ThemeConfig["components"] = {
  Card: { borderRadiusLG: 12 },
  Button: { controlHeight: 32 },
  Tabs: { inkBarColor: "#D97757" },
};

interface ThemeContextValue {
  mode: ThemeMode;
  setMode: (mode: ThemeMode) => void;
  toggle: () => void;
}

const ThemeContext = createContext<ThemeContextValue | null>(null);

function initialMode(): ThemeMode {
  const saved = localStorage.getItem(STORAGE_KEY);
  return saved === "dark" || saved === "light" ? saved : "light";
}

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [mode, setMode] = useState<ThemeMode>(initialMode);

  useEffect(() => {
    document.documentElement.dataset.theme = mode;
    localStorage.setItem(STORAGE_KEY, mode);
  }, [mode]);

  const value = useMemo<ThemeContextValue>(
    () => ({
      mode,
      setMode,
      toggle: () => setMode((m) => (m === "light" ? "dark" : "light")),
    }),
    [mode],
  );

  return (
    <ThemeContext.Provider value={value}>
      <ConfigProvider
        theme={{
          algorithm: mode === "dark" ? antdTheme.darkAlgorithm : antdTheme.defaultAlgorithm,
          token: mode === "dark" ? DARK_TOKEN : LIGHT_TOKEN,
          components: COMPONENTS,
        }}
      >
        {/* antd App 上下文:message/notification 走 App.useApp(),静态函数在暗色主题下不生效 */}
        <AntdApp>{children}</AntdApp>
      </ConfigProvider>
    </ThemeContext.Provider>
  );
}

export function useTheme(): ThemeContextValue {
  const ctx = useContext(ThemeContext);
  if (!ctx) {
    throw new Error("useTheme must be used within ThemeProvider");
  }
  return ctx;
}
