# 部署与发布

> 部署方式：用 docker compose 在 1Panel 上部署。镜像取 Docker Hub
> `quinlanhoo/mihomo-subscription:<version>` / `:latest`（多架构 amd64+arm64）。

---

## 部署

### Compose 示例

完整可粘贴示例（含全部可选变量与逐行注释）见 [README](../README.md)；最简形式：

```yaml
services:
  mihomo-subscription:
    image: quinlanhoo/mihomo-subscription:latest
    container_name: mihomo-subscription
    restart: unless-stopped
    ports:
      - "8080:8080"            # 主机端口:容器端口（容器固定 8080）
    environment:
      ADMIN_USERNAME: admin
      ADMIN_PASSWORD: change-me
      PUBLIC_BASE_URL: https://sub.example.com
      SECURE_COOKIES: "true"
      # 其余可选项见下表（日志、获取/缓存调优等）
    volumes:
      - ./data:/data           # 持久化 SQLite；容器以 root 启动后 chown 再降权
    networks:
      - 1panel-network
networks:
  1panel-network:
    external: true             # 接入 1Panel 的共享网络，供 OpenResty 反代
```

### 环境变量（权威表）

compose 的 `environment:` 与代码必须一致。

| 变量 | 默认值 | 必需 | 用途 |
|------|--------|------|------|
| `ADMIN_USERNAME` | —— | 是 | 管理登录账户 |
| `ADMIN_PASSWORD` | —— | 是 | 管理登录密码 |
| `PUBLIC_BASE_URL` | 空 | 建议 | 生成托管链接与校验管理 API `Origin` 的外部可达源（`https://sub.example.com`）。为空则链接缺少 scheme/host，且 Origin 校验仅回退到 Host |
| `PUBLIC_PATH_PREFIX` | 随机 | 否 | 公共路径前缀的种子；运行时值存于 `app_settings` 并可重置。空/空白被忽略并随机生成 |
| `RUST_LOG` | `info` | 否 | 日志级别 |
| `FETCH_TIMEOUT_SECONDS` | `15` | 否 | 机场获取总超时（含 DNS 解析与全部重定向）。出站拉取不读取 `HTTP_PROXY`/`HTTPS_PROXY` 等代理变量 |
| `FETCH_USER_AGENT` | `clash.meta/1.0` | 否 | 机场获取的 `User-Agent`。许多机场按 Clash 家族 UA 限制订阅、对未知客户端返回 `403`/`401`；默认匹配常见 `/clash/i` 检查。仅在面板需要特定客户端 UA 时覆盖（如 Shadowrocket/Stash） |
| `MAX_SUBSCRIPTION_SIZE_MB` | `8` | 否 | 机场响应大小上限 |
| `CACHE_TTL_MINUTES` | `15` | 否 | 仅管理员**预览**缓存 TTL；公共订阅回源节流由 `PUBLIC_REFRESH_MIN_SECONDS` 控制 |
| `PUBLIC_REFRESH_MIN_SECONDS` | `30` | 否 | 公共订阅同一 profile 两次真实回源刷新之间的最小间隔；间隔内复用最近缓存，降低公开 token 泄露后的上游拉取放大 |
| `TRUSTED_PROXY_HOPS` | `0` | 否 | 派生客户端 IP 时信任的反向代理跳数。`0` 表示忽略 `X-Forwarded-For`，按 TCP 对端限流 |
| `TRUSTED_PROXY_CIDRS` | 空 | 否 | 允许提供可信 `X-Forwarded-For` 的直接反向代理 CIDR，逗号分隔。为空时即使 `TRUSTED_PROXY_HOPS>0` 也忽略 `X-Forwarded-For` |
| `SECURE_COOKIES` | `auto` | 否 | 强制 `Secure` 会话 cookie。`auto`（及无法识别值）从 `https://` 的 `PUBLIC_BASE_URL` 推断；经 TLS 终止反代（应用走纯 HTTP）对外 HTTPS 时设 `true`；cookie 最终无 `Secure` 时记 warn |
| `PORT` | `8080` | 否 | 容器监听端口（一般不改，改主机映射即可） |
| `DATA_DIR` | `/data` | 否 | SQLite 数据目录（对应挂载卷） |
| `WEB_DIR` | `web/dist` | —— | Axum 提供的构建 SPA 目录；镜像内由 `ENV` 覆盖为 `/app/web/dist`，烘焙进镜像，非用户字段，仅列出 |

`PUBLIC_BASE_URL` 仅存外部可达源；随机 `PUBLIC_PATH_PREFIX` 前置在 token 化端点前：
`https://sub.example.com/<public-path-prefix>/api/sub/<token>`。

