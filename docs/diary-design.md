# 登录与私有日记设计

本文描述登录与连续日记的实现设计。接口见 [diary-api.md](diary-api.md)，管理后台与 YAML 见 [admin-settings-design.md](admin-settings-design.md)，部署、迁移、回滚及验证摘要见 [diary-operations.md](diary-operations.md)。本地验证不代表生产已迁移或上线。

## 范围与决策

- 保留电影、游戏、图书收藏的公开浏览和 API Key 写入；新增单账号浏览器登录、私有日记 CRUD、Agent 日记 API 和旧库离线导入。
- 不实现 stats、goal、独立 JSON 导入导出接口、audit 或第三方登录体系。HTTP 请求和响应使用 JSON，不等于存在额外的 `/json` 功能。
- 日记与收藏分表，但共用 SQLite 文件、进程和当前唯一的 `MEMENTO_API_KEY`。key 持有者可读取、创建、编辑、删除全部日记，也能写收藏；没有只读 key、日记专用 scope 或按 Agent 分权。
- 服务启动未设置/空白 key 时，`run()` 向 `AppState` 传空 key，禁用收藏写入、日记 key API 和 `/api/auth/verify`，仅日志提示禁用，不输出密钥；收藏公开读取和单独配置的浏览器登录仍可用。`Config` 内部旧随机值保留兼容，但不会被启动流程用于鉴权或输出。
- 浏览器不用 API Key。`/session` 和 `/private/diaries` 是同源浏览器专用协议，不是给第三方换取 token 的登录平台；Agent 只调用 `/api/diaries`。
- 浏览器模型严格单用户：唯一账号就是管理员，多 session 仅表示同一人的多设备/浏览器，不增加用户、角色或 RBAC。管理员 session 也用于 `/private/settings/site`，但 API Key 不能替代。
- 业务日记日期与实际写入时间分离。日期代表连续编排的位置，不是每次请求的自然日，也不限制一天只能创建一篇。
- 日记删除是软删除：不再显示，但原行及正文仍保留；本期无回收站、恢复、永久清除 API 或内容审计。收藏保持硬删除。两者都不保证数据库文件、WAL 或备份中的内容安全擦除；整库备份回滚不是单篇恢复接口。

## 模块边界

| 实现 | 职责 |
|---|---|
| `src/main.rs`、`src/cli.rs` | 先分派本地 CLI；无参数才加载 dotenv、初始化 tracing 并启动服务 |
| `src/lib.rs`、`src/browser.rs` | 路由及安全层；双环境配置、bcrypt/TOTP、challenge/session、Origin/CSRF |
| `src/settings.rs`、`src/handlers/settings.rs` | 非秘密 YAML 功能设置校验、原子持久化及管理员站点设置接口 |
| `src/handlers/diary.rs` | 两组日记路由共用的 HTTP 提取、脱敏错误、If-Match、ETag 与阻塞任务调度 |
| `src/diary.rs` | 与 HTTP 无关的正文校验、查询和事务内日期分配、版本校验、软删除 |
| `src/db.rs` | WAL 连接池、事务化 schema 升级及版本拒绝策略 |
| `scripts/import_diaries.py` | 旧库只读读取，目标校验、保真导入及 ledger 幂等 |
| `static/diary.html`、`static/diary.js`、`static/diary.css` | `/diary` 页面，同源 cookie 请求；原生 HTML/CSS/JS，无前端依赖 |

请求中的取连接、同步 SQL 和密码哈希验证都通过 `spawn_blocking` 执行。创建、编辑、删除使用 SQLite `BEGIN IMMEDIATE`，先取得写锁再读最大日期或版本；列表计数和分页读取位于同一读事务，保持单次响应内的快照一致。跨页请求不共享快照，写入期间翻页仍可能移动条目。

## 数据与日期

`diaries` 的字段为 `id`（自增主键）、`content`、`create_date`、可空的 `created_at`/`updated_at`、正整数 `version`（默认 1），以及 v2 新增的可空 `deleted_at TEXT`。DTO 保持原六字段，不暴露 `deleted_at`。日期索引为仅包含未删除记录的 `(create_date DESC, id DESC)` 部分索引，没有日期唯一约束，以保留旧库同日多篇。收藏和日记的 ID 序列相互独立。

