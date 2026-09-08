# 日记部署、迁移与回滚

末尾记录本地验证结果，不代表生产已迁移或上线。命令中的路径和 origin 均为示例，执行前由运维确认部署目录、文件权限及备份位置。

## 部署准备

release 二进制内嵌静态资源，但收藏、图片、日记和 session 持久化在外部 SQLite 文件，不在二进制中。默认 debug 从工作区读取静态资源，不能当作独立静态资源包交付。升级页面必须重新构建并重启 release 二进制。

| 配置 | 行为 |
|---|---|
| `MEMENTO_DB_PATH` | 开发默认 `./memento.dev.db`，生产默认 `./memento.db`，绝不能指向旧日记源库 |
| `MEMENTO_ENV` | 系统环境选择 development/production，默认 production；配置文件不能切换模式 |
| `MEMENTO_BIND` | 开发默认 `0.0.0.0:23457`，生产默认 `127.0.0.1:23457`，不仅是样例；生产经 Nginx 接入 |
| `MEMENTO_API_KEY` | 需要外部 API 时显式设置固定强 key；未设置/空白禁用全部 key 鉴权接口，启动只提示禁用、不输出密钥 |
| `MEMENTO_LOGIN_USERNAME` | 登录启用时默认 `admin`；非空且最多 256 UTF-8 字节 |
| `MEMENTO_PASSWORD_HASH` | 两模式必填 bcrypt hash，无默认密码；旧 Argon2 hash 须重生，不改业务数据 |
| `MEMENTO_TOTP_SECRET` | 两模式必填 Base32 secret，解码至少 20 字节，无默认 secret |
| `MEMENTO_SESSION_TTL_DAYS` | 默认 7，正整数天，不滑动续期、不限制有效 session 为 32 个 |
| `MEMENTO_PUBLIC_ORIGIN` | production 必须显式 canonical HTTP(S) origin；development 不做 Origin/Host 比对 |
| `MEMENTO_DIARY_TIMEZONE` | 默认 `Asia/Shanghai`；可解析的时区名，非法值使启动失败 |

开发与生产都必须配置密码和 TOTP，不能用开发模式绕过第二因素。默认用户名 admin。保留现有收藏 key 也意味着将日记读写权限授予所有现有 key 持有者；若他们不应接触日记，应先重新分配信任边界，本期不支持细分权限。

反过来，不配置 key 可以独立使用浏览器登录和私有日记，收藏公开读取也不受影响；收藏写入、`/api/diaries` 全部读写和 `/api/auth/verify` 则返回 401。`Config` 内部虽仍生成旧兼容随机值，`run()` 会改传空 key 给 `AppState`，不使用或输出该随机值；不要从日志寻找临时凭据。

无参数 server 启动先从系统 MEMENTO_ENV 选择模式，仅从进程 cwd 加载选定 `.env.development` 或 `.env.production`；不加载通用 `.env`、不向父目录查找，也不按二进制位置寻找。系统已有值优先，文件内的 MEMENTO_ENV 不能偷切模式。`hash-password`、`totp-secret`、`init-db` 均不加载任何 dotenv。样例为根目录 `.env.development.example` / `.env.production.example`，hash/secret 故意留空。实际配置仅供部署账号读取，勿置于 static、可下载目录、Git 或日志附件；不要假定旧忽略规则已保护新文件名。

## 最短内网启动

源码目录先运行凭据工具，参照 `.env.development.example` 在 cwd 配置开发文件，并安全登记本地验证器：

```bash
cargo run -- hash-password
cargo run -- totp-secret
# 完成 .env.development 的 hash/secret 配置后启动
MEMENTO_ENV=development cargo run
```

二进制部署对应：

```bash
./memento hash-password
./memento totp-secret
# 完成 .env.production 的 hash/secret/origin 配置与反代准备后启动
MEMENTO_ENV=production ./memento
```

hash 值保留单引号避免 `$` 插值，不把明文密码或 secret 写进命令。开发样例 DB `./memento.dev.db`，生产样例 DB `./memento.db`，相对路径基于 cwd。首次启动会初始化新库；已有目标升级仍须停服和一致备份，不能用旧 app.db 作为运行库。