### 反向代理（1Panel 兼容性要点）

- 管理 API 在状态变更请求校验 `Origin` 与 `PUBLIC_BASE_URL` 的完整 origin（CSRF 纵深防御）；
  `PUBLIC_BASE_URL` 必须与浏览器访问的外部地址一致。反代也应**保留原始 Host**
  （nginx/OpenResty：`proxy_set_header Host $host;`）。若浏览器 origin 与配置不一致，每个
  登录/POST/PUT/DELETE 返回 `403`。1Panel 的网站反代默认即保留 Host。
- 默认不信任 `X-Forwarded-For`，以避免容器端口被直连时伪造客户端 IP 绕过登录/下载限流。若确认
  后端端口只能被反代访问，且需要按真实客户端 IP 限流，同时设置 `TRUSTED_PROXY_HOPS=1` 与
  `TRUSTED_PROXY_CIDRS` 为反代容器/网关所在网段。
- 凭据经 `ADMIN_USERNAME`/`ADMIN_PASSWORD` 仅保护管理 UI/API；公共订阅链接仍靠随机路径前缀 +
  per-profile token。
- 容器以 root 启动以便 `docker-entrypoint.sh` chown `${DATA_DIR}`（`./data` 挂载会覆盖构建期 chown，
  否则 SQLite `code 14 (CANTOPEN)`），随后用 `gosu` 降权到 `appuser` 再 exec。
- 主机若运行 Mihomo/Clash 的 TUN + fake-ip 模式，域名会被解析到 `198.18.0.0/15`。该网段在 SSRF
  阻止列表内，所有机场拉取都会失败（`last_fetch_status` 为 `ssrf_rejected`）。让面板容器使用真实
  DNS，或把它排除在 TUN 之外；不要为此放宽 SSRF 规则。

---

## 发布

> 镜像策略：**发布到 Docker Hub**（`quinlanhoo/mihomo-subscription`，多架构 amd64+arm64），
> 1Panel 主机用 docker compose 直接 `docker pull`，无需同步源码；离线/内网用文末本地构建。

### 版本规则

- 语义化版本 `MAJOR.MINOR.PATCH`；`0.x` 允许破坏性变更，但每项须记入 changelog。
- 镜像 tag、`Cargo.toml`、`web/package.json`、`web/package-lock.json` 四处保持一致。

### 发布前检查

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
( cd web && npm ci && npm run build )
```

前端检查需 Node `>=22.22.0`；CI 与 Docker 构建阶段使用 Node 24。

人工确认：`changelog.md` 的 `[Unreleased]` 已含本次全部变更；受影响文档已对齐；版本号在各处一致。

### 滚动 Changelog

1. `[Unreleased]` 改名为 `[X.Y.Z] - YYYY-MM-DD`；2. 其上方新建空 `[Unreleased]`。

### 构建并推送镜像

```bash
VERSION=0.0.0; NS=quinlanhoo

# 登录（建议 PAT；非 TTY 用 --password-stdin）
echo "<token>" | docker login -u ${NS} --password-stdin

# 多架构需 docker-container driver 的 builder（默认 docker driver 不支持）
docker buildx create --name multiarch --driver docker-container --use --bootstrap 2>/dev/null \
  || docker buildx use multiarch

docker buildx build --platform linux/amd64,linux/arm64 \
  -t ${NS}/mihomo-subscription:${VERSION} -t ${NS}/mihomo-subscription:latest --push .
```

推送前可单架构冒烟：`docker build -t mihomo-subscription:${VERSION} .` →
`docker run --rm -p 8080:8080 -e ADMIN_USERNAME=admin -e ADMIN_PASSWORD=test -v "$(pwd)/tmp-data:/data" mihomo-subscription:${VERSION}` →
`curl -fsS http://localhost:8080/health`。

### 打标签 + GitHub Release

```bash
git tag -a v${VERSION} -m "Release v${VERSION}" && git push origin v${VERSION}
gh release create v${VERSION} --verify-tag --title "v${VERSION}" --notes "..."  # notes 取自 changelog 对应版本
```

### 发布后

- 确认 `[Unreleased]` 为空且在最新版本之上；GitHub Release 指向正确 tag；在 1Panel 用 compose
  拉取新镜像部署验证（登录、建配置、生成链接、客户端可拉取 YAML）。
- 发布缺陷走新 PATCH 版本，不覆盖已发布的镜像 tag。

### 可选：本地构建（离线/内网）

主机无法访问 Docker Hub 时，把仓库同步到主机本地构建，并把 compose 的 `image` 临时改为本地 tag
（镜像名须与 compose `image` 一致）：

```bash
docker build -t mihomo-subscription:${VERSION} .
```
