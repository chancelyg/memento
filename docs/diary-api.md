# 日记与浏览器 Session 接口

本文记录双环境、bcrypt/TOTP 与可选 If-Match 的接口契约，对应实现位于 `src/lib.rs`、`src/handlers/diary.rs`、`src/diary.rs`、`src/browser.rs`。[设计](diary-design.md) 说明 schema v3 防重用决策，[管理后台设计](admin-settings-design.md) 说明站点设置契约，[运维](diary-operations.md) 说明配置、迁移和验证范围。所有示例正文、日期和 ID 均为虚构。

## 两组日记路由

下表中的 `PREFIX` 是 `/api/diaries`（Agent）或 `/private/diaries`（同源浏览器）。两组复用相同 handler 和业务，只是鉴权不同。

| 方法与路径 | 请求 | 成功响应 |
|---|---|---|
| `GET PREFIX` | 查询参数见下表 | 200，`data` 为分页对象 |
| `POST PREFIX` | JSON `{"content":"正文"}` | 201，`data` 为 DiaryDto，带 ETag |
| `GET PREFIX/{id}` | ID 为可解析的 i64 | 200，`data` 为 DiaryDto，带 ETag |
| `PATCH PREFIX/{id}` | JSON `{"content":"新正文"}`；可选 If-Match | 200，更新后的 DiaryDto，带新 ETag |
| `DELETE PREFIX/{id}` | 可选 If-Match，无需正文 | 204，空 body；软删除 |

- `/api/diaries` 的读取和写入都要求 `X-API-Key`。不接受 cookie 替代，不要求浏览器 Origin/CSRF，不提供跨域 CORS 授权；适用于服务器端 Agent。
- `/private/diaries` 的读取和写入都要求有效 `memento_session` cookie。POST/PATCH/DELETE 两模式都必须有该 session 的 `X-CSRF-Token`，production 另需精确 Origin；development 不比对 Origin/Host。API Key 不能替代 session 或跳过 CSRF。
- 本期只有一个 key，兼具全部日记读写和收藏写权限；现有收藏 Agent 持有的 key 因而也能读取私有日记。不要向不应读日记的 Agent 分发此 key。
- 服务启动未设置或仅含空白的 `MEMENTO_API_KEY` 时，外部 key 鉴权接口全部禁用，请求返回 401；不会输出临时密钥。浏览器日记可凭独立登录配置继续使用，不能作为 Agent 绕过 key 的入口；收藏公开读取不受影响。
- 日记没有 PUT、批量写入、按日期 upsert、stats、goal、audit 或独立 JSON 导入导出路由。GET 路由的 HEAD 使用框架的无响应正文行为，业务客户端使用上述显式方法即可。

## 响应结构

JSON 成功响应为 `{"success":true,"data":...,"error":null}`；错误为 `{"success":false,"data":null,"error":"简短错误"}`。DELETE 成功不要解析 JSON；HEAD 无正文。已注册日记/session 路由的 405 经统一错误信封处理，不应将此保证外推到未知 URL、静态响应或代理错误页。

DiaryDto 示例：

```json
{
  "id": 42,
  "content": "示例正文",
  "create_date": "2030-01-02",
  "created_at": null,
  "updated_at": "legacy timestamp",
  "version": 1
}
```

`created_at` 和 `updated_at` 为字符串或 null，旧字符串不保证时间格式。新建使用 UTC RFC3339 毫秒时间，编辑更新 `updated_at`，不改变 `created_at`。`create_date` 为业务日记日期，不能由客户端指定或修改。列表 `items` 也包含完整正文和版本，并非摘要 DTO。

DTO 保持上述六字段，不暴露内部 `deleted_at`。列表和详情均只读取 `deleted_at IS NULL` 的未删除记录，不提供读取已删除正文的接口。

分页对象示例：

```json
{"items": [], "total": 0, "page": 1, "per_page": 24}
```

## 列表查询

| 参数 | 默认 | 规则 |
|---|---|---|
| `page` | 1 | 正整数；0、无法解析或 offset 超出 SQLite i64 范围返回 400 |
| `per_page` | 24 | 正整数；大于 100 截为 100，0 返回 400；数值须能解析为 u64 |
| `q` | 无过滤 | 正文字面子串搜索，最多 10000 个 Unicode 标量值；空串不筛选，不 trim，空白本身参与搜索 |
| `start_date` | 无下界 | 合法且严格 `YYYY-MM-DD`，年份 0001..9999，包含边界 |
| `end_date` | 无上界 | 同上，包含边界；小于 start_date 返回 400 |
| `sort` | `desc` | 只接受 `asc` 或 `desc`，大小写不自动归一化 |