开发默认监听 `0.0.0.0:23457`，可信内网浏览器访问 `http://<机器内网IP>:23457/diary`；0.0.0.0 不是访问 URL。生产默认仅监听 `127.0.0.1:23457`，浏览器经 HTTP Nginx 的 `http://机器IP:8080/diary` 访问，PUBLIC_ORIGIN 填 `http://机器IP:8080`，无需域名/证书。确认网络可达和防火墙允许外部端口，不暴露上游端口。测试实例可以另设端口，不改变产品默认值。

## 新密码与初始化

先限制后续新文件权限，再运行本地 CLI。以下初始化示例用于新目标；若路径已有数据库，必须先按下文停服并做一致备份再升级：

```bash
umask 077
./memento hash-password
./memento totp-secret
./memento init-db /var/lib/memento/memento.db
```

`hash-password` 从终端无回显读取两次相同密码，用 bcrypt DEFAULT_COST=12 生成随机盐 hash。密码非空且最多 72 UTF-8 字节，不 trim；这是算法限制以防截断，不另设至少 12 字符规则。仍应选择强且唯一的密码。旧 Argon2 hash 不兼容，重新生成配置即可，不清空或修改日记/收藏。避免录屏、共享终端或 stdout 采集，hash 也按敏感配置保护。

非交互入口为 `memento hash-password --stdin`，供测试/集成通过受控 stdin 提供密码；去除一个末尾 LF 及其前 CR，无二次确认。不要用含真实密码的命令行字面量或 shell history 传入，不要开启 shell xtrace。

`totp-secret` 生成全新 20 字节随机 secret 的 Base32 表示，显示到用户终端，不写日志。用户须在本地安全配置 authenticator：6 位、SHA1、30 秒，服务允许前后各 1 时间步；服务器/设备时间须准确，不依赖域名或外部二维码网站。禁止上传 secret 到第三方生成二维码，不提交 secret/code/password 到 Git。

在受保护的选定环境文件中以单引号包围完整 hash，避免 `$` 插值。以下只是不能直接登录的空值模板，实际运行参照根目录两份环境示例：

```dotenv
MEMENTO_LOGIN_USERNAME=admin
MEMENTO_PASSWORD_HASH=''
MEMENTO_TOTP_SECRET=''
MEMENTO_SESSION_TTL_DAYS=7
MEMENTO_DIARY_TIMEZONE=Asia/Shanghai
MEMENTO_BIND=127.0.0.1:23457
MEMENTO_DB_PATH=/var/lib/memento/memento.db
MEMENTO_PUBLIC_ORIGIN=http://192.168.1.20:8080
```

实际文件应仅允许部署账号读取（例如 0600，父目录也限制访问），置于进程 cwd 的所选环境文件，或由服务管理器安全注入系统环境。若启用外部 API，固定 key 另行安全注入。不要使用空模板启动，也不从旧系统迁移密码、TOTP、session 等认证信息。

`init-db <path>` 明确初始化/升级指定目标，打印 `database initialized`；它不启动服务器、不加载任何 dotenv、不初始化常规服务日志，也不生成或记录 API Key。hash-password/totp-secret 同样在服务配置之前执行。目标父目录须已存在且可写；init-db 会真实写数据库，不是 dry-run。未知 CLI 参数报错而非启动服务。

`build_pool` 对已有数据库先只读预检 schema，拒绝外来日记库、未来版本和残缺 v1/v2/v3，通过后才启用 WAL。init_schema 在 IMMEDIATE 事务内再次校验升级，不仅相信版本标记。v2 添加 nullable deleted_at，旧行默认 NULL；v3 添加 browser_totp_state(credential_hash TEXT PRIMARY KEY,last_used_step INTEGER NOT NULL)。支持空库/既有收藏库、v1/v2 顺序升级到 3，历史 1、2、3，保留原日记字段、会话、收藏、ledger 和 ID sequence。Unix 新文件 0600，旧权限不自动修改；升级前检查库、目录、WAL/SHM 和备份权限。预检不替代完整性或备份校验。

