# memento 项目上下文

## 产品与边界

- Rust 单二进制个人收藏海报墙与私有连续日记。电影／游戏／图书收藏公开浏览；日记仅本人登录或持有 API Key 的程序可读写。严格单用户，唯一浏览器账号就是管理员，多 session 仅表示同一管理员的多设备/浏览器；不引入多用户、RBAC、目标、看板、审计或回收站。
- release 内嵌静态前端；收藏、海报、日记和会话持久化在外部 SQLite，管理员可编辑的非秘密功能设置持久化在外部 YAML，**数据与设置均不打包进二进制**。`static/` 保持原生 HTML/CSS/JS，无 npm 或打包器。
- 收藏写入方先搜索比对，再创建或按 ID 更新；无自动去重/upsert，创建必须带海报。日记不去重：正文相同也可表示不同记录。
- 日记日期是产品规则：未删除集合（`deleted_at IS NULL`）为空时用业务时区今天，否则用**未删除最大 `create_date` + 1 天**，允许未来日期，不追随实际提交时间；删除末篇后再次创建可重用日期，全部软删后重新用今天，但不代表物理空表。默认 `Asia/Shanghai`，`MEMENTO_DIARY_TIMEZONE` 接受 IANA 时区。不得改为“每次提交都用今天”。
- 日记编辑只改正文，不改日期/创建时间；日记 DELETE 软删除，保留正文、原日期/created_at，将 deleted_at 设为 UTC RFC3339 毫秒时间、updated_at 同值、version+1。列表 q/日期/total/分页及详情都过滤 deleted_at IS NULL。DTO 保持原六字段，不暴露 deleted_at；POST/PATCH 不允许指定该字段或恢复。本期无恢复、回收站或永久清除 API；收藏保持硬删除。两者均不承诺安全擦除，日记删除确认和成功文案须说明不再显示、正文保留及本期无恢复。

## 架构与职责

单 Cargo package，lib + bin；Axum 0.8/Tokio，rusqlite bundled SQLite + r2d2。

| 路径 | 职责 |
| --- | --- |
| `src/main.rs`、`src/cli.rs` | 薄入口；本地建库、密码 hash 命令不启动服务、不加载 dotenv |
| `src/lib.rs`、`src/state.rs` | 启动、路由/层、状态；不要把路由逻辑搬回 main |
| `src/config.rs`、`src/browser.rs` | 环境配置；浏览器认证配置、密码验证、持久会话及 CSRF |
| `src/settings.rs`、`src/handlers/settings.rs` | YAML 功能设置校验/原子持久化；管理员站点设置 HTTP 接口 |
| `src/handlers/` | HTTP 提取与校验、阻塞任务调度、响应 |
| `src/models.rs`、`src/repo.rs` | 收藏的 `PreparedCreate`/`PreparedUpdate` 校验归一化与纯 SQL |
| `src/diary.rs` | 日记 DTO、`PreparedContent`/查询校验及事务 SQL |
| `src/db.rs` | 连接池、WAL 前只读兼容性检查、事务化版本迁移 |
| `src/auth.rs`、`src/image.rs`、`src/error.rs` | API Key、图片魔数/抓取、安全错误信封 |
| `src/assets.rs`、`static/` | 静态资源、站点文本注入和共享导航；`/diary`、`/login`、`/admin` 均是不含私密数据的页面壳 |
| `tests/` | router HTTP 集成、CLI/schema、Python 导入测试；模块内另有 Rust 单元测试 |
| `scripts/` | 标准库客户端、旧收藏导入、旧日记离线导入及 agent 指南 |

- 请求中的取连接、同步数据库、密码 hash、图片解码/阻塞下载使用 `spawn_blocking`；启动数据库初始化和 CLI 是同步的。校验产生新的 `Prepared*`，不原地修改请求。
- SQL 用户值参数绑定；LIKE 搜索转义 `%`、`_`、反斜线。日记创建使用 `BEGIN IMMEDIATE` 包含取最大日期和插入；编辑/删除版本检查与写入同事务；列表 count/items 同一快照。

## 认证、数据与兼容性

