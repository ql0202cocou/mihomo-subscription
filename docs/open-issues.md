# 待解决问题

> 代码审查发现的已知问题。基线：0.5.3（`4c49669`），审查日期 2026-09-29。
> 修复合并进 `main` 后从本文件移除对应条目，并在 `changelog.md` 的 `[Unreleased]` 记录。

---

## 已修复，待合并

以下修复在叠放的分支链上，均未推送：`main` ← `fix/generation-consistency` ← `fix/name-consistency` ←
`fix/fetch-hardening` ← `fix/rule-set-validation` ← `fix/web-audit`（最上层包含全部提交）。合并后删除本节。

| 条目 | 问题 | 提交 |
|------|------|------|
| 后端 1 | 规则就地重缝绕过校验，且不注入 `rule-providers` | `c01ae29` |
| 后端 2 | 重置 token / 公共路径前缀后，缓存中的规则集链接指向旧地址 | `c01ae29` |
| 后端 3 | `RULE-SET` 引用的规则集是否存在不做校验 | `332137c` |
| 后端 4 | 机场解析失败时 `last_fetch_status` 仍为 `success` | `183c79f` |
| 后端 5 | 全局节点库内 `name` 与 `content` 中的 `name` 可能不一致 | `5e99b13` |
| 后端 6 | 分组 `options` 可覆盖 `name` / `type` | `920dfab` |
| 后端 7 | 代理与分组重名、分组循环引用不做校验 | `332137c` |
| 后端 8 | 导入托管规则时 `policy` 未校验 | `19bca49` |
| 后端 9 | 远程规则集 `url` 写入期不做 SSRF 静态校验 | `e6ce4bb` |
| 后端 10 | 出站拉取：代理环境变量绕过 IP 钉定、DNS 无超时、每跳独立超时 | `e1977b1` |
| 后端 11 | 更新订阅时名称未校验 | `dd4b0ca` |
| 前端 1 | 高级键值行改 key 时撞上已有 key，旧值被清掉 | `d4b4fc3` |
| 前端 2 | 编辑 `RULE-SET` 规则固定发送 `cache: true`、不发 `enabled` | `2ab5191` |
| 前端 3 | 规则保存失败不回滚 | `86e1182` |
| 前端 4 | 只有规则行错误时，在其他页签点刷新毫无反馈 | `0ef2ab6` |
| 前端 5 | 高级行 YAML 编辑器错位、输入中途切换成单行输入 | `d4b4fc3`、`73691dc` |
| 前端 7 | 新增的高级行只能存字符串 | `d4b4fc3` |

- 后端 9 原有第二点「从 ② 导入到 ③ 时不沿用 ② 的启用状态」已判定**不是问题**：② 的 `enabled` 在界面上没有
  开关、② 也不参与生成；沿用它反而会让导入的规则集停用、追加的 `RULE-SET` 行校验失败。曾提交
  `9b7b3c2`，已由 `94f4945` 撤销。
- 后端修复带来的**升级提示**已写入 `changelog.md`：生成校验变严，此前「生成成功」但客户端无法加载的配置
  升级后会生成失败；新增迁移 `0012`；出站拉取不再读取代理环境变量。

---

## 前端

### 6. 保存成功提示与后端行为不符

- 位置：`web/src/i18n.ts` 各 `orderSaved`（如「规则顺序已保存并已应用到订阅。」）。
- 现象：规则/排序编辑后，后端离线重生成缓存是尽力而为——配置不合法时保留上一份合法输出，仍返回
  `204`。前端照常提示「已应用到订阅」，订阅实际仍是旧输出，错误要到点「刷新」才看得到。
- 待决定：保存类接口是否在响应中附带校验结果（仍 2xx、不阻止保存），前端据此显示警告。涉及 API 契约
  变更。

### 8. 保存后 reload 返回会强制关闭正在编辑的规则弹窗

- 位置：`web/src/pages/detail/RulesCard.tsx` `[initial]` effect（`setModalOpen(false)`）。
- 触发：拖拽或删除一条规则后立刻打开另一行编辑并输入 → PUT 与 reload 返回后弹窗关闭，输入丢失。

### 9. 排序请求没有先后序控制

- `GroupsCard.tsx`、`NodesCard.tsx`：失败回滚用闭包里的旧 `rows`；连续两次拖拽且第一次失败时，第二次的
  顺序在界面上被丢掉，但其 PUT 可能已成功。
- `GlobalNodes.tsx`、`RuleSets.tsx`：失败不回滚，靠 `finally load()` 纠正；连续拖拽时前一次 `load()` 会把
  界面短暂打回旧顺序。
- `ProfileDetail.reload` 并发响应无序号保护，乱序到达时旧 detail 覆盖新的；`global-nodes/order` 要重生成
  所有订阅、耗时长，放大了这个窗口。

### 10. 数值与校验错误的提示不完整

