import { Component, type ReactNode } from "react";

/** 页面渲染失败时保留侧栏和退出入口;调用方按路径换 key,导航后可恢复。 */
export default class PageErrorBoundary extends Component<{
  children: ReactNode;
  fallback: ReactNode;
}, { failed: boolean }> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  render() {
    return this.state.failed ? this.props.fallback : this.props.children;
  }
}
