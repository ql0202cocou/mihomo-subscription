// 路由表:/login 公开,其余页面统一套 RequireAuth(会话守卫)+ AppLayout(主框架)。
import { Navigate, Route, Routes } from "react-router-dom";
import AppLayout from "./components/AppLayout";
import RequireAuth from "./components/RequireAuth";
import Login from "./pages/Login";
import ProfileList from "./pages/ProfileList";
import ProfileDetail from "./pages/ProfileDetail";
import GlobalNodes from "./pages/GlobalNodes";
import RuleSets from "./pages/RuleSets";
import Settings from "./pages/Settings";

export default function App() {
  return (
    <Routes>
      <Route path="/login" element={<Login />} />
      <Route
        element={
          <RequireAuth>
            <AppLayout />
          </RequireAuth>
        }
      >
        <Route path="/" element={<ProfileList />} />
        <Route path="/profiles/:id" element={<ProfileDetail />} />
        <Route path="/nodes" element={<GlobalNodes />} />
        <Route path="/rules" element={<RuleSets />} />
        <Route path="/settings" element={<Settings />} />
        {/* 未知路径回到列表页(未登录会先被 RequireAuth 送去登录页) */}
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  );
}