## 环境与反代

| 边界 | development（轻量直连） | production（反代示例） |
|---|---|---|
| 密码与 TOTP | 都必须 | 都必须 |
| Origin/Host | 不要求比对 | 必须显式 canonical PUBLIC_ORIGIN，unsafe 精确 Origin |
| Cookie | HttpOnly、SameSite=Lax、无 Secure | HttpOnly、SameSite=Strict，https origin 才 Secure |
| 登录后 unsafe | session + CSRF | session + CSRF + Origin |
| 入口防滥用 | 不提供应用 IP/hash 限流，仅用于可信测试 | Nginx `/session` 限流 |

CSRF 由前端自动处理，无用户配置；API Key 分组规则不变。生产可用 HTTP 内网反代，HTTPS 是公网/不可信网络推荐方案，不是内网前置条件。PUBLIC_ORIGIN 必须是浏览器实际访问的规范 origin，如 `http://192.168.1.20:8080`，无路径/尾斜线/query/fragment/用户信息，不是上游 `127.0.0.1:23457`。不再提供自动 Origin 模式。

### 配置放置

仓库仅提供两份明确的替代方案，不自动安装、部署、启动或 reload Nginx：

- `deploy/nginx/memento.conf`：HTTP 内网，listen 8080、server_name _、upstream 127.0.0.1:23457。无需域名/证书，PUBLIC_ORIGIN 填 `http://机器IP:8080`。
- `deploy/nginx/memento-https.conf`：可选 HTTPS，443 ssl 与 80 固定目标 308 跳转；替换两处 server_name、跳转目标及 cert/key 路径。须已有覆盖自有 HTTPS 名/IP 的证书，不使用占位证书部署。

两文件是可整体 include 的 **http 上下文片段**，包含 limit_req_zone、upstream 和 server，适合已有 nginx.conf 在 http {} 内加载的 conf.d。不是直接 `nginx -c` 使用的 main 上下文完整配置，也不能放进 server/location 内。二选一启用，不能让 conf.d 通配符同时加载它们，否则同名 zone/upstream 冲突。不另拆公共 include；若已有其他服务配置，运维须检查监听、命名和继承冲突。

1. 所有路径原样代理：`/`、`/diary`、`/static/`、`/session`、两组日记与收藏 API；proxy_pass 无 URI 后缀、不 rewrite、不用 SPA fallback 吞掉 API 404。
2. `Host $http_host` 保留外部端口，`Origin $http_origin` 原样传递，不能将来源重写为固定可信值。Cookie、X-CSRF-Token、If-Match、X-API-Key 默认透传；Set-Cookie、ETag、Cache-Control 和安全响应头保留。
3. server 级 proxy_cache off、proxy_buffering off 覆盖敏感路径，不覆盖应用 no-store；proxy_next_upstream off 禁止上游失败自动重发 POST。不得增加 cookie 跨域 CORS。
4. 全站 body 20m 保留收藏图片上传，精确 `/session` 8k，`/api/diaries`、`/private/diaries` 及其子路径 64k；代理拒绝可能是非 JSON。
5. map 和 limit_req_zone 在 http 上下文定义，`/session` 使用 limit_req；示例 10r/m、burst 10、nodelay、失败 429，密码及验证码 POST 共用。GET/DELETE 使用空 key 不计入限流，不阻止会话查询或退出。可按个人使用调整。直接面向客户端以 remote address 分流；若再加前置代理，另行明确可信真实 IP 边界，不盲信 XFF。应用不再有 IP 分桶或全局 hash 限流，64 个待验证 challenge 不是长期 session 限制。
6. access_log off 避免 query/headers 被访问日志采集；error_log warn 不启用 debug/trace 或正文/认证头采样。**Nginx 错误日志和上级日志仍可能包含请求 URI**，因此限制日志权限、保留周期及错误采样，不承诺全部日志绝无隐私；同时检查 CDN/WAF/APM，不运行 curl verbose/trace 记录凭据。

### 临时语法验证