按 `create_date`、再按 `id` 同方向排序。始终限定 `deleted_at IS NULL`，再组合 `q`、日期过滤并计数和分页；`total` 仅是过滤后未删除篇数，已删除记录不占分页位置。超出末页返回空 `items`，不改变总数。`q` 的 `%`、`_`、反斜线会转义后参数绑定，不作为 LIKE 通配符；大小写行为遵循 SQLite LIKE，不承诺完整 Unicode 大小写折叠。客户端应对参数做 URL 编码，禁止把真实搜索 URL 写入日志。

## 创建与编辑

POST/PATCH 要求 JSON 对象且只有必填字符串 `content`，不能提供 `id`、`create_date`、timestamp、`version`、`deleted_at` 或 `extra`，也不能通过这些方法恢复已删除记录。正文服务端 trim 后须为 1..10000 个 Unicode 标量值；整份 JSON 请求体最多 65536 字节（64 KiB），是与正文字符数独立的限制。缺失/错误媒体类型返回 415，错误 JSON、未知字段、空白正文或超长正文返回 400，body 超限返回 413。

未删除集合为空时，新篇日期是业务时区今天（默认 `Asia/Shanghai`，可用 `MEMENTO_DIARY_TIMEZONE` 配置）；否则严格取未删除集合的最大日期加一天，不跟随现实今天。删除最大日期的末篇后可能重用日期，删除中间条目不补洞，全部软删后重新使用业务今天，但物理表仍有记录。新篇版本为 1；编辑只换正文，每次成功都将版本加一。

历史空白、重复日期、超过新长度限制的正文和旧 timestamp 允许通过离线迁移保留；HTTP 新建/编辑不开放绕过规则的模式。

## 软删除

DELETE 保留原行、正文、`create_date` 和 `created_at`，将 `deleted_at` 设为当前 UTC RFC3339 毫秒时间，`updated_at` 设为同一个值，`version` 加一。记录随后不再出现在列表、搜索、日期过滤、计数、分页或详情中，但数据库仍保留正文。这不是安全擦除，本期无恢复、回收站或永久清除 API；收藏 DELETE 仍为硬删除。

## 并发条件

GET 详情、POST 成功、PATCH 成功返回 `ETag: "<version>"`。列表不带集合 ETag，可读取条目 `version`，或先 GET 详情获取 ETag。PATCH 和 DELETE 可选发送一个带双引号的正整数版本，例如 `If-Match: "1"`。网页仍发送版本防冲突，没有自动合并。

- 缺少 If-Match：允许对最新未删除记录操作，不返回缺头 428；调用方主动放弃版本冲突保护。
- 未删除记录版本已变化：412，且不修改数据；重新读取并比较最新正文，让用户确认合并或删除意图后再提交，不能仅换新版本自动覆盖。
- 非法 If-Match：400。裸数字、弱 ETag `W/"1"`、`*`、逗号列表、多个同名头、前导零、零、负数及 i64 溢出都不接受。
- 不存在或已软删除的记录：详情返回 404，PATCH 和重复 DELETE 在前置校验通过后返回 404，而不是 412。非法条件或 body 可先于数据库查询报错，不保证任何请求都优先返回 404；已删 ID 的非法 If-Match 仍为 400，省略则无需该项校验。
- 同版本并发 DELETE：只有一个请求成功 204，另一个在前置校验通过后返回 404，不再次递增版本。
- 日期或版本无法继续递增：409；不要自动重试。未删除记录在版本溢出时 DELETE 返回 409，不设置删除标记或改动原行。未删除集合的最大日期非法则是脱敏 500。

ETag 用于乐观并发，不是缓存许可；这些路由使用 `Cache-Control: no-store`，当前没有 `If-None-Match`/304 读取协商。

## Agent 示例