- `/api/favorites` 公开读、API Key 写；`/api/diaries` **所有读写均需 `X-API-Key`**；`/private/diaries` 是同源浏览器会话入口。`GET/PUT /private/settings/site` 仅接受管理员 session，PUT 还需 CSRF、production Origin；API Key 不能替代。Cookie 与 API Key 不互相替代，也不因浏览器登录开放收藏写入。
- `/session` 的 POST/GET/DELETE 仅服务浏览器，不是第三方登录体系。两模式都必须配置 bcrypt `MEMENTO_PASSWORD_HASH` 和 Base32 `MEMENTO_TOTP_SECRET`（解码至少 20 字节），username 默认 admin。旧 Argon2 hash 不兼容，重生 hash 但不改日记/收藏数据。CLI hash-password（含 --stdin）使用 bcrypt DEFAULT_COST=12，密码非空且最多 72 UTF-8 字节以防截断，不另设 12 字符最低规则；totp-secret 生成新 20 字节 Base32，仅显示到用户终端，本地安全配置 6 位/SHA1/30 秒验证器，允许前后各 1 步，不依赖域名。
- 每次登录先 POST `/session` 的 username/password，正确仅返回 data{requires_totp:true,challenge}、无 Cookie；再 POST 同 URL 的 {challenge,code} 才返回 data{username,csrf_token} 与 Cookie。挑战 5 分钟、最多 5 次错误 OTP、64 并行待验证上限，仅限制短期 challenge。无恢复码或自助账户管理；丢设备由运维更换 secret，使旧 session 失效。
- 会话随机 token 只存 SHA256 摘要，`MEMENTO_SESSION_TTL_DAYS` 默认 7 且为正整数，不滑动续期，不限制有效 session 为 32 个。退出持久删除；用户名/hash/TOTP 变动踢旧会话，origin 变动不踢。schema v3 新增 browser_totp_state(credential_hash TEXT PRIMARY KEY,last_used_step INTEGER NOT NULL)：同一凭据只接受更大的 OTP 时间步，与 session 插入在同一 IMMEDIATE 事务提交，失败一起回滚。不得跨 challenge、重启或注销重用验证码；窗口内 401 提示等下一个验证码并重新登录，接受超前一步后须等更大时间步，时钟回拨可能暂时拒绝。保留前后各 1 步容差，无手动全局绕过开关；不是审计/账户平台。数据库失败必须拒绝授权，不能把撤销失败当成功。
- development 不要求 Origin/Host 比对，Cookie HttpOnly/SameSite=Lax/无 Secure；production 必须显式 canonical HTTP(S) `MEMENTO_PUBLIC_ORIGIN` 并精确校验浏览器 unsafe Origin，Cookie HttpOnly/SameSite=Strict，https origin 才 Secure。HTTP 内网生产反代可用，不强制域名/证书；公网推荐 HTTPS。两模式登录后 unsafe 均保留 session 绑定 CSRF，由前端自动处理，无用户配置。旧收藏 permissive CORS 不扩散到 Cookie 或日记组。
- 不保留应用 IP 分桶或全局 hash 限流，生产 `/session` 入口防滥用由 Nginx 负责；配置见 deploy/nginx，两个 http 上下文片段二选一，不能重复 zone/upstream。不重写 Origin/路径，不丢 Host 端口，不缓存或重试 POST，不记录敏感请求。
- 未配置/空白 `MEMENTO_API_KEY` 时，二进制禁用外部 key 接口，不输出临时 key；公开海报墙和已配置的浏览器登录仍可用。当前单 key 同时授权收藏写和日记读写，不是只写/分 scope 凭据。
- 日记新正文 trim 后非空、最多 10000 Unicode 字符，请求 64 KiB；登录 body 8 KiB。PATCH/DELETE 的 `If-Match: "<version>"` 可选，无头操作最新记录，不返回缺头 428；提供时格式错误 400、未删除记录版本陈旧 412。网页仍传版本防冲突，不自动合并。已删 ID 在前置校验正确时 PATCH/重复 DELETE 为 404；同版本并发 DELETE 仅一个 204、另一个 404；版本溢出 409 且不删除。POST 无幂等键，未知写入结果先核实，不能自动重试导致多篇。
- 旧日记可有重复日期、空白正文和 NULL 时间戳；迁移保留全部原字段，读取支持旧时间字符串，不能为了新提交校验清洗旧数据或增加日期唯一约束。
- 收藏三类共用 `favorites`：便利字段折入 `extra`，显式 extra 同名键优先。PUT 保持部分更新；可空字段显式 null 清空，不适用于 name/type/图片/sort_date。`extra` 是事务内浅合并，不是 JSON Merge Patch；null/空对象不清空整体，键值 null 不是删除键。
- 收藏固定 `sort_date DESC,id DESC` 排序；创建缺省 UTC 当天，更新不自动推进。收藏日期目前未严格校验，不能用日记的严格日期假设处理旧收藏。
- 首页 UI 已移除收藏名称搜索，只保留类型筛选/分页；公开收藏 API 的 `q` 参数继续保留。共享导航覆盖 `/`、`/diary`、`/login`、`/admin`，未登录账户入口去 `/login`，已登录去 `/admin`；登录 next 仅允许 diary/admin，默认 admin。
- 普通应用 JSON `{success,data,error}`，内部错误脱敏，BadRequest 文本会公开。日记提取拒绝也统一信封；删除 204 空体、图片原始字节。旧收藏提取器及静态资源并非全部使用信封。