使用可用的 Nginx，在临时目录建立隔离 prefix 和主配置；主配置含 `events {}`、`http { include /绝对路径/片段; }`，pid/error 日志定向临时目录。分别测试 HTTP 与 HTTPS，不能同时加载两份；副本的显式 error_log 也须指向临时目录。HTTPS 副本用合成证书替换占位路径，不读取实际私钥。仅做语法检查可执行下列命令，不启动或 reload 系统服务：

```bash
nginx -t -p /tmp/opencode/memento-nginx-check/ -c /tmp/opencode/memento-nginx-check/nginx.conf
```

前提是临时目录/主配置已创建、Nginx 带 HTTP proxy/limit_req 模块，HTTPS 另需 SSL 模块及可用的合成证书。自动化语法与真实反代回归可用 `python3 -B tests/nginx_smoke.py /path/to/nginx`，它只启动和清理隔离测试进程，不操作系统服务。语法通过本身不等同真实反代或大图上传通过，已执行范围见末尾摘要。

### 两步登录与会话

每次 POST `/session` 先传 username/password，正确只返回 data{requires_totp:true,challenge}、无 Cookie；再向同 URL POST {challenge,code}，通过后才 data{username,csrf_token}+Cookie。challenge 5 分钟有效、最多 5 次错误 OTP、最多 64 个并行待验证；长期 session 不设 32 个上限。MEMENTO_SESSION_TTL_DAYS 默认 7 且为正数，不滑动续期。用户名/hash/TOTP 变动踢旧会话，origin 变动不踢。无恢复码或自助账号管理平台，丢设备由运维更换配置 secret 并重新登记验证器。

schema v3 的 browser_totp_state 按凭据指纹保存已成功使用的时间步。OTP 通过后，只允许更大的时间步，与 session 插入在同一 IMMEDIATE 事务提交；失败一起回滚。同一验证码不能跨 challenge、重启或注销重用，窗口内重复使用返回 401 时应等下一个验证码并重新从密码开始。前后各 1 步仍为时钟容差；接受超前一步后须等更大的时间步，时钟回拨可能暂时拒绝。请保持系统和验证器时间准确，不清表或提供手动全局绕过开关。该表只是单账号防重用状态，不是审计或账户管理平台。

应用 trace 不记录 URI，但内部错误仍可能进入服务端日志，不能声称全部日志绝无隐私。数据库没有静态加密功能；保护磁盘、备份、部署账号与日志访问权限。

日记 DELETE 是软删除，正文、原日期和 created_at 仍保留在数据库中；deleted_at 写 UTC RFC3339 毫秒时间、updated_at 同值、version 加一。列表/搜索/日期筛选/计数/分页和详情不再显示这些记录。本期无恢复、回收站或永久清除 API，不能把删除成功理解为安全擦除或承诺能够单篇恢复。收藏继续硬删，同样不承诺清除 WAL、空闲页或备份。新日记仅按未删除最大日期加一天；未删除集合为空时用业务时区今天，全部软删并不意味着物理空表。

## 离线迁移

### 前置与备份

保留原始源文件，只读使用，严禁将 `src_db`/旧 e4 库设置为 `MEMENTO_DB_PATH` 或传给 `init-db`。导入脚本只读取 `diaries` 的 `id/content/create_date/created_at/updated_at`，不读取认证表或其他业务模块。

1. 停止旧源服务和 memento 目标服务，并暂停所有 Agent/定时任务写入；导入器不会替你检测或停止服务。保持停服直到迁移核验结束。
2. 对 source 和现有 target 分别做 SQLite 一致备份，并记录对应二进制版本、配置、备份时间及恢复位置。新目标尚不存在时记录该事实，初始化后、导入前再备份目标。
3. 使用 SQLite backup API 或 SQLite 工具的 backup 功能生成独立一致快照；验证备份可打开及完整性。不要随意 `cp` 活跃 WAL 模式下的 `.db`，也不要漏掉尚未 checkpoint 的提交。只有全部连接确已关闭且正确完成 checkpoint，才可考虑离线文件级备份；不要手工删除 WAL/SHM 充当 checkpoint。
4. 用新二进制对独立目标执行 init-db，导入器要求完整 schema v3，不能直接向 v1/v2 导入。空库/既有收藏库、v1/v2 按需顺序升级到 3，历史为 1、2、3；保留收藏、会话、日记、ledger 和 ID sequence。导入器检查 browser_totp_state 元数据但不读其真实内容，源五字段不变。已有文件先只读预检再启用 WAL，拒绝外来、未来或残缺 schema，事务内再次验证。初始化也是写操作，须在备份之后。

