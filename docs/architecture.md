# 架构参考

> API 契约、数据模型、安全设计——后端实现的权威参考。已实现，取舍见 `changelog.md`。

---

## API 设计

### 原则

- 管理 API 挂 `/api`、需登录会话、用 JSON；公开订阅端点无需登录（需随机路径前缀 +
  per-profile token）、输出 Mihomo YAML。
- 公开端点任何校验失败统一 `404`（不泄因，含路径参数解码失败）；管理 API 默认对机场 URL 脱敏。
- 管理 API **同源**（SPA 由 Axum 同源提供，不启用 CORS；见安全设计节）。
- 管理 API 的 JSON 请求体提取失败（语法错误、类型不符、Content-Type 不符）统一返回 `400`
  错误信封，不暴露框架默认的纯文本/状态码差异。
- 管理 API 与公开订阅/规则集端点的响应统一带 `Cache-Control: no-store`；SPA 静态资源与
  `index.html` 不继承该头（发版后浏览器可继续使用缓存的前端分块）。
- 管理请求体 ≤1MB（超限 `413`）；管理员提交的节点/分组 YAML 与机场内容用相同别名/嵌套限制解析。

### 约定

- 时间 RFC 3339 UTC；标识符 UUID v4；编码 `application/json`（公开端点 `text/yaml`）。

错误响应：`{ "error": { "code", "message", "details": [...] } }`。

| Code | 含义 |
|------|------|
| 200/201/204 | 成功（读/建/无内容） |
| 400 | 请求体格式错或校验失败 |
| 401 | 未登录/会话过期 |
| 404 | 资源不存在；公开端点统一失败响应 |
| 409 | 名称冲突（如重名） |
| 413 | 请求体超限 |
| 429 | 限流 |
| 500 | 内部错误（不含敏感信息） |

### 认证

凭据来自 `ADMIN_USERNAME` / `ADMIN_PASSWORD`（compose 环境变量）。登录签发会话 Cookie
（`HttpOnly`、`SameSite=Lax`，HTTPS 加 `Secure`）；登录按来源 IP 限流（成功/失败请求均计数）。除 `/health`、
登录、公开端点外，所有路由需有效会话（否则 `401`）。所有状态变更请求（含登录/登出）必须带同源
`Origin`；生产环境按 `PUBLIC_BASE_URL` 的完整 origin（scheme + host + port）校验，否则 `403`。

```text
POST /api/auth/login  {username,password} -> 204+Set-Cookie | 401 | 429
POST /api/auth/logout -> 204 清除 cookie
GET  /api/auth/session -> 200 {username} | 401
```

### 端点总览

| Method | Path | 鉴权 | 说明 |
|--------|------|------|------|
| GET | `/health` | 否 | 健康检查 |
| POST | `/api/auth/login` | 否 | 登录 |
| POST | `/api/auth/logout` | 是 | 登出 |
| GET | `/api/auth/session` | 是 | 当前会话 |
| GET / POST | `/api/profiles` | 是 | 配置列表 / 创建 |
| GET / PUT / DELETE | `/api/profiles/:id` | 是 | 配置详情 / 更新 / 删除 |
| PUT | `/api/profiles/:id/rules` | 是 | 替换自定义规则 |
| GET | `/api/profiles/:id/provider-rules` | 是 | 拉取机场原始 `rules`（实时，不缓存） |
| GET | `/api/profiles/:id/proxies` | 是 | 节点/分组预览（生成输出中的代理与分组，只读） |
| PUT | `/api/profiles/:id/node-section-order` | 是 | 两个节点块的先后（`["provider","custom"]` 排列） |
| PUT | `/api/profiles/:id/group-order` | 是 | 分组顺序（分组名数组） |
| GET / POST | `/api/global-nodes` | 是 | 全局自定义节点池（跨订阅共享，自动追加到每条配置） |
| PUT / DELETE | `/api/global-nodes/:id` | 是 | 单个全局节点 |
| PUT | `/api/global-nodes/order` | 是 | 全局自定义块顺序；立即重排所有配置缓存 |
| GET / POST | `/api/rule-sets` | 是 | 全局用户规则库（② 模板 / 导入源；不托管、不参与生成） |
| PUT / DELETE | `/api/rule-sets/:id` | 是 | 单个全局规则集 |
| PUT | `/api/rule-sets/order` | 是 | 全局库显示顺序（仅展示序） |
| GET / POST | `/api/profiles/:id/rule-sets` | 是 | 订阅自有规则库（③）；生成只读此处 |
| PUT / DELETE | `/api/profiles/:id/rule-sets/:rsid` | 是 | 单个订阅规则集 |
| POST | `/api/profiles/:id/rule-sets/import` | 是 | 从全局 ② 复制规则集进本订阅 ③ + 追加 `RULE-SET` 规则行 |
| GET / POST | `/api/profiles/:id/groups` | 是 | 自定义分组 |
| PUT / DELETE | `/api/profiles/:id/groups/:group_id` | 是 | 单个分组 |
| POST | `/api/profiles/:id/import-provider-groups` | 是 | 导入机场 `proxy-groups` 为可编辑自定义分组（实时，跳过同名/不支持类型） |
| GET | `/api/profiles/:id/preview` | 是 | 预览生成的 YAML |
| POST | `/api/profiles/:id/generate` | 是 | 校验并生成 |
| POST | `/api/profiles/:id/reset-token` | 是 | 重置该配置 token（响应含 `regenerate`，见「Token 轮换」） |
| GET | `/api/settings` | 是 | 应用设置 |
| POST | `/api/settings/reset-public-path` | 是 | 重置公共路径前缀 |
| GET | `/:public_path_prefix/api/sub/:token` | 否 | 公开订阅下载 |
| GET | `/:public_path_prefix/api/sub/:token/r/:name/:file` | 否 | 公开规则集托管（③，按订阅 token 隔离；`:file` = `<behavior>.<format>`） |

### Profile 资源

列表返回摘要，详情返回完整对象。`source_url` 默认脱敏，仅创建/更新接受完整值。详情的
`nodes` 是**全局节点池的只读快照**（各配置一致，编辑/排序走 `/api/global-nodes`），供详情
展示与分组/规则引用建议。

```json
{
  "id": "...", "name": "My Profile",
  "source_url_masked": "https://example.com/api/sub?token=***",
  "output_type": "mihomo",
  "subscription_url": "https://sub.example.com/<prefix>/api/sub/<token>",
  "last_generated_at": "...", "last_fetch_at": "...", "last_fetch_status": "success",
  "reality_mlkem": true,
  "reality_fingerprint_risks": ["vless-hk"],
  "rules": { "content": "...\nMATCH,Proxy", "updated_at": "..." },
  "nodes": [ { "id": "...", "name": "my-ss", "node_type": "ss", "enabled": true } ],
  "groups": [ { "id": "...", "name": "MyGroup", "group_type": "select",
               "members": ["my-ss","DIRECT"], "enabled": true } ],
  "created_at": "...", "updated_at": "..."
}
```

