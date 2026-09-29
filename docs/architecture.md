# 架构参考

> API 契约、数据模型、安全设计——后端实现的权威参考。已实现，取舍见 `changelog.md`。

---

## API 设计

### 原则

- 管理 API 挂 `/api`、需登录会话、用 JSON；公开订阅端点无需登录（需随机路径前缀 +
  per-profile token）、输出 Mihomo YAML。
- 公开端点任何校验失败统一 `404`（不泄因）；管理 API 默认对机场 URL 脱敏。
- 管理 API **同源**（SPA 由 Axum 同源提供，不启用 CORS；见安全设计节）。
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
| POST | `/api/profiles/:id/reset-token` | 是 | 重置该配置 token |
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
  "rules": { "content": "...\nMATCH,Proxy", "updated_at": "..." },
  "nodes": [ { "id": "...", "name": "my-ss", "node_type": "ss", "enabled": true } ],
  "groups": [ { "id": "...", "name": "MyGroup", "group_type": "select",
               "members": ["my-ss","DIRECT"], "enabled": true } ],
  "created_at": "...", "updated_at": "..."
}
```

- 创建：`{name, source_url}`。创建成功后**同步触发一次生成/拉取**（尽力而为），故新订阅
  立即带有真实 `last_fetch_status`，不存在「未拉取」中间态。
- `source_url` 写时静态校验（http/https、无内嵌凭据、非本地/私有地址），否则 `400`；真正 SSRF
  在拉取时按 DNS 解析 + IP 固定。
- `last_fetch_status`：`success` / `http_error:<code>` / `ssrf_rejected` / `timeout` / `too_large`，
  从未拉取为 `null`。

请求体（节点走全局池，分组按配置）：

```text
POST /api/global-nodes { name, node_type, content, enabled? }  # name 全局唯一（重名 409）；content 为完整 Mihomo proxy 映射 YAML
POST /api/profiles/:id/groups { name, group_type, members, options?, enabled? }
```

- 节点 `content` 保存时结构校验，生成时原样并入**每条配置**输出的 `proxies`；`PUT` 同体整体替换。
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
   # source=remote: { url, interval_hours?=24, cache?=true }   format∈yaml/text/mrs；url 须 http(s)
PUT    /api/rule-sets/:id   { ...同上 }                 # remote 编辑 url 留空则沿用原值（已脱敏不回显）
DELETE /api/rule-sets/:id
PUT    /api/rule-sets/order { order: [规则集名] }        # 仅展示序，未列出落末尾
```

② 不再公开托管、不再参与生成，仅是可复用模板。通过 ③ 的导入端点复制进某订阅后才生效。

#### ③ 每订阅托管规则库（下发来源）

```text
GET    /api/profiles/:id/rule-sets                     # 列表；每项含 url（按订阅 token 隔离的托管链接）、count、source、remote_url_masked、cache、last_fetch_status
POST   /api/profiles/:id/rule-sets   { name, behavior, source?, format, ... }   # name 在本订阅内唯一（重名 409）；字段同 ②
PUT    /api/profiles/:id/rule-sets/:rsid   { ...同上 }
DELETE /api/profiles/:id/rule-sets/:rsid
POST   /api/profiles/:id/rule-sets/import  { names: [②规则集名], policy }   # 复制 ② 定义进 ③（含真实远程 URL）+ 为未引用名追加 RULE-SET,<name>,<policy> 行；返回 { imported }（实际复制进 ③ 的定义数，已存在同名的不计）
```

- manual / remote 行为与 ② 一致（校验/渲染/镜像同一套逻辑）。托管在按订阅 token 隔离的链接
  `/<prefix>/api/sub/<token>/r/<name>/<behavior>.<format>`；remote 关缓存则转换时直接注入上游 URL。
- 被本订阅 `RULE-SET,<name>` 规则引用时注入指向该托管链接的 `rule-providers:` 条目（公共链接每拉取即
  重生，故定义改动即时生效）；未被引用不注入。
- 注入时若名与机场 `rule-providers` 已有条目**撞名**，用本订阅托管版**覆盖**机场版；生成端点
  （`POST .../generate`）在响应 `ruleset_conflicts` 列出撞名的名字，详情页据此告警，避免静默替换。

### 节点/分组预览与排序

```text
GET /api/profiles/:id/proxies
{ "generated": true, "generated_at": "...",
  "proxies": [{ "name":"hk-1","type":"ss" }, { "name":"my-ss","type":"ss" }],
  "node_section_order": ["provider","custom"],
  "groups": [{ "name":"Proxy","type":"select" }] }
```