下面假定 source 是已生成的一致源快照，target 是停止服务后的独立 memento 库：

```bash
python3 -B scripts/import_diaries.py \
  --source /srv/legacy/source-snapshot.db \
  --target /var/lib/memento/memento.db
```

默认即 dry-run，没有 `--dry-run` 参数。两端都须存在；source 用 `mode=ro`、`query_only` 和读事务，dry-run target 也只读。脚本拒绝相同路径、符号链接或硬链接实际指向同一文件。只有明确带 `--apply` 才以读写模式打开 target，并在单个 `BEGIN IMMEDIATE` 事务内写入日记及 ledger：

```bash
python3 -B scripts/import_diaries.py \
  --source /srv/legacy/source-snapshot.db \
  --target /var/lib/memento/memento.db \
  --apply
```

### 保真与幂等

- 首次导入（ledger 为空）要求目标日记表物理为空，不要求收藏为空；全部软删仍有物理行，不满足此条件。目标为空仍须满足下述 ID high watermark 条件。必须先迁移再允许新建日记，不要为满足检查而删除现有目标日记。
- 原 ID、正文、日期和两个 timestamp 原值保留，新版本设为 1，显式设置 deleted_at=NULL；source 仍只读原五字段，fingerprint 输入不变。空白正文、重复日期、超出新内容上限的旧正文均保留，不 trim、不去重、不重排日期、不伪造 timestamp。
- 源 ID 须为互不重复的正整数，正文须为字符串，日期须为有效 `YYYY-MM-DD`，timestamp 须为字符串或 null；不合法行使整批拒绝，而不是静默跳过。
- ledger 的源 ID 与五字段 fingerprint 一致时跳过，即使目标正文已编辑或已软删，也不覆盖、不清 deleted_at、不复活。软删后 ledger 的 `diary_id` 仍指向原行；`ON DELETE SET NULL` 仅兼容历史物理删除，历史已删项也跳过。保留 ledger 才能防止重导复活。
- 同源 ID 的内容/日期/timestamp 发生变化，或新源 ID 与目标已有日记冲突时，整批拒绝。所有未入 ledger 的源 ID 还必须严格大于目标日记 `sqlite_sequence` 的导入前 high watermark，否则同样报 `target id conflict`；这防止复用已经硬删除的本地 ID，即使目标表已空也不例外。已入 ledger 的匹配项不受该追加条件影响，仍跳过。
- 只有未见过且通过全部检查的源 ID 可以追加导入；这不是任意多源合并或同步工具，不传播源库删除。ID high watermark 与业务日期无关，不禁止删末篇后重用日期。
- 任何写入失败会回滚该事务；不要手工改 fingerprint、清空 ledger、重置 sqlite_sequence 或删除目标行来绕过冲突。

脚本成功时只输出聚合 JSON：

| 字段 | 含义 |
|---|---|
| `total` | 当前源行数 |
| `planned` | 本批尚未导入且无冲突的行数 |
| `imported` | 本次实际提交行数，dry-run 恒为 0 |
| `skipped` | ledger 匹配而跳过的行数 |
| `duplicate_dates` | 每日期第一篇以外的行数之和，不是重复日期种类数 |
| `blank_contents` | Python `strip()` 后为空的源正文数 |

每次迁移都以该源快照和目标库的实际 dry-run 输出为准，不使用文档中的固定源统计作为验收常量。报告仅保留必要聚合，不记录真实日记日期或正文。

### 核验与恢复服务

在仍停服的情况下，核对退出状态及聚合计数、source 未被修改、目标收藏仍保留。首次 apply 的 `imported` 应与审核过的 dry-run `planned` 对应；对同一源快照再次 dry-run 应显示 `planned=0`、`imported=0`、`skipped=total`。校验失败先停止推进，不把原样重试当修复。