- 创建：`{name, source_url}`。创建成功后**同步触发一次生成/拉取**（尽力而为），故新订阅
  立即带有真实 `last_fetch_status`，不存在「未拉取」中间态。创建与更新的 `name` 均去除首尾空白，
  空名 `400`。
- `source_url` 写时静态校验（http/https、无内嵌凭据、非本地/私有地址），否则 `400`；真正 SSRF
  在拉取时按 DNS 解析 + IP 固定。
- 更新（`PUT`）改动 `source_url` 即视为换源：更新在 per-profile 写锁内、单个事务中完成——换源时
  递增 `source_revision`、删除旧缓存（不回退到换源前的输出）、清空公开刷新观测一并提交，随后
  同步触发一次重拉（尽力而为，`PUT` 等待其完成）。拉取观测、公开
  刷新时间与缓存回写均绑定写入时的 `source_revision`，换源前发起的在途拉取结果一律丢弃；换源
  期间并发的生成请求可能因此返回 `409`，重试即可。
- `reality_mlkem`（布尔，可选，缺省保持不变）：是否为未显式设置 `support-x25519mlkem768` 的 REALITY
  节点自动补开（见「生成与校验」）。未换源时改动它会用缓存的机场原文离线重生成，响应另带
  `regenerate`（见「节点/分组预览与排序」）；换源时随重拉一并生效。
- 详情的 `reality_fingerprint_risks`：当前缓存输出里 ML-KEM 已开启但 `client-fingerprint` 不是
  `chrome` 的 REALITY 节点名（无缓存或无风险为空数组），详情页据此常驻告警。
- `last_fetch_status`：`success` / `http_error:<code>` / `ssrf_rejected` / `timeout` / `too_large` /
  `provider_parse`（拉取成功但内容不是可解析的 Mihomo YAML），从未拉取为 `null`。

请求体（节点走全局池，分组按配置）：

```text
POST /api/global-nodes { name, node_type, content, enabled? }  # name 全局唯一（重名 409）；content 为完整 Mihomo proxy 映射 YAML
POST /api/profiles/:id/groups { name, group_type, members, options?, enabled? }
```

- 节点 `content` 保存时结构校验，生成时并入**每条配置**输出的 `proxies`，其中 `name` 以库内 `name` 为准
  （覆盖 content 中缺失或不一致的值）；`PUT` 同体整体替换。节点与分组的 `name` 不得占用 Mihomo 内置
  保留名（见「生成与校验」），否则 `400`。
- 全局节点为单一共享池：新建落末尾、`name` 全局唯一；增删改在下次生成（公共链接每拉取即重生）
  进入各配置输出，排序见下立即生效。

### 规则集库（三规则库模型）

规则下发与订阅**解耦**为三个库：① 机场原始规则（`GET .../provider-rules`，「导入机场规则」）、
② 全局用户规则库（`/api/rule-sets`）、③ 每订阅托管规则库（`/api/profiles/:id/rule-sets`）。**下发只读
③**；①② 仅作导入源。

#### ② 全局用户规则库（模板 / 导入源）

```text
GET    /api/rule-sets                                  # 列表；每项含 count、source、remote_url_masked、cache（无托管链接）
POST   /api/rule-sets   { name, behavior, source?, format, ... }
   # name 全局唯一（重名 409）且限 [A-Za-z0-9._-]；behavior∈domain/ipcidr/classical；source∈manual（默认）/remote
   # source=manual: { content }                 format∈yaml/text
   # source=remote: { url, interval_hours?=24, cache?=true }   format∈yaml/text/mrs；url 须 http(s)，cache=true（面板代为拉取）时另做与 source_url 相同的 SSRF 静态校验，cache=false 时由客户端拉取、允许局域网地址
PUT    /api/rule-sets/:id   { ...同上 }                 # remote 编辑 url 留空则沿用原值（已脱敏不回显）
DELETE /api/rule-sets/:id
PUT    /api/rule-sets/order { order: [规则集名] }        # 仅展示序，未列出落末尾
```

② 不再公开托管、不再参与生成，仅是可复用模板。通过 ③ 的导入端点复制进某订阅后才生效。

#### ③ 每订阅托管规则库（下发来源）

```text
GET    /api/profiles/:id/rule-sets                     # 列表；每项含 url（按订阅 token 隔离的托管链接）、count、source、remote_url_masked、cache、last_fetch_status
POST   /api/profiles/:id/rule-sets   { name, behavior, source?, format, ... }   # name 在本订阅内唯一（重名 409）；字段同 ②。新建/更新响应为最新定义，改动影响所服务输出时另带 regenerate 字段
PUT    /api/profiles/:id/rule-sets/:rsid   { ...同上 }
DELETE /api/profiles/:id/rule-sets/:rsid
POST   /api/profiles/:id/rule-sets/import  { names: [②规则集名], policy }   # names 最多 1000 个（逐个 trim、去空、按规则集名同一约束校验、保序去重，超限或含非法名 400）。在 per-profile 锁内以单个 BEGIN IMMEDIATE 事务复制 ② 定义进 ③（含真实远程 URL）+ 为导入后本订阅实际存在定义且尚未引用的名插入 RULE-SET,<name>,<policy> 行（插到最后一条非注释 MATCH 兜底行之前，无 MATCH 则追加末尾；② 中不存在、未能复制的名字不追加；policy 须为单个非空名字，不含逗号/换行，否则 400 且不写入）；规则文本以读取时的旧内容为条件写回（CAS），并发规则编辑导致冲突时整体回滚、返回 409（重试即可）；返回 { imported, regenerate, rules_content }（imported 为实际复制进 ③ 的定义数，已存在同名的不计；复制了定义或新增了规则行都会触发离线重生成，都没有时 regenerate 为 null；rules_content 为导入提交后的完整规则文本，无改动时保留原文，供前端同步编辑状态）
```

- manual / remote 行为与 ② 一致（校验/渲染/镜像同一套逻辑）。托管在按订阅 token 隔离的链接
  `/<prefix>/api/sub/<token>/r/<name>/<behavior>.<format>`；remote 关缓存则转换时直接注入上游 URL。
- 被本订阅 `RULE-SET,<name>` 规则引用时注入指向该托管链接的 `rule-providers:` 条目（公共链接每拉取即
  重生，故定义改动即时生效）；未被引用不注入。
- 注入时若名与机场 `rule-providers` 已有条目**撞名**，用本订阅托管版**覆盖**机场版；生成端点
  （`POST .../generate`）在响应 `ruleset_conflicts` 列出撞名的名字，详情页据此告警，避免静默替换。
- 引用保护：规则集被本订阅当前规则文本或生成缓存的规则快照（`rules_snapshot`；`0014` 前无快照的
  缓存改从 `output_yaml` 的 `rules` 段判断，无法判断时按被引用处理）以 `RULE-SET,<name>` 引用时，
  对其改名、停用（`enabled=false`）或删除返回 `409`；须先移除引用。③ 的写操作（增删改/导入）都在
  与生成/公开刷新共用的 per-profile 锁内完成读改写。
