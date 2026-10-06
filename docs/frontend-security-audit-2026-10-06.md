# 前端安全审计 — 2026-10-06

基线：0.6.1，`main` 的 `e63cf9a` 加当前未提交的后端安全修复。审查 `web/src` 全部组件、
请求与认证层、构建配置和锁定依赖，并核对后端鉴权、机场数据进入前端的路径及 SPA 响应头。
初次审计未修改生产行为；下文为修复前的问题与证据。FE-01 至 FE-04 随后已在当前工作区修复，
尚未发布；修复与回归说明见文末。

| 编号 | 优先级 | 问题 | 必要条件 |
| --- | --- | --- | --- |
| FE-01 | P2 | 机场节点类型可触发 React 渲染崩溃 | 已配置的机场返回特制类型，管理员打开节点页 |
| FE-02 | P2 | 退出失败仍进入未登录界面，服务端会话可能继续有效 | 退出请求未到达服务器或服务器拒绝/失败 |
| FE-03 | P2 | 管理页没有防嵌入响应头，存在条件性点击劫持风险 | 反代没有补头，攻击者页面能在 iframe 内使用管理会话 |
| FE-04 | P2 | 切换订阅时旧详情与新操作目标混用 | SPA 内从详情 A 切到 B，B 的请求慢或失败 |

## FE-01：不受信任的类型触发继承属性查表

位置：`web/src/pages/detail/NodesCard.tsx:138`，标签定义在 `web/src/components/nodeSchema.ts:54`；
同类查表也用于自定义节点展示。

`NODE_TYPE_LABELS` 是普通 JavaScript 对象。`NODE_TYPE_LABELS[p.type] ?? p.type` 对 `__proto__`
取到的是继承的原型对象，而不是空值。React 无法把它当作文本子节点，抛出
`Objects are not valid as a React child`。当前入口没有错误边界。

机场只需返回：

```yaml
proxies:
  - { name: probe, type: __proto__, server: example.com, port: 443 }
rules:
  - MATCH,DIRECT
```

路由探针确认该输入可生成缓存，并经 `/api/profiles/:id/proxies` 把 `__proto__` 原样送到前端；
前端探针使用实际 `NodesCard` JSX 和已安装的 React 渲染器，确认渲染抛错。
这是机场可触发的管理界面可用性问题；没有证据表明可执行脚本或修改全局原型。

建议所有外部字符串查表使用 `Map` 或 `Object.hasOwn`，未知类型退回字符串；为主要页面加错误边界。
仅后端限制代理类型可能影响扩展协议兼容性，不宜作为唯一防线。

## FE-02：退出失败被呈现为已退出

位置：`web/src/auth.tsx:51-56`；调用方 `web/src/components/AppLayout.tsx:66-69`。

`logout()` 在 `finally` 中无条件清空 `user`。如果网络请求在到达后端前失败，后端没有执行
`sessions.remove()`，浏览器也没有收到清除 HttpOnly cookie 的 `Set-Cookie`；但 `RequireAuth`
已经把页面送回登录页。调用方没有捕获错误并提示“退出未完成”。网络恢复后，刷新页面会再次
通过 `/api/auth/session` 恢复旧登录态。在共享设备上，用户以为退出，后续使用者仍可进入后台。
正常会话空闲寿命为 7 天；仅更新本地状态不会撤销它。

受控探针运行实际 `AuthProvider`，让退出请求直接 reject，确认错误向外抛出但本地用户仍被清空。
该探针不声称所有网络失败都保留服务端会话：如果退出已完成而仅响应丢失，服务端可能已撤销。

建议成功收到退出响应后再呈现已退出；失败时明确提示并支持重试。收到 401 时可以按会话已经
无效处理。退出操作期间禁用重复提交，调用方负责捕获错误。

## FE-03：管理页缺少防嵌入保护

位置：`src/app.rs:226-260` 的 SPA 静态文件/兜底响应；`web/index.html` 也没有相关保护。

集成探针对 `/`、`/login` 和 `/settings` 的实际响应确认：没有 `X-Frame-Options` 或
`Content-Security-Policy`。这意味着应用自身未限制第三方 iframe 嵌入。

利用有条件：`SameSite=Lax` 通常阻止跨站 iframe 携带会话，不能简单宣称任意恶意网站都能操作
已登录后台。但相同 scheme 和注册域下的恶意兄弟子域，例如 `https://evil.example.com` 嵌入
`https://sub.example.com`，属于同站而不同源，Lax 并不能作为完整保护。iframe 内页面自身发起
操作时 Origin 是后台自己的 origin，后端的 Origin 校验不会阻止这种诱导点击。攻击者需要用户
交互，多次确认也会增加利用难度。实际 1Panel 反代是否已经补头，本次没有验证。