保真检查应在受控本地比较五字段、ID、null/旧字符串和 ledger，不把比对正文输出到日志或报告。使用备份副本或合成库演练编辑/删除后的重导、版本竞争和回滚，不能为了验收破坏真实日记。

确认目标路径、环境模式、密码/TOTP 与生产 origin 配置和实际访问地址后启动服务并重新开放写入。浏览器验收两步登录、读取、筛选、编辑冲突、退出及桌面/手机页面；鉴权验收匿名拒绝、开发无 Origin 比对/生产精确 Origin、cookie/key 不可互换、两模式 CSRF、no-store、收藏公开读不回归。另验无 If-Match 可写、有版本仍防冲突。真实数据环境只执行获授权操作，CRUD 演练优先使用隔离测试实例。

## 回滚与凭据轮换

迁移失败时若事务未提交，目标日记和 ledger 不会部分提交，但此前 `init-db` 是单独完成的 schema 升级；需要恢复升级前状态时仍须还原备份。

1. 停止相关服务和写入方，保留失败现场供受控排查，确认选择 source/target 对应的迁移前一致备份。
2. 按备份工具的恢复流程还原完整目标库，不用批量 DELETE、清空 ledger 或手改 schema 版本模拟回滚。源库正常情况下没有改动；仅在确有需要时还原对应源备份。
3. 还原时处理现有数据库及其 WAL/SHM 的一致性，不能将旧备份 `.db` 与本次运行残留的 WAL 混用；所有进程保持停止，按 SQLite 恢复流程处理，不直接对活库覆盖。
4. 恢复与备份匹配的程序和配置后再验收。整库还原也会回退备份后新增的收藏、日记及 session，提前明确数据损失窗口，不承诺无损自动回滚。

需要撤销浏览器旧 session 时生成新 hash 或 TOTP secret 并更新配置、重启；用户名变化也踢旧 session，origin 变化不踢。丢失验证器没有恢复码/自助重置入口，由有部署权限的运维运行 totp-secret、安全更换配置并在本地重新登记。恢复旧库/旧凭据可能让旧 session 再次有效，安全事件后不要恢复泄露配置。单独轮换 API Key 不替代浏览器凭据轮换；key 轮换须同步所有授权 Agent。

## 验证入口