- 更新规则集总会清空镜像缓存（下次拉取重新回源）。改动已注入输出的字段（`behavior`/`format`/
  `source`/`cache`、remote 关缓存时的直连 `url`、启停，或引用状态变化使条目出现/消失）时，同一事务
  先把 `generated_cache.capability_hash` 置 NULL（标脏，此后公开端点不再复用该缓存），提交后离线
  重生成；无法重生成（无机场原文或新配置不合法）则删除缓存，下次公开拉取回源重建。纯 manual
  内容/镜像间隔不影响输出，不做无谓重生成；新建补齐了已被引用名字的定义时同样离线重生成。

### 节点/分组预览与排序

```text
GET /api/profiles/:id/proxies
{ "generated": true, "generated_at": "...",
  "proxies": [{ "name":"hk-1","type":"ss" }, { "name":"my-ss","type":"ss" }],
  "node_section_order": ["provider","custom"],
  "groups": [{ "name":"Proxy","type":"select" }] }
```

只读，解析自 `generated_cache.output_yaml`，直接返回缓存当前内容（排序改动会离线重生成缓存）；
未生成、或缓存的来源/公开能力上下文与当前不一致（`0017` 前旧缓存、换源/轮换残留、已标脏）时
返回 `generated:false` + 空数组。`proxies` = 机场块 + 自定义块按 `node_section_order` 拼接，
前端据全局节点名集合拆成两块渲染（机场只读，自定义在「节点配置」排序）；`groups` 全为自定义
分组。两者也作分组成员候选。

```text
PUT /api/global-nodes/order               { order: [节点名] }              # 全局，未列出落末尾；重写为 0..n-1 -> 204
PUT /api/profiles/:id/node-section-order  { order: ["custom","provider"] } # per-profile，必须是排列
PUT /api/profiles/:id/group-order         { order: [分组名] }              # per-profile
-> 200 { "regenerate": { "status": "applied" | "pending" | "invalid", "errors"?: [..] } }
```

- 自定义块顺序由全局 `global-nodes/order` 决定（作用所有配置）；两块先后由 per-profile
  `node-section-order`；分组顺序由 per-profile `group-order`。名字超长/数组过大 `400`。
- 这些端点保存后用缓存中的机场原文（`generated_cache.provider_yaml`）**离线重跑完整转换、无需重拉
  机场**，改动**立即生效**（预览与公共链接随即反映）；全局排序重生成**每条配置**缓存，无缓存者
  首次生成时生效。
- 规则拖拽同理：规则顺序即语义（命中即止），存为 `rulesets.content` 有序文本，前端经
  `PUT .../rules` 整体保存，同样离线重生成、立即生效，响应同上。编辑器将最后一条 `MATCH` 钉底，
  较早的 `MATCH` 保留在普通列表中，不在加载时归并或丢弃。
- 前端 `RULE-SET` 编辑器默认只保存引用；显式开启本地定义维护并新建或修改定义字段时才写入 ③。
  托管导入与规则保存共用前端串行队列，导入期间暂停其他规则编辑，完成后按响应的 `rules_content`
  同步本地规则，避免在途完整保存覆盖导入结果。
- 离线重生成与 `generate` 走同一转换器（含校验与 `rule-providers` 注入）；所有写缓存的路径（生成、新建时的
  自动拉取、公开刷新、离线重生成）共用 per-profile 锁；保留 `generated_at`（回源节奏不变）。
- per-profile 的保存接口（`rules`、`node-section-order`、`group-order`、`rule-sets/import`）在响应的
  `regenerate` 如实说明改动是否已下发：`applied` 已重写缓存；`invalid` 校验失败，编辑照常入库但缓存保留
  上一份合法输出，`errors` 为与「生成」一致的逐条错误；`pending` 没有机场原文（未生成或 `0012` 之前的旧
  缓存）或重生成出错，下次回源后生效。保存本身始终成功（2xx），前端据 `status` 提示成功或警告。
- 每次生成把输出的分组顺序快照回写 `group_order`（新增分组落末尾）；节点顺序为全局
  `global_nodes.position`，不 per-profile 快照，机场块恒上游序。
- 节点/分组均结构化表单录入（节点常用字段 + 高级 KV；分组按类型给选项 + 高级 KV；成员从候选
  下拉选），前端分别序列化为节点 `content` YAML 与分组 `options` JSON。
- 桌面表单弹窗按视口高度限高，标题和底部操作区保持可见，正文超高时内部滚动；节点、分组、规则集等
  长表单按弹窗实际可用宽度显示一至三列，短表单与顺序式规则编辑保持单列。

### 生成与校验

- `POST .../generate` 完整校验，成功刷新缓存并返回托管链接，失败 `400` + 逐条错误（对应弹窗
  文案）。详情页「原始订阅源」手动刷新复用本端点。
- `GET .../preview` 是只读版：缓存兼容（来源与公开能力均匹配，见「缓存与刷新」）且新鲜则返回，
  否则实时拉取生成；不写缓存、不动 `last_*`。与所有写缓存路径共用同一 per-profile 锁，避免
  旧正文搭配新配置。
- 校验：分组类型限 `select`/`url-test`/`fallback`/`load-balance`（当前 Mihomo 已移除 `relay`，
  创建/导入/生成均拒绝；迁移 `0018` 已把遗留 relay 分组停用并使相关订阅缓存失效）；机场
  `proxies`、自定义节点、自定义分组各自区内重名拒绝；规则引用的分组须存在于已启用自定义分组；
  `RULE-SET,<name>` 的规则集须存在于本订阅已启用的
  ③ 或机场自带 `rule-providers`，逻辑规则（`AND`/`OR`/`NOT`/`SUB-RULE`）内嵌套的 `RULE-SET` 引用同样
  检查（镜像 Mihomo 的括号解析，嵌套超过 16 层报校验错误）；自定义分组名可与机场分组重名（机场分组
  整体替换），但不得与任何代理（机场节点/自定义节点）重名；分组无成员时须开启
  `include-all`/`include-all-proxies`；显式 `use` 或 `include-all-providers` 因输出剥离
  `proxy-providers` 不可能再解析而拒绝；`empty-fallback` 须指向已知的非分组代理；分组成员须引用
  存在的机场节点（透传）/
  启用自定义节点/启用自定义分组，且分组间引用不得成环；节点（含机场透传代理与自定义节点）与自定义
  分组名不得占用 Mihomo 内置保留名——节点侧 `DIRECT`/`REJECT`/`REJECT-DROP`/`PASS`/`PASS-RULE`/
  `COMPATIBLE`/`GLOBAL`，分组侧同列除 `GLOBAL`（自定义 `GLOBAL` 分组是 Mihomo 支持的用法）；输出中
  所有 `rule-providers` 的 `path` 必须唯一，冲突报校验错误；输出须
  合法 Mihomo YAML。成功响应 `{ subscription_url, generated_at }`。