建议对管理 HTML 响应设置 `Content-Security-Policy: frame-ancestors 'none'` 和
`X-Frame-Options: DENY`；如有可信嵌入需求，使用明确的允许来源。`frame-ancestors` 必须通过
HTTP 响应头生效，不能靠 HTML meta。完整脚本 CSP 可以另外评估，尤其需要兼容 antd 动态样式。
机制参考：[MDN 点击劫持说明](https://developer.mozilla.org/en-US/docs/Web/Security/Attacks/Clickjacking)与
[MDN X-Frame-Options](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/X-Frame-Options)。

本次验证了响应头缺失，没有运行真实同站双域浏览器点击劫持攻击。

## FE-04：详情状态没有绑定当前路由 ID

位置：`web/src/pages/ProfileDetail.tsx:32-69` 和 `:77`。

相同详情路由切换参数时，组件会保留旧的 `detail`，直到新请求成功。页面及子表单用
`detail.id`（A），但刷新生成用路由 `id`（B）。因此页面显示 A，刷新却触发 B；编辑基本信息、
重置 token 等操作仍使用 A。新请求失败时，只设置 `loadFailed`；由于旧 `detail` 非空，错误
占位不会出现，旧内容可以一直保留。

组件探针模拟路由已经变成 B、详情状态仍为 A，确认基本信息表单收到 A，同时其刷新回调请求
`/api/profiles/B/generate`。这属于数据完整性与操作目标问题，没有证明跨管理员权限绕过。
常规“回列表再打开详情”会卸载组件，通常不会触发；浏览器前进/后退等详情之间的 SPA 导航可触发。

建议在 `detail.id !== id` 时显示加载/错误占位并阻止操作，或按路由 ID 重挂载详情组件，统一所有
动作的目标。同时处理旧请求失效和子组件状态复位。

## 验证与边界

- `npm run lint`、`npm run build` 通过。
- 2026-10-06 对锁定依赖执行实时 `npm audit --json`：总计 262 个依赖，已知漏洞 0。
  这不意味着不存在未知漏洞或业务逻辑风险。
- [前端受控复现探针](audits/frontend-security-probes-2026-10-06.cjs)：修复前三个问题复现成功，
  HTML 节点名负对照确认按文本转义。运行：`node docs/audits/frontend-security-probes-2026-10-06.cjs`。
- [后端入口探针](audits/frontend-security-probes-2026-10-06.rs)：1 个测试通过，确认特制类型进入
  前端响应，以及三个管理页面缺少防嵌入头。探针临时复制到 `tests/frontend_audit_probe.rs`，
  使用 `mod common;` 运行 `cargo test --test frontend_audit_probe`，执行后移回审计材料目录。
  探针通过表示当前风险已复现，不能作为修复验收标准。
- 未发现直接 HTML 注入、动态代码执行入口；二维码由本地组件绘制，没有向外部二维码服务传输
  订阅链接；`localStorage` 仅保存主题，会话由后端 HttpOnly cookie 管理；机场和远程规则集 URL
  在响应中脱敏，输入的新 URL 通过同源管理 API 提交。配置预览含节点凭据是管理员功能本身。
- 后端对受保护 API 实施会话校验，变更请求要求同源 Origin，不启用宽松 CORS；前端路由守卫
  不是实际授权边界。
- 未做线上攻击、真实浏览器端到端测试、反代部署检查或 Mihomo 客户端验证。缺少一般性 CSP、
  敏感管理响应的 `Cache-Control: no-store` 可列为后续加固，本次不把它们单独计为已复现漏洞。

## 修复与回归（2026-10-06）

- **FE-01**：统一使用 `nodeTypeLabel()`，只读取标签表的自有属性；未知类型仍显示原始文本。
  机场节点、自定义节点和类型选项共用该函数；管理内容区新增错误边界，故障时保留导航与退出。
- **FE-02**：退出成功或 `401` 后才清空用户；网络错误、`403`、`500` 保留会话状态。调用方捕获
  失败并明确提示重试，在途请求禁用退出按钮，同时用 ref 阻止同一渲染帧内的重复点击。
- **FE-03**：所有响应加 `Content-Security-Policy: frame-ancestors 'none'` 和
  `X-Frame-Options: DENY`，覆盖 SPA 深链、GET/HEAD、静态资源、`304` 及 API 错误响应。
  公共订阅仍返回相同 YAML；部署文档要求反代保留这两个头。
- **FE-04**：详情以路由 ID 重挂载，切换时不再显示旧表单。卸载后的重载/生成响应不更新状态、
  不提示或再次重载；相同订阅的请求仍按序号只采纳最新结果。

验收入口：`cd web && npm test` 与 `cargo test --test frontend_security`。
前端回归直接运行实际 TS/TSX 模块，控制网络、路由和 hooks 的状态/生命周期，使用实际 React
渲染器检查不可信类型与 HTML 转义；覆盖退出各类结果、重复点击、详情加载失败、旧请求迟到和
同订阅请求乱序。它不等同于真实浏览器端到端测试。后端回归使用临时 SPA 目录、SQLite 和假获取器，
无需已有 `web/dist` 或真实网络。历史探针保留旧漏洞断言，在修复后的代码上不作为验收命令使用。

当前工作区验收结果：前端 10 个安全回归、后端全部 136 个测试通过；`npm run lint` 无告警、
`npm run build`、`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings` 均通过。
本轮未新增或升级依赖，未部署或发布，也未进行真实浏览器/1Panel 端到端验证。