只读，解析自 `generated_cache.output_yaml`，直接返回缓存当前内容（排序改动会离线重生成缓存）；
未生成返回 `generated:false` + 空数组。`proxies` = 机场块 + 自定义块按 `node_section_order` 拼接，
前端据全局节点名集合拆成两块渲染（机场只读，自定义在「节点配置」排序）；`groups` 全为自定义
分组。两者也作分组成员候选。

```text
PUT /api/global-nodes/order               { order: [节点名] }              # 全局，未列出落末尾；重写为 0..n-1
PUT /api/profiles/:id/node-section-order  { order: ["custom","provider"] } # per-profile，必须是排列
PUT /api/profiles/:id/group-order         { order: [分组名] }              # per-profile
-> 均 204
```

- 自定义块顺序由全局 `global-nodes/order` 决定（作用所有配置）；两块先后由 per-profile
  `node-section-order`；分组顺序由 per-profile `group-order`。名字超长/数组过大 `400`。
- 这些端点保存后用缓存中的机场原文（`generated_cache.provider_yaml`）**离线重跑完整转换、无需重拉
  机场**，改动**立即生效**（预览与公共链接随即反映）；全局排序重生成**每条配置**缓存，无缓存者
  首次生成时生效。
- 规则拖拽同理：规则顺序即语义（命中即止），存为 `rulesets.content` 有序文本，前端经
  `PUT .../rules` 整体保存，同样离线重生成、立即生效。
- 离线重生成与 `generate` 走同一转换器（含校验与 `rule-providers` 注入），与公开刷新共用 per-profile
  锁；保留 `generated_at`（回源节奏不变）。校验失败时编辑照常入库，但缓存保留上一份合法输出（逐条错误
  由下次「生成」报出）；无机场原文的旧缓存（`0012` 之前生成）为 no-op，下次回源后生效。
- 每次生成把输出的分组顺序快照回写 `group_order`（新增分组落末尾）；节点顺序为全局
  `global_nodes.position`，不 per-profile 快照，机场块恒上游序。
- 节点/分组均结构化表单录入（节点常用字段 + 高级 KV；分组按类型给选项 + 高级 KV；成员从候选
  下拉选），前端分别序列化为节点 `content` YAML 与分组 `options` JSON。

### 生成与校验

- `POST .../generate` 完整校验，成功刷新缓存并返回托管链接，失败 `400` + 逐条错误（对应弹窗
  文案）。详情页「原始订阅源」手动刷新复用本端点。
- `GET .../preview` 是只读版：有新鲜缓存则返回，否则实时拉取生成；不写缓存、不动 `last_*`。
- 校验：规则引用的分组须存在于已启用自定义分组；自定义分组名可与机场分组重名（机场分组整体
  替换）；分组成员须引用存在的机场节点（透传）/启用自定义节点/启用自定义分组；输出须合法
  Mihomo YAML。成功响应 `{ subscription_url, generated_at }`。

顶层键处理（转换器逐键显式处理）：

| 键 | 处理 |
|-----|------|
| `proxies` | 机场块（上游序）+ 自定义块（启用全局节点按 `global_nodes.position` 排），按 `node_section_order` 拼接 |
| `proxy-groups` | 整体替换为启用的自定义分组（机场分组不透传，需「导入机场分组」），按 `group_order` 重排 |
| `rules` | 整体替换为用户规则 |
| `rule-providers` | 机场原样透传；另把被 `RULE-SET` 引用、本订阅自有（③）规则集合并在上（同名覆盖，指向按订阅 token 隔离的托管链接） |
| `proxy-providers` | **剥离**（会让客户端拉取绕过 SSRF/缓存的 URL，可能暴露机场 URL） |
| 其余（`port`/`dns`/`tun`/`sniffer`…） | 原样透传（新 Mihomo 选项无需改转换器） |

- `import-provider-groups`：实时拉取机场，把 `proxy-groups` 解析为可编辑自定义分组写入
  （`name`/`type`/`proxies`→成员，其余→`options`；跳过同名/不支持类型），返回 `{imported,skipped}`；
  只改 `custom_groups`，需重新生成才进输出；鉴权 + SSRF 同 `provider-rules`。
- 规则集：除透传机场自带 `rule-providers` 外，面板按 `RULE-SET,<name>,<policy>` 注入**本订阅自有规则库
  （③）**中同名条目，指向按订阅 token 隔离的托管链接；`<name>` 也可仍引用机场条目名。
  全局 ② 库不参与生成。

### 公开订阅端点

```text
GET /:public_path_prefix/api/sub/:token
  -> 200 text/yaml   有效前缀 + token + 配置启用
  -> 503             有效但无缓存且上游拉取失败（通用）
  -> 404             其余一切（统一）
```

响应头：`content-type: text/yaml`、`content-disposition: attachment; filename="<name>.yaml"`、
`subscription-userinfo`（从机场透传，无则省略）、`profile-update-interval: 24`（小时，MVP 固定）。