## 必须保留的安全实现

- API Key 保留 `subtle::ConstantTimeEq`，不得换成 `==`。不记录日记搜索 URI、正文、密码、Cookie 或 CSRF；反代也需避免记录这些数据。
- 图片创建恰需一个非空 image_base64/image_url，更新至多一个，省略保留旧图。magic bytes 判断 MIME，不信声明；拒绝非栅格内容，接受上限 10 MiB，收藏写 body 20 MiB（不是所有下载路径的流式内存上限）。
- 远程图片抓取保留 HTTP(S)、DNS 全地址检查、固定合格解析地址、禁重定向整套防护，不只校验 URL 字符串。
- 保留 `nosniff`、`DENY`、`no-referrer`；日记/会话/后台设置响应 no-store。API 文本只用 textContent，来源链接限 HTTP(S)，YAML 站点 name/slogan/icon 保持对应 HTML 文本/属性转义。

## 运行、迁移与验证

- 从当前仓库根执行命令。Cargo.lock 跟踪，edition 2021；README 要求 Rust 1.96+，CI 浮动 stable，尚无验证过的 MSRV。需要 rustfmt/clippy 和 C 编译/链接工具；SQLite bundled，无外部数据库服务。
- 无参数 server 启动先从系统 `MEMENTO_ENV` 选择 development/production，默认 production，仅从进程 cwd 加载选定 `.env.development`/`.env.production`；不加载通用 `.env`、不找父目录，已有系统值优先，文件不能偷切模式。hash-password/totp-secret/init-db CLI 不加载任何 dotenv；直接调用库 run 不负责 dotenv/tracing。
- 开发先用 `cargo run -- hash-password` 和 `cargo run -- totp-secret` 生成本地配置，完成验证器设置，再 `MEMENTO_ENV=development cargo run`。Config 开发默认 bind `0.0.0.0:23457`、DB `./memento.dev.db`、YAML `./memento.development.yaml`；生产默认 bind `127.0.0.1:23457`、DB `./memento.db`、YAML `./memento.production.yaml`，相对 cwd 且不仅是样例值。生产 `MEMENTO_ENV=production ./memento`，经 Nginx HTTP 8080 接入并显式配置 `http://机器IP:8080` origin。HTTPS 是单独可选范例，不自动部署。CLI 显式检查密码非空且 <=72 UTF-8 字节，再调用 bcrypt::hash(DEFAULT_COST=12)，不要改用会拒绝恰 72 字节的 non_truncating_hash。
- Env 仅承载运行/安全/秘密/业务规则；YAML 仅承载后台可编辑非秘密功能设置；SQLite 承载业务数据且 schema 仍为 v3，导入器不变。server 首次缺 YAML 时创建，父目录须存在，Unix 新文件 0600；旧 site Env 只作一次性 seed，文件存在后忽略/弃用。无效既有 YAML 拒启动且不覆盖；CLI 不读 YAML。程序重写不保留注释，按单实例运行且不监听外部修改。站点约束和原子 rename 边界见管理后台设计。
- rust-embed 8 默认 release 内嵌，静态修改需重建重启 release；debug 未启用 debug-embed，读取文件系统资源。实际环境文件、默认运行 YAML、YAML 临时文件、凭据、数据库及 WAL/SHM 不入库；`memento.example.yaml` 可跟踪且不含秘密。自定义配置路径不一定被忽略，提交前检查。
- 当前 schema v3：v2 从 v1 添加 deleted_at TEXT，旧行默认 NULL；v3 添加 browser_totp_state。空库/既有收藏库、v1/v2 均在事务内按顺序升级到 3，历史为 1、2、3，保留会话、favorites、ledger、ID sequence 及原日记字段，不能只改 CREATE。拒绝 foreign/future/残缺 schema，不自动修复。Unix 新建数据库 0600，旧文件权限不自动更改；WAL/SHM 同属持久化边界。
- `memento init-db <path>` 初始化目标；`scripts/import_diaries.py --source ... --target ...` 默认只读 dry-run，`--apply` 才写目标，要求完整 v3（含新表元数据），不读取 TOTP 状态真实内容。不将旧 app.db 直接作为运行库。源五字段不变，新导入显式 deleted_at=NULL；ledger 匹配则跳过，不覆盖编辑、不清软删/不复活。软删 diary_id 仍指向原行，ON DELETE SET NULL 仅兼容历史物理删除。首次导入仍要求物理空目标日记表，全部软删不算空；未入账源 ID 不得复用目标历史分配 ID。具体停服/一致备份/回滚见操作文档。

