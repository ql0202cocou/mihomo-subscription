// 管理员会话状态:启动时探活 /api/auth/session 恢复登录态;任何 401 经 api.ts 回调清空会话。
import {
  createContext,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from "react";
import { api, ApiError, setUnauthorizedHandler } from "./api";

interface AuthContextValue {
  user: string | null;
  loading: boolean;
  login: (username: string, password: string) => Promise<void>;
  logout: () => Promise<void>;
}

const AuthContext = createContext<AuthContextValue | null>(null);

export function AuthProvider({ children }: { children: ReactNode }) {
  const [user, setUser] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  // 探活当前会话;启动时的失败(未登录、会话过期、网络错误)按未登录处理。
  // strict 模式(登录成功后的会话确认)只容忍 401:其余失败如实抛回调用方,
  // 不能静默清空用户后当作成功。
  async function refresh(options?: { strict?: boolean }): Promise<void> {
    try {
      const session = await api<{ username: string }>("/api/auth/session");
      setUser(session.username);
    } catch (e) {
      setUser(null);
      if (options?.strict && !(e instanceof ApiError && e.status === 401)) throw e;
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    // API 返回任何 401 都清空会话;随后由 RequireAuth 负责跳转。
    setUnauthorizedHandler(() => setUser(null));
    void refresh();
    return () => setUnauthorizedHandler(null);
  }, []);

  async function login(username: string, password: string): Promise<void> {
    await api("/api/auth/login", {
      method: "POST",
      body: JSON.stringify({ username, password }),
    });
    await refresh({ strict: true });
  }

  async function logout(): Promise<void> {
    try {
      await api("/api/auth/logout", { method: "POST" });
    } catch (e) {
      // 401 已确认会话无效;网络错误/其他失败无法证明已撤销,交给界面提示重试。
      if (!(e instanceof ApiError && e.status === 401)) throw e;
    }
    setUser(null);
  }

  return (
    <AuthContext.Provider value={{ user, loading, login, logout }}>
      {children}
    </AuthContext.Provider>
  );
}

export function useAuth(): AuthContextValue {
  const ctx = useContext(AuthContext);
  if (!ctx) {
    throw new Error("useAuth must be used within AuthProvider");
  }
  return ctx;
}