从当前仓库根目录执行，不使用旧 `/root/codes/memento` 路径。按 `.github/workflows/ci.yml` 顺序；本轮结果见末尾摘要，后续代码变化须重新验证：

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all
cargo build --release
```

release 构建后运行 Python 标准库迁移测试；以下严格入口将 ResourceWarning 视为错误，与本轮验证口径一致：

```bash
python3 -B -W error::ResourceWarning -m unittest discover -s tests -p test_import_diaries.py -v
```

聚焦入口为 `cargo test --test diary_api`、`cargo test --test browser_auth`、`cargo test --test migrations`、`cargo test --test cli` 和 `cargo test --lib browser::tests`。Rust HTTP 测试不是实际浏览器/反代验证，Python 合成数据库测试也不是源库迁移成功的证据。

可选真实浏览器回归需要本机已安装 `agent-browser` CLI 和浏览器（不是应用的 npm 依赖），先构建 release，再执行：

```bash
python3 -B tests/browser_smoke.py
```

脚本自启回环测试服务器，使用临时数据库、合成凭据和独立浏览器会话，从不读取本地 `.env` 或真实日记；结束关闭服务器和浏览器并移除临时库。回归目标包括 release 内嵌、登录、纯文本显示、冲突保留草稿后编辑、软删除、窄屏和列表挂起时退出。删除检查保留列表消失和详情 404 断言，捕获确认文案检查不含“永久”、说明不再显示/正文保留/本期无恢复；仅以 mode=ro、query_only 查询脚本自身临时 fixture，断言原行和正文/日期保留、deleted_at 已设置、version 加一及 updated_at 同值，不输出正文。

### 可选反代回归

先构建 release，准备带 HTTP proxy/limit_req/SSL 模块的 Nginx 可执行文件和 openssl（仅用于生成临时自签名证书），再执行：

```bash
python3 -B tests/nginx_smoke.py /path/to/nginx
```

省略参数时使用 PATH 中的 nginx。脚本使用临时 prefix、配置副本、合成凭据/证书、临时生产环境文件和数据库，自启回环应用及代理，检查两份片段的 nginx -t 和实际 HTTP/HTTPS 请求，结束清理。不读取或 reload 系统 Nginx 配置，不安装服务；测试生产环境文件是脚本自建 fixture，不是仓库环境模板的回读验证。

### 可选覆盖率

以下保留历史验证使用的工具版本与可复现入口，按需安装开发工具，不是项目运行依赖或强制 CI 步骤；本次文档任务不执行安装。使用与项目构建相同的 Rust toolchain：

```bash
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov --version 0.9.1 --locked
cargo llvm-cov --all-targets --show-missing-lines --quiet
```

默认 LLVM 源覆盖报告包含 `src/` 模块内单元测试代码，不能解释成纯业务代码覆盖或分支覆盖；未使用 `--branch`。不设由历史数字推导的强制阈值，覆盖率也不是 100% 安全证明。后续源码、测试、工具链变化可能改变结果。

## 本轮验证

以下为当前双环境、bcrypt/TOTP、可选版本条件实现的本地验证结果（Rust 1.98.1、Python 3.13.5、Linux）。后续修改须重新验证，不把测试数量作为固定验收常量。

| 检查 | 结果与范围 |
|---|---|
| Rust 格式/静态检查/构建 | cargo fmt --all -- --check、严格 clippy、cargo build --release 通过 |
| Rust 全量测试 | 242 个通过：110 lib + 30 api + 42 browser_auth + 7 cli + 38 diary_api + 15 migrations = 242 |
| Python 导入测试 | 32 个通过，启用 -W error::ResourceWarning；使用合成数据库 |
| 浏览器 smoke | development bcrypt + 两步 OTP（第一阶段未登录断言）、XSS 纯文本、版本冲突编辑、软删存储保留、390px 视口、挂起列表时退出通过 |
| Nginx smoke | Nginx 1.26.3，两份片段 nginx -t 与真实隔离 HTTP/HTTPS 反代通过；生产环境文件加载、Origin 403、密码/OTP 两步、HTTP Strict 无 Secure / TLS Secure、TTL 2 天、API Key 透传、可选 If-Match、64k/8k body 413、limit_req 429 通过 |
| 限流范围 | 密码/验证码 POST 达到限额后，GET 会话查询和 DELETE 退出仍能到达应用，没有被同一额度阻挡 |
| Rust 行覆盖率 | cargo-llvm-cov 0.9.1：整体 92.69%；browser.rs 96.23%、diary.rs 97.25%、日记 handler 99.34%。包含模块内测试代码，非分支覆盖或安全证明 |
| 真实旧库演练 | 在临时 v3 目标导入 3069 篇，五个原字段逐项一致；重复执行全部跳过，软删后重导不复活，完整性/外键检查通过。临时目标已删除，源库 size/mtime 未变 |
| 人工验收启动 | 当前任务 release 已以 development 监听 0.0.0.0:23459；通过机器内网地址验证页面、密码和 TOTP 登录，保留既有验收日记。仅独立验收实例，不是生产部署 |

验证环境未安装系统 Nginx，使用在 /tmp 下载解包的 Nginx 1.26.3、临时 prefix 和合成证书，没有安装或操作系统服务。仓库新环境模板未被本轮回读验证；上述环境加载结果只来自测试自建 fixture。

未验证项仍包括真实域名/证书部署、多平台运行、TTY 交互式密码输入、真实环境恢复及部分公网图片下载路径。两份仓库环境模板的回读被工具权限策略拦截，未绕过；环境文件加载及 Nginx 测试使用的是受控临时 fixture。真实 app.db 未变，未进行生产迁移或部署。验收地址与凭据交付位置以本次交付说明为准，不作为项目固定配置。