顶层键处理（转换器逐键显式处理）：

| 键 | 处理 |
|-----|------|
| `proxies` | 机场块（上游序）+ 自定义块（启用全局节点按 `global_nodes.position` 排），按 `node_section_order` 拼接；REALITY 节点（`reality-opts.public-key` 非空）未设置 `support-x25519mlkem768`（或值为 null）且订阅 `reality_mlkem` 开启时补 `true`，显式值不动；键名按 mihomo 规则匹配（`_` 视作 `-`、忽略大小写）。ML-KEM 生效而 `client-fingerprint` 缺失或非 `chrome` 的节点名随缓存保存（`generated_cache.reality_fingerprint_risks`），名单变化时记日志 |
| `proxy-groups` | 整体替换为启用的自定义分组（机场分组不透传，需「导入机场分组」），按 `group_order` 重排 |
| `rules` | 整体替换为用户规则 |
| `rule-providers` | 机场原样透传；另把被 `RULE-SET` 规则（含逻辑规则内嵌套）引用、本订阅自有（③）规则集合并在上（同名覆盖，指向按订阅 token 隔离的托管链接）。注入条目的 `path` 为 `./ruleset/<name>-<摘要>.<format>`（摘要取所指向 URL 的 SHA-256 前 8 字节 hex），使不同订阅 token 或不同上游地址的同名规则集不会在客户端共享本地缓存文件 |
| `proxy-providers` | **剥离**（会让客户端拉取绕过 SSRF/缓存的 URL，可能暴露机场 URL） |
| 其余（`port`/`dns`/`tun`/`sniffer`…） | 原样透传（新 Mihomo 选项无需改转换器） |

- 机场 YAML 的 merge key（`<<`）在解析阶段即按预算归一化展开（显式键优先，多源序列取先出现者），
  转换器只处理展开后的值（限界见「安全设计 → 不受信任内容」）。
- `import-provider-groups`：实时拉取机场，把 `proxy-groups` 解析为可编辑自定义分组写入
  （`name`/`type`/`proxies`→成员，其余→`options`；跳过同名/不支持类型/占用 Mihomo 内置保留名者），
  返回 `{imported,skipped}`；只改 `custom_groups`，需重新生成才进输出；鉴权 + SSRF 同 `provider-rules`。
- 规则集：除透传机场自带 `rule-providers` 外，面板按 `RULE-SET,<name>,<policy>` 注入**本订阅自有规则库
  （③）**中同名条目，指向按订阅 token 隔离的托管链接；`<name>` 也可仍引用机场条目名。逻辑规则
  （`AND`/`OR`/`NOT`/`SUB-RULE`）内嵌套的 `RULE-SET` 引用同样计入注入集合。全局 ② 库不参与生成。

### 公开订阅端点

```text
GET /:public_path_prefix/api/sub/:token
  -> 200 text/yaml   有效前缀 + token
  -> 503             有效但无缓存且上游拉取失败（通用）
  -> 404             其余一切（统一，含路径参数解码失败）
```

响应头：`content-type: text/yaml`、`content-disposition: attachment; filename="<name>.yaml"`、
`subscription-userinfo`（从机场透传，无则省略）、`profile-update-interval: 24`（小时，MVP 固定）、
`cache-control: no-store`。

- 缓存复用/回源/兜底与 single-flight 合并的策略见「缓存与刷新」节。无「生成配置」按钮；
  响应/错误绝不含机场 URL；按来源 IP 限流。

兼容：早期 `/api/v1/subscriptions*`、`/api/v1/merged` 已移除，无兼容层。

### 公开规则集托管端点

```text
GET /:public_path_prefix/api/sub/:token/r/:name/:file
  -> 200 text/plain | application/octet-stream(mrs)   前缀+token 匹配 + 名在该订阅存在且启用 + :file=="<behavior>.<format>" + 已托管
  -> 503             remote 镜像首拉失败且无缓存
  -> 404             其余一切（统一，含前缀错/token 错/名不存在/未启用/文件名不符/remote 关缓存未托管/路径参数解码失败）
```

- 托管本订阅自有规则库（③），**按订阅 token 隔离**，与订阅端点共用 `public_path_prefix`（重置公共路径同样
  使其失效）。规则集是规则清单、非私密，按名可枚举可接受；仍按来源 IP 限流。全局 ② 库不再公开托管。
  响应与订阅端点一样带 `cache-control: no-store`。
- manual：`yaml`→Mihomo `payload:` 列表、`text`→逐行（均忽略空行与 `#` 注释）。remote（`cache=1`）：
  single-flight 懒刷新——缓存超 `interval_hours` 才回源（SSRF 安全字节），失败回退旧缓存、无缓存 `503`；
  字节原样托管（`mrs` 为 `application/octet-stream`）。remote（`cache=0`）不在此托管，统一 `404`。

---

## 数据模型

> SQLite 模式由 `migrations/` 实现；连接池按本文档对**每个**连接应用 pragma。

### 存储约定

- DB：`${DATA_DIR}/mihomo-subscription.db`（`DATA_DIR` 默认 `/data`）。
- 每连接 pragma：`foreign_keys = ON`、`busy_timeout = 5000`——**每连接**生效，须在连接池
  after-connect 钩子里对每个连接设（只设一次会让其余连接外键静默失效、留孤儿行）；busy_timeout
  让并发写在锁上等待而非抛 `SQLITE_BUSY`。`journal_mode = WAL` 设一次随文件持久化。
- 主键 UUID v4（`TEXT`）；时间戳 RFC 3339 UTC（`TEXT`）；布尔 `INTEGER` 0/1；结构化字段 JSON 存 `TEXT`。

### 实体关系

```text
app_settings (单行)
global_nodes (全局池，不挂任何 profile)

profiles 1 ── 1 rulesets
profiles 1 ── * custom_groups
profiles 1 ── * profile_rule_sets
profiles 1 ── 1 generated_cache
```

`global_nodes` 是**跨订阅共享**的自定义节点池，自动追加到每条 profile 输出，不与 profile 外键
关联（删 profile 不影响它）。

### 表定义

#### app_settings

应用级设置；`public_path_prefix` 支持运行时重置（故存库），首启时空则从 `PUBLIC_PATH_PREFIX`
初始化，否则随机生成。

```sql
CREATE TABLE app_settings (
    id                  INTEGER PRIMARY KEY CHECK (id = 1),
    public_path_prefix  TEXT    NOT NULL,
    updated_at          TEXT    NOT NULL
);
```

#### profiles

```sql
CREATE TABLE profiles (
    id          TEXT    PRIMARY KEY,
    name        TEXT    NOT NULL,
    source_url  TEXT    NOT NULL,
    output_type TEXT    NOT NULL DEFAULT 'mihomo',
    token       TEXT    NOT NULL,
    last_fetch_at     TEXT,
    last_fetch_status TEXT,
    node_order  TEXT,
    node_section_order TEXT,
    group_order TEXT,
    created_at  TEXT    NOT NULL,
    updated_at  TEXT    NOT NULL
);

CREATE UNIQUE INDEX idx_profiles_token ON profiles (token);
CREATE UNIQUE INDEX idx_profiles_name  ON profiles (name);
```