1. 未删除集合（`deleted_at IS NULL`）为空时，新篇日期取业务时区的今天。默认 `Asia/Shanghai`，服务启动读取 `MEMENTO_DIARY_TIMEZONE`；无效时区使启动失败。
2. 未删除集合非空时，新篇日期严格为该集合的 `MAX(create_date) + 1 day`，不取最大 ID 的日期，也不与今天取较大值。未删除的历史最大日期落后或领先今天时仍按该规则递增。
3. 删除末篇后，下次创建可能重用其日期，这是明确的产品规则，不是 bug；删除中间条目不会补洞。全部软删后再次按业务今天起步，但物理表并不为空。日期重用不代表 ID 重用。
4. 编辑只接收正文，保留 ID、日记日期和 `created_at`；每次成功编辑（即使正文相同）都会递增版本，并更新 `updated_at`。

新正文先做 Rust `str::trim()`，再验证非空且不超过 10000 个 Unicode 标量值（`chars().count()`，不是 UTF-8 字节数、UTF-16 长度或视觉字素数）。POST/PATCH 只允许 `content` 字符串，拒绝未知字段和服务端管理字段，包括 `deleted_at`，不能用于恢复。JSON 请求体还有独立的 64 KiB 字节上限，转义后的 JSON 可能先触及该上限。

新建的两个 timestamp 都是当前 UTC RFC3339 字符串，含毫秒；编辑只重写 `updated_at`。旧 timestamp 为 null 或不透明旧字符串时原样保留，客户端不能假定全部可按 RFC3339 解析。导入不套用新正文规范，空白、换行、超长历史正文及重复日期均保真；编辑这些历史记录时，新提交正文仍须符合当前限制。

DELETE 在事务内保留正文、原日期和 `created_at`，设置 UTC RFC3339 毫秒 `deleted_at`，`updated_at` 与其同值，版本加一。列表的 q/日期过滤、total、分页及详情都限定 `deleted_at IS NULL`。PATCH/DELETE 的 If-Match 可选，无头操作最新记录，不返回缺头 428；提供时格式错误为 400、未删除记录过时版本为 412。网页仍传版本防冲突，无自动合并。已删 ID 在前置校验正确时返回 404；同版本并发删除仅一个 204、另一个 404；版本溢出返回 409 且不删除。

## 鉴权与安全

| 入口 | 权限与边界 |
|---|---|
| 收藏 GET | 继续公开 |
| 收藏写入、`GET /api/auth/verify` | `X-API-Key` 常量时间比较 |
| `/api/diaries` 及其详情路由 | 全部读写必须有 key；cookie 不能替代 |
| `/private/diaries` 及其详情路由 | 全部读写必须有有效 session；key 不能替代；写入两模式均需 CSRF，生产另需精确 Origin |
| `/private/settings/site` | GET/PUT 必须有管理员 session；PUT 另需 CSRF，production 需精确 Origin；key 不能替代 |
| `/session` | POST 分密码、TOTP 两步；GET 查询 session；DELETE 撤销 session |
| `/diary` | 公开加载页面壳，不含日记数据；数据另外鉴权获取 |

系统 `MEMENTO_ENV` 先选择 development/production，默认 production。无参数 server 仅加载 cwd 的选定 `.env.development`/`.env.production`，不读通用 `.env` 或父目录，系统值优先，文件不可切换模式。hash-password、totp-secret、init-db 不加载任何 dotenv。开发样例 bind `0.0.0.0:23457`、DB `./memento.dev.db`；生产样例 bind `127.0.0.1:23457`、DB `./memento.db`，经 Nginx 接入。模式在启动命令显式指定，不能靠 Cargo debug/release 推断。