以下仅示例语法。`BASE` 是实际 HTTP(S) origin，内网可用 `http://<机器内网IP>:23457`，公网/不可信网络推荐 HTTPS；key 已由安全环境注入。示例会创建或更改数据，不是健康检查；不要用真实正文作为 shell 命令参数或开启 curl verbose/trace。正式客户端应在内存中编码 JSON，并关闭请求/响应内容日志。

```bash
curl -i "$BASE/api/diaries?page=1&per_page=24&sort=desc" \
  -H "X-API-Key: $MEMENTO_API_KEY"

curl -i -X POST "$BASE/api/diaries" \
  -H "X-API-Key: $MEMENTO_API_KEY" \
  -H 'Content-Type: application/json' \
  --data '{"content":"仅作演示的新正文"}'
```

使用刚读取的实际 ID 和版本；以下 `42`、`"1"` 只是占位示例：

```bash
curl -i -X PATCH "$BASE/api/diaries/42" \
  -H "X-API-Key: $MEMENTO_API_KEY" \
  -H 'If-Match: "1"' \
  -H 'Content-Type: application/json' \
  --data '{"content":"仅作演示的修改正文"}'
```

POST 无幂等键，也不按正文去重。超时不能证明未写入；先用鉴权列表/详情核对，不要盲目重发导致新增下一日期的重复内容。PATCH/DELETE 的结果不明时也先重新读取。

## 浏览器 Session

`/session` 仅供本应用浏览器使用，不是 Agent 登录入口。浏览器自动同源携带 cookie 和写请求 Origin，脚本从响应保存 CSRF token 到当前页面内存；不需要也不应把 API Key 写入页面。

这是严格单用户协议：唯一浏览器账号就是管理员，配置用户名只会重命名该账号。多个有效 session 仅表示同一管理员在多台设备或多个浏览器登录，不创建用户、角色或 RBAC。

两模式都必须配置 bcrypt `MEMENTO_PASSWORD_HASH`、Base32 `MEMENTO_TOTP_SECRET`（解码至少 20 字节），用户名默认 admin。CLI hash-password（含 --stdin）采用 DEFAULT_COST=12，密码非空且至多 72 UTF-8 字节，防截断、不另设至少 12 字符规则；旧 Argon2 hash 须重生，不改业务数据。TOTP 使用 6 位数字字符串（保留前导零）、SHA1、30 秒、前后各 1 时间步容差；用 totp-secret 本地生成新 20 字节 secret 并安全登记验证器，不依赖域名。

development 不要求 Origin/Host 比对。production 必须显式 canonical HTTP(S) PUBLIC_ORIGIN，POST 两阶段和 DELETE 都要求精确单一 Origin，不从转发头猜测。两模式登录后的 unsafe 请求均保留 CSRF，由前端自动处理。生产 HTTP 内网反代可用，HTTPS 是公网推荐方案而非所有生产的硬约束。

| 方法 | 条件与请求体 | 成功响应 |
|---|---|---|
| `POST /session` 第一步 | JSON `{"username":"admin","password":"用户输入"}`；生产需精确 Origin；body 最多 8192 字节 | 200，`data: {requires_totp:true, challenge}`，无 Set-Cookie |
| `POST /session` 第二步 | JSON `{"challenge":"上一步返回值","code":"六位验证码"}`；生产需精确 Origin；body 最多 8192 字节 | 200，`data: {username, csrf_token}`，Set-Cookie |
| `GET /session` | 有效 cookie；不要求 Origin/CSRF | 200，同上 `data`，不返回 session token，不续期 |
| `DELETE /session` | 有效 cookie、该 session 的 X-CSRF-Token；生产另需精确 Origin | 204 空 body，撤销数据库记录并设置 Max-Age=0 |

每次登录必须完成两步；密码正确不是已登录。challenge 有效 5 分钟、最多 5 次错误 OTP、最多 64 个并行待验证挑战，失效后重新从密码开始；上限仅针对短期 challenge，不限制长期 session 数量。不记录 challenge、密码、secret、code 或 Cookie。登录前没有 session CSRF token，生产 POST 用 Origin 防护；登录后两模式都需 CSRF。重复 cookie 中出现多个 memento_session 会被拒绝。