- `token`：≥32 随机字节、URL-safe；自托管单用户场景明文存储以便展示完整链接（有意不哈希）。
  `source_url` 含机场凭据，视敏感：不完整入日志，API 默认脱敏。
- `last_fetch_*`：最近机场拉取观测（`success`/`http_error:502`/`ssrf_rejected`/`timeout`/`too_large`）。
- 输出 `proxies` = **机场块**（机场代理，上游序，不可排）+ **自定义块**（全局 `global_nodes`，各
  profile 一致）拼接。
- `node_order`：**已弃用**（自 `0007` 恒 NULL；列保留仅避 `DROP COLUMN`）。自定义块顺序改由全局
  `global_nodes.position` 决定。迁移 `0002`。
- `node_section_order`：两块先后，JSON 两元数组（`["provider","custom"]` 排列，NULL=机场块在前）；
  **仍 per-profile**，由 `PUT .../node-section-order` 写。迁移 `0004`。
- `public_refresh_at`：最近一次公开刷新完成时间，成功/失败均记录，供节流与合并使用。迁移 `0013`。
- `source_revision`：`source_url` 的版本号（创建时 0，每次换源 +1）；拉取观测、公开刷新时间与缓存
  回写均绑定它，换源前的在途拉取结果据此丢弃。迁移 `0015`。
- `group_order`：`proxy-groups` 顺序（分组名数组，NULL=创建序）；生成时快照回写、新增落末尾；
  `PUT .../group-order` 覆盖。迁移 `0003`。
- `reality_mlkem`：是否为未显式设置 `support-x25519mlkem768` 的 REALITY 节点补开（默认 1）。新版
  Xray 服务端要求该密钥协商，旧版 REALITY 服务端却不兼容（mihomo 因此默认关闭）；机场节点无法逐个
  编辑，故按订阅（= 按机场）取舍。迁移 `0016`。

#### rulesets

每 profile 一份规则文本（`UNIQUE(profile_id)`）；保留 `priority`/`name` 备扩展；`enabled` 同样是保留列（恒为 1，不参与生成与查询），仅备扩展、避免无谓迁移。

```sql
CREATE TABLE rulesets (
    id          TEXT    PRIMARY KEY,
    profile_id  TEXT    NOT NULL REFERENCES profiles (id) ON DELETE CASCADE,
    name        TEXT    NOT NULL DEFAULT 'default',
    content     TEXT    NOT NULL,
    priority    INTEGER NOT NULL DEFAULT 0,
    enabled     INTEGER NOT NULL DEFAULT 1,
    updated_at  TEXT    NOT NULL
);

CREATE UNIQUE INDEX idx_rulesets_profile ON rulesets (profile_id);
```

#### global_nodes

**全局自定义节点池（跨订阅共享）。** 自 `0007_global_nodes.sql` 起自定义节点不再隶属单个
profile，而是一份全局集合自动追加到**每条** profile 输出的自定义块；编辑/排序统一在「节点配置」
页（`/api/global-nodes`），详情页只读。

```sql
CREATE TABLE global_nodes (
    id          TEXT    PRIMARY KEY,
    name        TEXT    NOT NULL UNIQUE,
    node_type   TEXT    NOT NULL,
    content     TEXT    NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 1,
    position    INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT    NOT NULL,
    updated_at  TEXT    NOT NULL
);

CREATE INDEX idx_global_nodes_position ON global_nodes (position);
```

- `name` 全局唯一；`node_type`（`ss`/`vmess`/…）不加 CHECK 免迁移；`content` 为完整 Mihomo proxy
  映射，生成时并入每条 profile 输出（输出的 `name` 以本表 `name` 为准）。
- `position`：全局自定义块顺序（`ORDER BY position, name`，name 作确定性兜底）；新建取 `MAX+1`，
  `PUT /api/global-nodes/order` 重写为 `0..n-1` 并即时重排所有 profile 缓存。新建与排序都在
  `BEGIN IMMEDIATE` 事务中读取 `(id, name)` 快照、按不可变 id 回写 `position`，并发改名/增删不会串位。
- 迁移：原各 profile `custom_nodes` 按 `name` 去重（取 `updated_at` 最新）合并进本表（初始
  `position` 全 0，初始序按 name），随后 `DROP TABLE custom_nodes`。

#### rule_sets

**全局用户规则库 / 导入源（② 用户规则库），`0008_rule_sets.sql`。** 管理员在「规则托管」页维护命名
规则集模板（手动 payload 或远程来源）。**自「订阅自包含规则库」起 ② 仅作导入源：不再公开托管、不再
参与生成**（对比早期版本曾托管在 `/<prefix>/r/<name>/...` 并按引用注入）。订阅通过「导入托管规则」
把所选 ② 条目复制进自己的 `profile_rule_sets`（③）；生成只读 ③。表结构不变；`url`（remote 上游）、
`cached_*` 列保留但 ② 不再镜像（导入到 ③ 后由 ③ 镜像）。

```sql
CREATE TABLE rule_sets (
    id                TEXT    PRIMARY KEY,
    name              TEXT    NOT NULL UNIQUE,
    behavior          TEXT    NOT NULL CHECK (behavior IN ('domain', 'ipcidr', 'classical')),
    format            TEXT    NOT NULL CHECK (format IN ('yaml', 'text', 'mrs')),
    source            TEXT    NOT NULL DEFAULT 'manual' CHECK (source IN ('manual', 'remote')),
    content           TEXT    NOT NULL DEFAULT '',  -- manual payload（每行一条）
    rule_count        INTEGER NOT NULL DEFAULT 0,   -- 列表展示用，免读 BLOB
    url               TEXT,                          -- remote 上游 URL
    interval_hours    INTEGER NOT NULL DEFAULT 24,
    cache             INTEGER NOT NULL DEFAULT 1,    -- remote 是否本地二次托管
    cached_body       BLOB,                          -- 镜像字节（text/yaml/mrs 二进制）
    cached_at         TEXT,
    last_fetch_status TEXT,
    enabled           INTEGER NOT NULL DEFAULT 1,
    position          INTEGER NOT NULL DEFAULT 0,
    created_at        TEXT    NOT NULL,
    updated_at        TEXT    NOT NULL
);

CREATE INDEX idx_rule_sets_position ON rule_sets (position);
```

- `name` 全局唯一，同时是 URL 路径段与 `RULE-SET` 引用名，故限定 `[A-Za-z0-9._-]`。
- **manual**：`content` 为 payload（每行一条）；托管时 `yaml` 渲染为 `payload:` 列表、`text` 逐行原样；
  format 限 `yaml`/`text`。