按 CI 顺序执行：
```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all
cargo build --release
python3 -B -W error::ResourceWarning -m unittest discover -s tests -p test_import_diaries.py -v
```

聚焦与可选验证：
```bash
cargo test --test diary_api
cargo test --test browser_auth
cargo test --test migrations
cargo test --test api create_missing_image_is_bad_request -- --exact
cargo test --lib repo::tests::update_merges_extra_with_existing -- --exact
python3 -B tests/browser_smoke.py  # 先 release build，需 agent-browser CLI
python3 -B tests/nginx_smoke.py /path/to/nginx # 需 release、Nginx 和 openssl，仅隔离实例
cargo llvm-cov --all-targets --show-missing-lines --quiet # 可选，需 llvm-tools-preview/cargo-llvm-cov
```
- Rust/Python 测试使用临时库、内联 PNG 与合成凭据，不使用真实日记或服务。可选浏览器 smoke 验证 release 内嵌、两步登录、XSS、冲突、软删存储、窄屏和退出；Nginx smoke 使用临时 prefix/合成证书验证语法与真实 HTTP/HTTPS 反代，不操作系统服务。具体执行证据和覆盖率口径见运维文档，后续修改须重跑，不从已有记录推断当前通过或生产已部署。
- 未配置独立 typecheck、前端打包、codegen 或 npm 测试。覆盖率不是安全证明，公网图片/DNS成功路径、生产反代、跨平台发布及真实环境恢复需单独验证。`v*` 发布工作流不依赖 CI 成功，不将发布成功等同测试通过。

## 文档导航

- `README.md`：启动、收藏 API、文档入口；`CLAUDE.md`：已有开发约定，处理对应模块时显式按需读取。
- `docs/diary-design.md`：日记/认证/迁移设计决策。
- `docs/diary-api.md`：日记 API、版本条件和浏览器会话契约。
- `docs/diary-operations.md`：配置、密码设置、迁移/回滚、验证与限制。
- `docs/admin-settings-design.md`：严格单用户后台、Env/YAML/SQLite 边界、站点约束、导航、原子保存与平台限制。
- `scripts/AGENT_GUIDE.md`：外部客户端流程；`submit_example.py` 仅封装收藏 POST，不包 extra 是 helper 限制而非 HTTP 限制。`seed_import.py --skip-existing` 只按名称去重，不是 upsert；其无图开关和客户端无图演示不满足收藏创建契约。脚本不加载 dotenv，写入脚本不是验证入口。