Env 仅保存运行、安全、秘密和业务规则；管理员可编辑的非秘密站点功能设置进入 YAML；SQLite 继续保存业务数据，schema 保持 v3，导入器不变。YAML 开发/生产默认分别为 cwd 下 `./memento.development.yaml` / `./memento.production.yaml`，首次缺失时 server 创建，CLI 不读取。旧 site Env 只作首次 seed，文件存在后忽略。详细生命周期、字段与原子保存边界见管理后台设计。

两模式均要求 bcrypt `MEMENTO_PASSWORD_HASH` 和 Base32 `MEMENTO_TOTP_SECRET`（解码至少 20 字节），用户名默认 admin。不兼容旧 Argon2 hash，须重生但不改日记/收藏数据，不迁移旧认证数据。hash-password 及 --stdin 使用 bcrypt DEFAULT_COST=12，密码非空且至多 72 UTF-8 字节，防止算法截断，不另设至少 12 字符规则。totp-secret 生成新 20 字节 Base32 secret，仅在用户终端显示；本地安全登记验证器（6 位、SHA1、30 秒、前后各 1 步容差），不依赖域名。

每次登录第一步 POST `/session` 的 username/password，正确后仅返回 data{requires_totp:true,challenge}，不设置 Cookie。第二步 POST 同 URL 的 {challenge,code}，TOTP 正确才返回 data{username,csrf_token} 和 Cookie。challenge 为短期状态：5 分钟有效、最多 5 次错误 OTP、最多 64 并行待验证；不是长期 session 上限。没有恢复码或自助账号管理；丢设备由运维替换配置 secret、重新登记验证器，旧 session 随之失效。禁止记录密码、secret、code、challenge 或会话凭据。

development 不比较 Origin/Host，Cookie HttpOnly/SameSite=Lax/无 Secure。production 必须显式 canonical HTTP(S) PUBLIC_ORIGIN，不含路径、query、fragment 或 userinfo，unsafe 浏览器请求需精确单一 Origin；Cookie HttpOnly/SameSite=Strict，https origin 才带 Secure。两模式的登录后写入/退出都需要 session 绑定的 X-CSRF-Token，常量时间比较，由前端自动处理，无用户配置项。SameSite 不能替代 CSRF。HTTP 内网生产反代无需域名/证书，公网推荐 HTTPS 而非强制所有生产 HTTPS；不保留自动 Origin 模式。

session 使用随机 32 字节 token，cookie 名 memento_session，Path=/、无 Domain，Max-Age 按 `MEMENTO_SESSION_TTL_DAYS`（默认 7，正整数）计算。数据库只存 token SHA256 摘要、独立随机 CSRF、凭据指纹和到期时间，不滑动续期、不设 32 个有效 session 上限。重启保留未过期会话；用户名/hash/TOTP 改变踢旧会话，origin 改变不踢。恢复旧数据库/凭据可能恢复仍未过期会话，安全事件后不要回退泄露配置。

schema v3 新增 `browser_totp_state(credential_hash TEXT PRIMARY KEY,last_used_step INTEGER NOT NULL)`。密码/TOTP 正确后，在 session 插入的同一 IMMEDIATE 事务内只接受比持久 last_used_step 更大的时间步；失败时状态与 session 一起回滚。防重用跨 challenge、进程重启和注销保持，不能靠重新申请 challenge 复用同一验证码。前后各 1 步是时钟容差，不是重放许可；窗口内重复提交返回 401，应等下一个验证码并重新开始登录。若先接受超前一步，须等更大的时间步；时钟回拨会暂时拒绝，无手动全局绕过开关。新表是单账号最小防重用状态，不是审计/账户管理平台。

Config 的实际默认值按模式区分：development 为 `0.0.0.0:23457` / `./memento.dev.db`，production 为 `127.0.0.1:23457` / `./memento.db`，不仅是样例。CLI 先显式校验密码非空且 <=72 UTF-8 字节，再调用 bcrypt::hash(DEFAULT_COST=12)，保证恰 72 字节有效；不使用会拒绝该边界的 non_truncating_hash。