Cookie 两模式都有 HttpOnly、Path=/、无 Domain；development 是 SameSite=Lax、无 Secure，production 是 SameSite=Strict，https origin 才 Secure。TTL 由正整数 `MEMENTO_SESSION_TTL_DAYS` 配置、默认 7 天，Max-Age 默认 604800 秒，不滑动续期，不设 32 个有效 session 上限。重启不自动撤销；用户名/hash/TOTP 变动使旧 session 失效，origin 变动不踢旧会话。退出只撤销当前会话，已无效返回 401，并非幂等 204。无恢复码/自助账号管理，丢设备须由运维更新 secret 并重新配置验证器。

TTL 配置只影响之后新建的会话，不重写已有会话的 `expires_at`；已有会话仍按创建时的期限到期。

schema v3 的 `browser_totp_state` 按 credential_hash 保存 last_used_step。OTP 成功时只允许比已使用值更大的时间步，和 session 插入在同一 IMMEDIATE 事务提交，失败一起回滚。前后各 1 步仍是时钟容差，**不是重复使用许可**：同一时间步的验证码不能跨 challenge、重启或注销重用，返回 401 时应等下一个验证码并重新从密码开始登录。若先接受了超前一步，须等比它更大的时间步；时钟回拨可能暂时拒绝。没有手动全局绕过开关，此表不是审计/账户平台。

## 管理员站点设置

`/admin` 是公开可加载但不含设置值的页面壳；脚本先检查 `/session`，无有效会话时跳到 `/login?next=%2Fadmin`。登录页只接受 `/diary` 或 `/admin` 作为 next，默认进入 `/admin`。`/`、`/diary`、`/login`、`/admin` 的账户入口也会通过 `/session` 在“登录”和“管理”之间切换。

| 方法 | 条件与请求体 | 成功响应 |
|---|---|---|
| `GET /private/settings/site` | 有效管理员 cookie；不要求 CSRF | 200，`data: {name,slogan,icon}` |
| `PUT /private/settings/site` | 有效 cookie 和该 session 的 `X-CSRF-Token`；production 另需精确 Origin；JSON body 最多 512 KiB | 200，返回 trim 后的完整 `{name,slogan,icon}` |

API Key 不能替代 session。PUT 是完整站点设置替换，不是 PATCH：后台提交 `name`、`slogan`、`icon` 三字段，拒绝未知字段。`name` trim 后为 1..80 个 Unicode 标量值；`slogan` trim 后最多 200 个，可为空；`icon` 允许内置默认值、受限 HTTPS URL 或严格 base64 栅格图片 data URI，完整约束见[管理后台设计](admin-settings-design.md)。错误媒体类型返回 415，body 超限返回 413，JSON/字段/值错误返回 400；私有设置响应均为 `no-store`。写入响应未知时先 GET 核实，不要盲目重发。

首页不再提供可见的收藏名称搜索控件，但公开 `GET /api/favorites` 的 `q` 参数仍保留；这不是 API 删除。日记页的正文/日期筛选也不受影响。

## 状态码速查

| 状态码 | 含义 |
|---|---|
| 200 / 201 / 204 | 查询或编辑成功 / 创建成功 / 删除或退出成功 |
| 400 | 查询、路径、JSON、正文、站点字段或 If-Match 无效 |
| 401 | key 错误/缺失；session 缺失、过期、撤销或配置失配；登录凭据错误、challenge 无效或 OTP 时间步已使用（等待下一个验证码） |
| 403 | 浏览器 Origin 或 CSRF 不符合要求 |
| 404 | 日记不存在或已软删除（前置校验通过后） |
| 405 | 当前路径不支持该方法（鉴权等中间件可能先拒绝） |
| 409 | 日期或版本达到支持范围上限 |
| 412 | 提供的 If-Match 版本过时；省略该头不会返回 428 |
| 413 / 415 | body 超限 / 需要 JSON 媒体类型 |
| 429 | 生产示例 Nginx 的 `/session` 限流拒绝，代理响应不保证 JSON 信封 |
| 500 | 内部故障，客户端只收到脱敏信息 |
| 503 | 暂时无法受理登录（如待验证 challenge 容量已满），不是长期 session 数量上限 |

应用不再提供 IP 分桶或全局 hash 限流。生产示例由 Nginx 对 `/session` 两步入口共同限流（10r/m、burst 10，可调），客户端不得盲目重试 POST。短期 challenge 的容量/失败次数控制不是长期会话限流。验证范围及结果归属见[运维文档](diary-operations.md#本轮验证)。