- **remote**：`url` 为上游；`cache=1` 时面板按 `interval_hours` 懒拉取（每拉取检查新鲜度，过期才回源，
  SSRF 安全）、把原始字节存入 `cached_body`（BLOB，故二进制 `mrs` 不损坏）并以稳定链接二次托管，失败
  回退旧缓存；`cache=0` 则不托管，转换时直接注入上游 `url`。`last_fetch_status` 同 profile 拉取标签。
  更新规则集会清空缓存列（下次拉取重新回源）。
- `position`：仅「规则托管」页的展示顺序（`ORDER BY position, name`）；新建与排序同全局节点池：
  `BEGIN IMMEDIATE` 事务 + 按不可变 id 回写。

#### profile_rule_sets

**每订阅自包含规则库（③ 托管规则库），`0011_profile_rule_sets.sql`。** 镜像 `rule_sets` 的字段但按
`profile_id` 隔离、去掉无语义的 `position`（rule-providers 是 map）。下发时 `RULE-SET,<name>` 引用按名
注入本订阅自己的定义；托管在**按订阅 token 隔离**的链接
`/<prefix>/api/sub/<token>/r/<name>/<behavior>.<format>`，故不同订阅可复用同名而不冲突。

迁移 `0013` 增加 `last_fetch_at`（成功/失败镜像完成时间）与 `revision`（定义更新版本，默认 0），
用来合并失败重试并阻止换源前的在途请求回写。

```sql
CREATE TABLE profile_rule_sets (
    id                TEXT    PRIMARY KEY,
    profile_id        TEXT    NOT NULL REFERENCES profiles (id) ON DELETE CASCADE,
    name              TEXT    NOT NULL,
    behavior          TEXT    NOT NULL CHECK (behavior IN ('domain', 'ipcidr', 'classical')),
    format            TEXT    NOT NULL CHECK (format IN ('yaml', 'text', 'mrs')),
    source            TEXT    NOT NULL DEFAULT 'manual' CHECK (source IN ('manual', 'remote')),
    content           TEXT    NOT NULL DEFAULT '',
    rule_count        INTEGER NOT NULL DEFAULT 0,
    url               TEXT,
    interval_hours    INTEGER NOT NULL DEFAULT 24,
    cache             INTEGER NOT NULL DEFAULT 1,
    cached_body       BLOB,
    cached_at         TEXT,
    last_fetch_status TEXT,
    enabled           INTEGER NOT NULL DEFAULT 1,
    created_at        TEXT    NOT NULL,
    updated_at        TEXT    NOT NULL,
    UNIQUE (profile_id, name)
);

CREATE INDEX idx_profile_rule_sets_profile ON profile_rule_sets (profile_id);
```

- `name` 在**单个订阅内**唯一（`UNIQUE(profile_id, name)`），是 URL 路径段与 `RULE-SET` 引用名，限定
  `[A-Za-z0-9._-]`。
- **manual / remote** 行为与 `rule_sets` 完全一致（校验/渲染/镜像逻辑由 `src/rulelib.rs` 共用）；唯一
  区别是托管链接含订阅 token、且这是生成时**唯一**的规则集来源。
- 「导入托管规则」从 ② 复制条目进本表（含真实远程 URL，由后端复制，前端只见脱敏 URL）。

#### custom_groups

```sql
CREATE TABLE custom_groups (
    id          TEXT    PRIMARY KEY,
    profile_id  TEXT    NOT NULL REFERENCES profiles (id) ON DELETE CASCADE,
    name        TEXT    NOT NULL,
    group_type  TEXT    NOT NULL CHECK (group_type IN
                    ('select','url-test','fallback','load-balance','relay')),
    members     TEXT    NOT NULL,
    options     TEXT,
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT    NOT NULL,
    updated_at  TEXT    NOT NULL,
    UNIQUE (profile_id, name)
);

CREATE INDEX idx_custom_groups_profile ON custom_groups (profile_id);
```

- 是输出 `proxy-groups` 的**唯一来源**（转换器整体替换机场分组，机场原生分组不透传；经
  `import-provider-groups` 落为自定义分组才可编辑入输出）。
- `group_type` 的 CHECK 仍列有 `relay` 只是历史列约束：当前 Mihomo 已移除该类型，API 与生成校验只
  接受 `select`/`url-test`/`fallback`/`load-balance`；迁移 `0018` 把升级前遗留的 relay 分组停用
  （`enabled=0`，不参与生成）并删除相关订阅的生成缓存，使下次生成按当前支持的类型重建。
- `members`：有序 JSON 数组，可引用机场节点（透传）/自定义节点/分组；引用有效性在生成时校验，
  不靠 DB 约束。`options`：类型特有选项 JSON（如 `{"url":"...","interval":300}`）；生成时忽略其中的
  `name`/`type`/`proxies`，这三项只由分组自身字段决定。

> **已移除自定义规则集（rule-providers）托管：** `0005` 曾建 `rule_providers` 表，
> `0006_drop_rule_providers.sql` 用 `DROP TABLE IF EXISTS` 删除（对旧装机幂等）。转换器只透传
> 机场自带 `rule-providers:`；规则里仍可 `RULE-SET,<name>,<policy>` 引用机场条目名。

#### generated_cache

```sql
CREATE TABLE generated_cache (
    profile_id            TEXT PRIMARY KEY REFERENCES profiles (id) ON DELETE CASCADE,
    content_hash          TEXT NOT NULL,
    output_yaml           TEXT NOT NULL,
    subscription_userinfo TEXT,
    provider_yaml         TEXT,
    rules_snapshot        TEXT,
    reality_fingerprint_risks TEXT,
    generated_at          TEXT NOT NULL,
    source_revision       INTEGER,
    capability_hash       TEXT
);
```

- 每 profile 仅留最新一份；公共端点复用/回源/兜底与管理端 `preview` TTL 见「缓存与刷新」节。
- `subscription_userinfo`：机场响应头原文，随缓存保存并在公共端点透传（无则 NULL）。
  `content_hash`：对生成输出（`output_yaml`）的哈希，随缓存保存；目前仅写入、无读取方。
- `provider_yaml`：本次输出所用的机场原文，供排序/规则编辑与重置 token/前缀后离线重生成（见「节点/分组
  预览与排序」）。迁移 `0012`；之前生成的缓存为 NULL，下次回源后补齐。
- `rules_snapshot`：产出本缓存时所用的规则文本快照；重置 token/前缀时若当前规则文本已无法通过校验，
  用它离线重生成，使缓存中的托管链接换绑后仍对应一份可加载的配置。迁移 `0014`。
- `reality_fingerprint_risks`：本缓存输出里 ML-KEM 已开启但指纹不是 chrome 的 REALITY 节点名（JSON
  数组，无则 NULL）；回源与离线重生成都会刷新，供详情页常驻告警。迁移 `0016`；之前的缓存为 NULL。