- 缓存复用/回源/兜底与 single-flight 合并的策略见「缓存与刷新」节。无「生成配置」按钮；
  响应/错误绝不含机场 URL；按来源 IP 限流。

兼容：早期 `/api/v1/subscriptions*`、`/api/v1/merged` 已移除，无兼容层。

### 公开规则集托管端点

```text
GET /:public_path_prefix/api/sub/:token/r/:name/:file
  -> 200 text/plain | application/octet-stream(mrs)   前缀+token 匹配 + 名在该订阅存在且启用 + :file=="<behavior>.<format>" + 已托管
  -> 503             remote 镜像首拉失败且无缓存
  -> 404             其余一切（统一，含前缀错/token 错/名不存在/未启用/文件名不符/remote 关缓存未托管）
```

- 托管本订阅自有规则库（③），**按订阅 token 隔离**，与订阅端点共用 `public_path_prefix`（重置公共路径同样
  使其失效）。规则集是规则清单、非私密，按名可枚举可接受；仍按来源 IP 限流。全局 ② 库不再公开托管。
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
- `group_order`：`proxy-groups` 顺序（分组名数组，NULL=创建序）；生成时快照回写、新增落末尾；
  `PUT .../group-order` 覆盖。迁移 `0003`。

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
  映射，生成时并入每条 profile 输出。
- `position`：全局自定义块顺序（`ORDER BY position, name`，name 作确定性兜底）；新建取 `MAX+1`，
  `PUT /api/global-nodes/order` 重写为 `0..n-1` 并即时重排所有 profile 缓存。
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
- `position`：仅「规则托管」页的展示顺序（`ORDER BY position, name`）。

#### profile_rule_sets

**每订阅自包含规则库（③ 托管规则库），`0011_profile_rule_sets.sql`。** 镜像 `rule_sets` 的字段但按
`profile_id` 隔离、去掉无语义的 `position`（rule-providers 是 map）。下发时 `RULE-SET,<name>` 引用按名
注入本订阅自己的定义；托管在**按订阅 token 隔离**的链接
`/<prefix>/api/sub/<token>/r/<name>/<behavior>.<format>`，故不同订阅可复用同名而不冲突。

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
- `members`：有序 JSON 数组，可引用机场节点（透传）/自定义节点/分组；引用有效性在生成时校验，
  不靠 DB 约束。`options`：类型特有选项 JSON（如 `{"url":"...","interval":300}`）。

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
    generated_at          TEXT NOT NULL
);
```

- 每 profile 仅留最新一份；公共端点复用/回源/兜底与管理端 `preview` TTL 见「缓存与刷新」节。
- `subscription_userinfo`：机场响应头原文，随缓存保存并在公共端点透传（无则 NULL）。
  `content_hash`：对生成输出（`output_yaml`）的哈希，随缓存保存；目前仅写入、无读取方。
- `provider_yaml`：本次输出所用的机场原文，供排序/规则编辑与重置 token/前缀后离线重生成（见「节点/分组
  预览与排序」）。迁移 `0012`；之前生成的缓存为 NULL，下次回源后补齐。

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
  库 ID 或机场 URL。
- 放行 = 前缀匹配 **且** token 存在 **且** 配置启用；否则一律 `404`（不透露哪步失败）。
- **防时序侧信道**：无论前缀是否匹配都执行 token 查找，前缀恒定时间比较；规则集托管端点同样先
  查 token 再判定前缀。
- **规则集托管** `…/<PUBLIC_PATH_PREFIX>/api/sub/<profile_token>/r/<name>/<behavior>.<format>`
  共用同一前缀与 profile token（重置任一秘密均使其失效）。规则集内容是规则清单、非私密，按名可枚举
  可接受；仍按源 IP 限流。`name` 限 `[A-Za-z0-9._-]`，杜绝路径穿越。

### Token 轮换

重置单配置 token、重置全局 `PUBLIC_PATH_PREFIX`（使所有链接失效）均支持；重置后缓存随即离线重生成，
使其中按 token 隔离的规则集托管链接改用新值。机场变化时链接保持稳定，除非显式重置。

### 管理员认证

- 凭据来自 `ADMIN_USERNAME` / `ADMIN_PASSWORD`（compose 环境变量），未设置拒绝启动；
  恒定时间比较；登录按来源 IP 限流（成功/失败请求均计数）。
- 会话 Cookie：≥128 位 CSPRNG ID、`HttpOnly` + `SameSite=Lax`、HTTPS 加 `Secure`；存内存
  （重启失效）、空闲超时默认 7 天。`Secure` 由 `https://` 的 `PUBLIC_BASE_URL` 推断；TLS 终止
  代理后（应用走 HTTP）需显式 `SECURE_COOKIES=true`，否则告警。