删除应用 IP 分桶及全局 hash 限流，生产防滥用交给 Nginx `/session` 入口（两步共享限流，示例 10r/m、burst 10、拒绝 429）。代理保留 Host 端口和原始 Origin、默认透传 Cookie/CSRF/If-Match/Key，不 rewrite 路径或将 Origin 固定成可信值；不缓存、不自动重发 POST。HTTP/HTTPS 两份替代配置均为 http 上下文 include 片段，部署与日志边界见运维文档。

日记已注册路由、`/session` 和 `/diary` 响应经 `no-store` 层，包含鉴权及校验失败；保留全局 `nosniff`、`DENY` 和 `no-referrer`。旧收藏路由组保留 permissive CORS，不向日记或 cookie 组扩散；不需要为了浏览器同源登录开放 credentialed CORS。未知 URL、静态资源或反代生成的响应不可一概视为同一 JSON/no-store 契约。

应用 trace span 只记录 method，不记录 URI，防止搜索 query 泄露。日记提取错误不回显原始输入；内部错误只向客户端给通用消息。运维仍须限制服务日志访问，并确保反代/CDN 不记录 query、cookie、认证头或正文。前端用 `textContent` 展示日记而非解释 HTML；草稿仅在当前页面保留，不写 localStorage、不自动保存。主动退出清除草稿，会话过期后的同页重新登录可保留草稿，刷新或关闭页面不能视为可靠保存。

## 迁移决策

`memento_migrations` 当前版本为 3，历史顺序为 1、2、3。v1 创建日记、session 和导入 ledger；v2 添加 nullable deleted_at，历史行默认 NULL；v3 添加 browser_totp_state。空库/既有收藏库、v1/v2 均在同一事务中按需顺序升级到 v3，保留原字段、会话、favorites、ledger 和 ID sequence，不以直接创建最终表替代迁移历史。build_pool 先只读预检再启用 WAL，init_schema 在 BEGIN IMMEDIATE 内再次校验升级。Unix 新建数据库 0600，已有权限不自动更改。

预检及事务内验证拒绝外来同名日记表、非预期版本序列和残缺 v1/v2/v3；无版本数据库只接受空库或符合既有收藏结构的旧库。按版本检查日记/session/ledger/收藏完整元数据、约束和外键，v2 检查删除列，v3 检查 browser_totp_state 列类型、主键及非空约束。版本标记不是完整性证明，schema 检查不替代 SQLite integrity/foreign-key 检查。导入器要求完整 v3，检查新表元数据但不读其真实内容，源五字段及 fingerprint 不变。不能将旧 e4 源库直接作为服务数据库。

`diary_imports` 保存 `source_id`、可空 `diary_id` 和源行 fingerprint。fingerprint 对原五个源字段的紧凑 ASCII JSON 数组做 SHA-256，不增加源字段；新导入显式写 `deleted_at=NULL`。软删除后 `diary_id` 仍指向原行；`ON DELETE SET NULL` 仅兼容历史物理删除。再次导入匹配 ledger 时直接跳过，不覆盖本地编辑、不清除软删标记或复活记录。首次导入要求目标日记物理空表，全部软删不符合要求，收藏可以已有数据。fingerprint 不是内容审计或恢复副本；ledger 只按源 ID 识别，不是多源命名空间，不能用来任意合并多个旧库。

导入还读取目标 `sqlite_sequence` 中日记序列的导入前 high watermark：每个未入 ledger 的源 ID 都必须严格大于它，否则整批拒绝。即使目标表已空，历史使用过的本地 ID 仍不能以版本 1 复用，避免旧 ID/版本条件误命中新记录。这是 ID 安全边界，不改变删除末篇可重用业务日期的规则。

## 验证边界

测试入口包括 diary_api、browser_auth、migrations、cli、Python 导入测试和模块单元测试，覆盖日期、可选版本条件、双环境、bcrypt/TOTP 挑战与持久防重用、会话/CSRF、schema 升级及保真幂等。browser_smoke.py 验证真实浏览器两步登录、冲突草稿、软删存储及退出；nginx_smoke.py 使用隔离 prefix/合成证书验证两份配置及真实 HTTP/HTTPS 反代。具体执行证据、覆盖率口径和未验证范围见运维文档；不能用合成测试推断生产已迁移或部署。
