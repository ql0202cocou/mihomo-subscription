# Mihomo Subscription Manager

面向 1Panel 自托管的轻量级 Mihomo 订阅转换/分发服务(类 Sub-Store Lite):录入机场
订阅,配置自定义规则、节点、代理分组,生成长期有效的 Mihomo 订阅链接。带 Web 管理
页面、管理员登录、SSRF 防护与订阅 URL 脱敏。

> 状态:已实现并经安全审计加固。镜像发布在 Docker Hub
> (`quinlanhoo/mihomo-subscription:latest`,多架构 amd64+arm64),用 docker compose 部署。

## 在 1Panel 中部署

镜像发布在 **Docker Hub**(`quinlanhoo/mihomo-subscription`,多架构 amd64+arm64),
1Panel 主机直接拉取,无需本地构建。离线/内网请参见 `docs/deploy.md` 的本地构建备选。

最简单的方式是手写 Compose:在 1Panel「容器 → 编排」新建,把下面整段粘贴进去,
**只改 4 个标注 `← 修改` 的值**,创建即部署。

**1. 部署容器**

```yaml
services:
  mihomo-subscription:
    image: quinlanhoo/mihomo-subscription:latest
    container_name: mihomo-subscription
    restart: unless-stopped
    networks: [1panel-network]
    ports:
      - "8080:8080"                                  # 宿主:容器
    volumes:
      - ./data:/data                                 # 持久化 SQLite
    environment:
      # ── 必填 ──────────────────────────────────────────────
      - ADMIN_USERNAME=admin                         # ← 修改: 管理员账号
      - ADMIN_PASSWORD=change-me-to-a-strong-secret  # ← 修改: 用强密码
      - PUBLIC_BASE_URL=https://sub.example.com      # ← 修改: 对外访问地址(含 https)
      - SECURE_COOKIES=true                          # ← 修改: HTTPS 反代填 true,纯 HTTP 填 false
      # ── 固定,勿改 ─────────────────────────────────────────
      - PORT=8080
      - DATA_DIR=/data
      # ── 可选,留默认即可 ───────────────────────────────────
      - RUST_LOG=info                                # 日志级别
      - PUBLIC_PATH_PREFIX=                          # 留空自动生成随机前缀
      - FETCH_TIMEOUT_SECONDS=15                     # 机场拉取超时
      - MAX_SUBSCRIPTION_SIZE_MB=8                   # 机场响应大小上限
      - CACHE_TTL_MINUTES=15                         # 仅管理员预览缓存 TTL
      - PUBLIC_REFRESH_MIN_SECONDS=30                # 公开订阅回源刷新下限
      - TRUSTED_PROXY_HOPS=0                         # 默认不信任 XFF
      - TRUSTED_PROXY_CIDRS=                         # 可信反代 CIDR,留空忽略 XFF
    healthcheck:
      test: ["CMD-SHELL", "wget -qO- \"http://localhost:$${PORT:-8080}/health\" || exit 1"]
      interval: 30s
      timeout: 5s
      retries: 3
      start_period: 10s
networks:
  1panel-network:
    external: true
```

缺少 `ADMIN_USERNAME`/`ADMIN_PASSWORD` 时服务会拒绝启动;每个变量的含义见
[docs/deploy.md](docs/deploy.md) 的环境变量表。

**2. 反向代理** — `PUBLIC_BASE_URL` 必须等于浏览器访问的外部 origin,
并在 1Panel「网站 → 反向代理」指向该容器时**保留原始 Host 头**,否则管理 API 的 `Origin`
同源校验会让登录/写操作返回 `403`:

```nginx
proxy_set_header Host $host;
```

默认配置会忽略 `X-Forwarded-For`,按 TCP 对端做登录/下载限流。若后端端口不会被公网直连,
且需要按真实客户端 IP 限流,同时设置 `TRUSTED_PROXY_HOPS=1` 与反向代理所在的
`TRUSTED_PROXY_CIDRS`。

部署后管理界面在站点根路径。

## 使用

1. 用 compose 里的管理员账户登录。
2. 新建订阅配置，填写机场订阅 URL。
3. (可选)编辑分流规则、追加自定义节点和代理分组。
4. 点击生成,得到永久订阅链接,复制到 Mihomo 客户端:

```text
https://<host>/<public-path-prefix>/api/sub/<profile-token>
```

链接长期有效;可重置单个配置的 token,或在系统设置重置全局路径前缀以一次失效所有链接。

## 文档与开发

参考文档见 [docs/](docs/)（[architecture](docs/architecture.md)、
[deploy](docs/deploy.md)、[changelog](docs/changelog.md)）。CI 门禁见
[.github/workflows/ci.yml](.github/workflows/ci.yml)，发布与变更规则见
[docs/deploy.md](docs/deploy.md)。

前端本地构建需 Node `^20.19.0 || >=22.12.0`;CI 与 Docker 镜像构建使用 Node 22。

## 许可证

[MIT](LICENSE)