- **不启用 CORS 层**（SPA 同源；宽松 CORS 会破坏 cookie 同源保护）；状态变更请求必须带同源
  `Origin`，生产环境按 `PUBLIC_BASE_URL` 的完整 origin（scheme + host + port）校验（缺失或不匹配
  均 `403`）。公共链接不需会话。

### SSRF 保护

**所有**出站获取（generate / preview / 公共端点 + provider-rules / import-provider-groups +
规则集远程镜像）走单一保护获取器。规则集远程镜像复用同一获取器的字节路径（`fetch_bytes`，为二进制
`mrs` 不强制 UTF-8），享受同样的 IP 钉定 / 重定向逐跳重查 / 超时 / 大小限制。

- 仅 `http` / `https`；拒空主机、内嵌凭据、`localhost` 回环名、阻止段裸 IP。
- 解析域名 → 检查解析 IP → **连接时固定该 IP**（防 DNS 重绑定 TOCTOU），非请求时重解析；
  每个重定向同规则重查，上限 3。
- IPv6 内嵌 IPv4（映射 `::ffff:0:0/96`、NAT64 `64:ff9b::/96`、6to4 `2002::/16`）须解包出
  IPv4 再按 IPv4 段查（经典绕过，如 `http://[::ffff:127.0.0.1]/`）。
- 出站限制：连接超时 5-10s、总超时 10-20s、最大响应 5-10MB（按流字节计，不信
  `Content-Length`）、重定向 ≤3、仅取文本/YAML。

阻止 IPv4：`0.0.0.0/8 10.0.0.0/8 100.64.0.0/10 127.0.0.0/8 169.254.0.0/16 172.16.0.0/12
192.0.0.0/24 192.0.2.0/24 192.88.99.0/24 192.168.0.0/16 198.18.0.0/15 198.51.100.0/24
203.0.113.0/24 224.0.0.0/4 240.0.0.0/4`
阻止 IPv6：`::/128 ::1/128 ::ffff:0:0/96 64:ff9b::/96 2002::/16 fc00::/7 fe80::/10 ff00::/8`

### 不受信任内容（机场响应即使过 SSRF 也不可信）

- 解析 YAML 用资源限制：**先**扫原文限锚点/别名数（防「十亿笑」），**再**限嵌套深度/节点数；
  管理员提交的节点/分组 YAML 同等限制；请求体 ≤1MB（超限 `413`）。
- `subscription-userinfo` 存/回显前校验格式（仅 `key=value; ...`，拒 CR/LF，防头注入）。
- 机场节点/分组名视为纯数据，渲染时转义，绝不拼入 HTML / shell。

### 敏感数据（机场 URL 含秘密）

不写完整 URL 进日志 / 公共输出 / 错误；管理 API 默认脱敏；Web UI 仅持脱敏值。脱敏规则
（确定性，处处一致）：留 scheme/host/path，每个查询值 → `***`（`?token=abcdef` → `?token=***`）。
HTTP trace 只记录脱敏 path，公开订阅/规则集路径中的 `PUBLIC_PATH_PREFIX` 与 profile token 均替换为
占位值。

### 限流与客户端 IP

- 登录按**源 IP**（成功/失败请求均计数）；公共下载按**源 IP**（独立于 token，`404` 也计数）
  限流，使枚举共享单一预算；首版内存限流。
- 默认不信任 `X-Forwarded-For`： `TRUSTED_PROXY_HOPS=0`，按 TCP 对端限流。需要按真实客户端 IP
  限流时，必须同时设置受信跳数与 `TRUSTED_PROXY_CIDRS`（逗号分隔的直接反代网段）；只有 TCP 对端
  落在该网段内时才读取 `X-Forwarded-For`，并取**最右**不受信跳（最左可伪造）。头缺失、过短、
  或 peer 不可信时回退 TCP 对端。

### 缓存与刷新

- 公共端点以 `PUBLIC_REFRESH_MIN_SECONDS`（默认 30 秒）作为每配置最小回源间隔：间隔内复用最近
  `generated_cache`（降低 token 泄露后高频请求对机场的回源放大），间隔外回源拉取并重新生成；
  拉取失败时用旧缓存兜底（无则 `503`）。`CACHE_TTL_MINUTES`（默认 15，按 `generated_at`）
  仅管理端 `preview`。
- **single-flight**：同配置并发刷新在 per-profile 锁后合并为一次上游获取（后到者等待或拿陈旧
  缓存），防踩踏扇出。

### 错误处理

- 公共端点：无效路径 / token → 通用 `404`；有效但无缓存且拉取失败 → 通用 `503`
  （体内无上游细节）；不透露 token 是否存在。
- 管理 API：返回有用校验错误但不含机场秘密；内部细节脱敏后入日志。