- `source_revision` / `capability_hash`：构建本缓存时的来源版本与公开能力摘要
  （`capability_hash = SHA-256(token + 公共路径前缀)`，带域分隔）。缓存写入以 SQL 条件核对
  来源版本、token、前缀仍与库中一致，任一变化则整份结果丢弃、旧缓存保持不变。**NULL 表示
  `0017` 前的旧缓存或已被编辑标脏，不做回填**；上下文核对与复用/迁移策略见「缓存与刷新」。
  迁移 `0017`。

### 迁移

- `sqlx::migrate!`，文件 `migrations/NNNN_description.sql`，启动自动执行；已应用文件不可改，只追加。
- `0001` 直接建全表（无生产数据，丢弃原型 `subscriptions`）。SQLite `ALTER` 受限，改列用
  「建新表 → 拷贝 → 改名」。

---

## 安全设计

自托管于 1Panel，默认安全：不泄露机场秘密、不被当内网扫描器、链接不易枚举、管理面全程
鉴权、错误/日志不含秘密。

**信任边界（区别对待）：** ① 管理员浏览器 → Web UI / 管理 API：需认证；② 公共客户端 →
订阅端点：无登录，需随机路径前缀 + per-profile token；③ 后端 → 机场 URL：每次出站获取受
SSRF 保护。

### 公共链接

```text
https://<PUBLIC_BASE_URL>/<PUBLIC_PATH_PREFIX>/api/sub/<profile_token>
```

- `PUBLIC_PATH_PREFIX` 随机 16-24 字符；`profile_token` ≥32 随机字节、每配置独立；链接不含
  库 ID 或机场 URL。前缀（环境变量种子与库中已存值均如此）启动时 trim 并校验为单个 URL-safe
  路径段（`[A-Za-z0-9._~-]`、≤128 字节、非 `.`/`..`），非法值拒绝启动；库中旧值不规范时启动
  即规范化回写。
- 放行 = 前缀匹配 **且** token 存在；否则一律 `404`（不透露哪步失败）。订阅没有停用状态
  （`0010` 起），吊销一条公开链接只能重置其 token、重置公共前缀或删除该订阅。
- **防时序侧信道**：无论前缀是否匹配都执行 token 查找，前缀恒定时间比较；规则集托管端点同样先
  查 token 再判定前缀。
- **规则集托管** `…/<PUBLIC_PATH_PREFIX>/api/sub/<profile_token>/r/<name>/<behavior>.<format>`
  共用同一前缀与 profile token（重置任一秘密均使其失效）。规则集内容是规则清单、非私密，按名可枚举
  可接受；仍按源 IP 限流。`name` 限 `[A-Za-z0-9._-]`，杜绝路径穿越。

### Token 轮换

重置单配置 token、重置全局 `PUBLIC_PATH_PREFIX`（使所有链接失效）均支持；重置后缓存随即离线重生成，
使其中按 token 隔离的规则集托管链接改用新值。重生成先用当前规则文本；当前文本校验失败时回退到产出
该缓存时的规则快照（`rules_snapshot`）再试一次；两者都无法产出合法输出时删除缓存，公开链接在下次
成功回源前按无缓存处理。`reset-token` 响应含 `regenerate`（同「节点/分组预览与排序」），前端据此
提示缓存是否已换绑。机场变化时链接保持稳定，除非显式重置。
缓存构建时的上下文（`source_revision`/`capability_hash`）绑定与公开端点侧的离线懒迁移见
「缓存与刷新」。
公开订阅和规则集在入口、拿到刷新锁后、交付响应前重新校验前缀/token，轮换期间排队或回源中的
旧能力请求返回统一 `404`，不会从新生成的规则集 URL 获得新前缀。前缀比较在 token 查询前后均为
恒定时间比较，并始终执行查询。管理生成与创建自动生成在锁内重读 token/来源。

### 管理员认证

- 凭据来自 `ADMIN_USERNAME` / `ADMIN_PASSWORD`（compose 环境变量），未设置拒绝启动；
  恒定时间比较；登录按来源 IP 限流（成功/失败请求均计数）。
- 会话 Cookie：≥128 位 CSPRNG ID、`HttpOnly` + `SameSite=Lax`、HTTPS 加 `Secure`；存内存
  （重启失效）、空闲超时默认 7 天。`Secure` 由 `https://` 的 `PUBLIC_BASE_URL` 推断；TLS 终止
  代理后（应用走 HTTP）需显式 `SECURE_COOKIES=true`，否则告警。
- **不启用 CORS 层**（SPA 同源；宽松 CORS 会破坏 cookie 同源保护）；状态变更请求必须带同源
  `Origin`，生产环境按 `PUBLIC_BASE_URL` 的完整 origin（scheme + host + port）校验（缺失或不匹配
  均 `403`）。公共链接不需会话。
- 所有响应统一带 `Content-Security-Policy: frame-ancestors 'none'` 和 `X-Frame-Options: DENY`，
  禁止管理页面被 iframe 嵌入，包括同站兄弟子域；覆盖 SPA 深链、静态资源与条件请求的 `304`。
  CSP 仅限制嵌入，不限制前端脚本/动态样式，不改变公共订阅的响应体或下载方式。
- 前端仅在退出成功或确认会话无效（`401`）后进入已退出状态。退出网络错误或其他失败时保留
  登录态，明确提示会话可能仍有效、允许重试；退出在途期间阻止重复点击。
- 节点协议标签只查自有属性，机场提供的未知类型按文本展示（包括 `__proto__` 等特殊键）。
  管理内容区设渲染错误边界，故障时保留导航和退出入口，切换页面后恢复。
- 详情组件以订阅 ID 重挂载，切换时立即卸载旧表单、预览和本地状态；卸载后的请求结果不再更新
  详情或触发旧生成的提示/重载。相同订阅的并发重载仅采纳最后发起的结果。

### SSRF 保护

**所有**出站获取（generate / preview / 公共端点 + provider-rules / import-provider-groups +
规则集远程镜像）走单一保护获取器。规则集远程镜像复用同一获取器的字节路径（`fetch_bytes`，为二进制
`mrs` 不强制 UTF-8），享受同样的 IP 钉定 / 重定向逐跳重查 / 超时 / 大小限制。

- 仅 `http` / `https`；拒空主机、内嵌凭据、`localhost` 回环名、阻止段裸 IP。
- 解析域名 → 检查解析 IP → **连接时固定该 IP**（防 DNS 重绑定 TOCTOU），非请求时重解析；
  每个重定向同规则重查，上限 3。
- IPv6 内嵌 IPv4（映射 `::ffff:0:0/96`、NAT64 `64:ff9b::/96`、6to4 `2002::/16`）须解包出
  IPv4 再按 IPv4 段查（经典绕过，如 `http://[::ffff:127.0.0.1]/`）。
- 出站限制：连接超时 5-10s、总超时 10-20s（一次拉取共用一个截止时间，覆盖 DNS 解析、全部重定向跳与
  响应体读取）、最大响应 5-10MB（按流字节计，不信 `Content-Length`）、重定向 ≤3、仅取文本/YAML。
  系统 DNS 解析走全局信号量（并发上限 32），排队与解析都占用同一份截止时间，超时后尾部有界——
  在途的系统解析最多再占满信号量，不会无限堆积。