- `RuleSets.tsx`、`RulesCard.tsx` 的间隔小时 `InputNumber` 未设 `precision={0}`：输入 `1.5` 时后端反序列化
  失败，axum 返回纯文本 `422`，界面只显示「HTTP 422」。
- 预览（`ProfileDetail.tsx` `PreviewCard`）遇到校验失败只显示「Validation failed」，丢掉 `details`。

### 11. 编辑已有规则时，策略下拉被原值过滤

- 位置：`web/src/pages/detail/RulesCard.tsx` `RuleComposer` 的策略 `AutoComplete`（`filterOption`）。
- 现象：`AutoComplete` 按输入框当前文字做子串过滤；编辑已有规则时输入框预填原策略（如 `DIRECT`），下拉
  只剩同名选项，看起来没有 `REJECT` 等其他策略。清空输入框后才显示全部候选。
- 方向：打开下拉时不按原值过滤，用户开始输入后再过滤。

---

## 部署

### 部署主机开启 fake-ip 时，所有机场拉取都被拒绝

- 现象：主机上运行 Mihomo/Clash 的 TUN + fake-ip 模式时，所有域名都解析到 `198.18.0.0/15`。该网段在 SSRF
  阻止列表内，面板拉取任何机场都得到 `ssrf_rejected`。
- 发现：浏览器验证时本机 `provider.example`、`example.com` 均解析为 `198.18.x.x`。
- 方向：在 `deploy.md` 的反向代理/已知坑中说明——面板容器不要走 fake-ip DNS（或把面板排除在 TUN 之外）。
  不应为此放宽 SSRF 规则：放行该网段即放行一切被解析到它的地址。

---

## 仓库卫生

- 约 25 处源码注释引用已并入 `architecture.md` 的旧文档 `docs/api-design.md`、
  `docs/security-design.md`、`docs/data-model.md`（`grep -rn "api-design\|security-design\|data-model" src`）。
- `.github/workflows/ci.yml` 注释提到「release workflow」推送多架构镜像，但仓库只有 `ci.yml`；实际发布按
  `deploy.md` 手动 buildx。
- 部分历史提交作者为 `Your Name <your@email.com>`，提交机器未配置 git 身份。
- 远程 `dev` 分支落后 `main` 15 个提交且无独有提交，疑似废弃。
- `serde_yaml` 上游已弃用，迁移暂缓（见 `.cargo/audit.toml`）。
- 前端单包约 1.1 MB（`vite build` 提示超过 500 kB），未做代码分割。

---

## 审计记录

### 2026-09-29 后端审计

- 范围：`src/` 全部 22 个文件、`migrations/` 全部 11 个迁移。
- 已核对无问题：
  - SSRF：解析后校验并钉定 IP、重定向逐跳重查、IPv4 内嵌 IPv6 解包、按流字节限大小。
  - YAML 炸弹：锚点计数的字符集与 libyaml 锚点名规则（`[0-9A-Za-z_-]`）一致；`serde_yaml` 另有
    100 倍展开上限兜底。
  - 鉴权：凭据恒定时间比较；会话 ID 256 位熵并清扫过期；状态变更请求校验 Origin。
  - 限流：令牌桶实现；XFF 仅在 TCP 对端属可信网段时读取，取最右不受信跳。
  - 公开端点：前缀与 token 查询无论匹配与否都执行，恒定时间比较，失败统一 `404`。
  - SQL 全部参数绑定；日志不含机场 URL 与 token，HTTP trace 路径已打码。
- 共性：后端 1、3、7、8 同源——「生成成功」不等于「客户端可加载」。修复后编辑与生成走同一转换器
  （`regenerate_from_cache`），校验集中在 `converter::validate`。
- 验证：每个修复先写失败测试再修复；本地 `cargo fmt --check`、`clippy -D warnings`、`cargo test`（118）
  通过。`cargo audit` 与 Docker 构建未在本地运行，待 CI。「Mihomo 拒绝加载」仍基于对其解析逻辑的理解，
  未用真实客户端验证。

### 2026-09-29 前端审计

- 范围：`web/src` 全部文件（约 4500 行）；基线 `npm run lint`、`npm run build` 通过（本地以
  `npm_config_cache=web/.npm-cache` 运行）。
- 已核对无问题：`types.ts` 与后端结构体一致；敏感信息不泄露（`localStorage` 只存主题，机场 URL 只写不
  回显）；401 处理与登录；规则解析/序列化往返；后端新增的 `400`（URL、名称、policy）均显示服务端文案。
- 浏览器手动验证（本地服务 + 临时数据）：前端 1、5、7（高级行重名标红、已知字段不被覆盖、删行不错位、
  文本切 YAML 按 YAML 保存）、前端 2（`cache=false` 与 `enabled` 保留）、前端 3（失败回滚到服务端最近确认
  状态）通过；前端 4 仅验证了拉取失败路径，规则错误路径本地无法复现（机场拉取被 SSRF 拦截）。