- 不使用系统/环境变量代理（`HTTP_PROXY` 等）：代理会自行解析域名，绕过 IP 固定。

阻止 IPv4：`0.0.0.0/8 10.0.0.0/8 100.64.0.0/10 127.0.0.0/8 169.254.0.0/16 172.16.0.0/12
192.0.0.0/24 192.0.2.0/24 192.88.99.0/24 192.168.0.0/16 198.18.0.0/15 198.51.100.0/24
203.0.113.0/24 224.0.0.0/4 240.0.0.0/4`
阻止 IPv6：`::/128 ::1/128 64:ff9b:1::/48 fc00::/7 fe80::/10 ff00::/8`；内嵌 IPv4 的
`::ffff:0:0/96`（映射）、`64:ff9b::/96`（NAT64 周知前缀）、`2002::/16`（6to4）不按文本阻止，
而是一律解包出内嵌 IPv4 再按 IPv4 段查（见上）。

### 不受信任内容（机场响应即使过 SSRF 也不可信）

- YAML 两道限界。先用 libyaml 事件流预检原文：只接受单文档，事件总数 ≤20,000（集合的结束事件
  也计入），集合嵌套深度 ≤32，原始标量/tag/锚点字节合计 ≤8 MiB——不构建任何值即拒绝畸形与
  高扇出输入。再由 serde_norway 物化，构建每个值时扣预算：深度 32、节点总数 10,000（包含映射键、
  值、tag 名和内部值），展开后的标量总字节最多 8 MiB，且不超过原文的 8 倍（最小预算 1 KiB）。
  小规模锚点/别名与 tag 仍支持；预算在别名物化过程中扣除，拒绝标量复制与嵌套别名放大。外部
  原文最多 8 MiB；自生成输出与内部重解析（如分组顺序快照回写）放宽到 16 MiB。merge key（`<<`）
  在物化后随即按同一份节点步数预算归一化展开（显式键优先，多源序列取先出现者），转换器只处理
  展开后的值。转换后合并的
  Value 同样检查深度、节点、8 MiB 标量预算，序列化写入最多 16 MiB 输出；超限不更新合法缓存。
  管理员提交的节点 YAML 同等限制；请求体 ≤1MB（超限 `413`）。
- `subscription-userinfo` 只接受标准十进制字段：键限 `upload`/`download`/`total`/`expire`，值须为
  非空纯 ASCII 数字，整体 ≤512 字节，拒绝任何控制字符（防头注入）；不合规的头不透传、不入库。
- 机场节点/分组名视为纯数据，渲染时转义，绝不拼入 HTML / shell。

### 敏感数据（机场 URL 含秘密）

不写完整 URL 进日志 / 公共输出 / 错误；管理 API 默认脱敏；Web UI 仅持脱敏值。脱敏规则
（确定性，处处一致）：留 scheme/host/path，每个查询值 → `***`（`?token=abcdef` → `?token=***`）。
HTTP trace 只记录脱敏 path，公开订阅/规则集路径中的 `PUBLIC_PATH_PREFIX` 与 profile token 均替换为
占位值。

### 限流与客户端 IP

- 登录按**源 IP**（成功/失败请求均计数）；公共下载按**源 IP**（独立于 token，`404` 也计数）
  限流，使枚举共享单一预算；首版内存限流。每个限流器最多 10,000 个桶，满时新 IP 返回 `429`，
  现有 IP 保留独立预算；按 `min(窗口, 30 秒)` 的间隔调度清理已可完全补满的空闲桶，
  避免每次请求扫描全表。
- 默认不信任 `X-Forwarded-For`： `TRUSTED_PROXY_HOPS=0`，按 TCP 对端限流。需要按真实客户端 IP
  限流时，必须同时设置受信跳数与 `TRUSTED_PROXY_CIDRS`（逗号分隔的直接反代网段）；只有 TCP 对端
  落在该网段内时才读取 `X-Forwarded-For`。读取时把该头的所有字段值按逗号合并为一条链，从右往左
  验证：取倒数第 `TRUSTED_PROXY_HOPS` 个条目为客户端，其右侧的条目须全部是可解析且落在可信网段
  内的代理地址（最左条目客户端可伪造，从不采用）。头缺失、条目不足、代理链不符或 peer 不可信时
  回退 TCP 对端。

### 缓存与刷新

- 公共端点以 `PUBLIC_REFRESH_MIN_SECONDS`（默认 30 秒）作为每配置最小回源间隔：间隔内复用最近
  `generated_cache`（降低 token 泄露后高频请求对机场的回源放大），间隔外回源拉取并重新生成；
  成功/失败均保存公开刷新完成时间 `profiles.public_refresh_at`，失败或转换校验失败同样遵守该间隔，
  用旧缓存兜底（无则 `503`）。`CACHE_TTL_MINUTES`（默认 15，按 `generated_at`）
  仅管理端 `preview`。
- **缓存上下文绑定**：每份缓存记录构建时的 `source_revision` 与 `capability_hash`（token + 公共
  前缀摘要，见「数据模型」）。公开端点只复用两者都与当前库值匹配的缓存——包括节流内的复用与
  失败兜底。仅能力不符（token/前缀轮换后来源未变）时，在 per-profile 锁内离线懒迁移：用缓存的
  机场原文重跑完整转换换绑托管链接（当前规则文本校验失败回退 `rules_snapshot` 再试）；来源不符
  或上下文为 NULL（`0017` 前旧缓存、已被标脏）时先使缓存失效，再在线回源重建。无法迁移的缓存
  删除，公开链接按无缓存处理直到下次回源成功。管理端 `preview` 只读、与所有写缓存路径共用同一
  per-profile 锁，且只直接返回兼容且新鲜的缓存。
- **single-flight**：同配置并发刷新在 per-profile 锁后合并为一次上游获取（后到者等待或拿陈旧
  缓存），防踩踏扇出；同批等待者共享已完成的成功/失败结果，即使最小间隔设为 0。锁表随用随收：
  无等待者的条目在结束后即移除，等待/执行中的 future 被取消时同样释放锁并清理条目，不随请求量
  只增不减。
- 远程规则集镜像：成功缓存按 `interval_hours` 复用；失败通过 `last_fetch_at` 短缓存 30 秒，
  继续提供旧缓存（无则 `503`），同批等待者共享失败。定义每次更新递增 `revision` 并清空缓存和
  尝试时间；锁内重读 URL、名称、格式、启用和托管条件，回写缓存/失败状态以 revision 作条件，
  定义已变或删除则丢弃旧在途结果并返回 `404`。

### 错误处理

- 公共端点：无效路径 / token → 通用 `404`；有效但无缓存且拉取失败 → 通用 `503`
  （体内无上游细节）；不透露 token 是否存在。
- 管理 API：返回有用校验错误但不含机场秘密；内部细节脱敏后入日志。
