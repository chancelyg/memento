# memento

电影 / 游戏 / 图书三合一的个人海报墙，另提供浏览器登录和私有日记。Rust 单文件二进制交付 release 前端，数据保存在外部 SQLite；收藏读取公开，日记读写均需鉴权。

数据存在本地 SQLite 文件里（含海报图片本身），不打包进可执行文件。release 前端静态资源内嵌进二进制，部署还需配置和可持久化的数据目录。新增、修改收藏都通过 HTTP API 完成，方便由脚本、Telegram 机器人或 agent 提交。

特性：

- 三种类型（电影、游戏、图书）共用一面墙，支持按类型过滤、按名称搜索、分页。
- 单文件二进制：SQLite 经 `rusqlite` bundled feature 编译，无系统依赖；前端经 `rust-embed` 内嵌。
- 收藏读接口公开，收藏写接口（POST / PUT / DELETE / 上传图片）需 `X-API-Key`；日记 API 的读写都需要 key。
- 海报图片支持三种入库方式：base64、由服务端抓取的 URL、直接上传字节。
- 暗色前端，含详情弹窗、懒加载、明暗主题切换。
- `/diary` 提供同源浏览器登录和日记创建、筛选、编辑、软删除；浏览器使用 session cookie，不使用 API Key。删除后不再显示，但正文仍保留在数据库中，本期无恢复、回收站或永久清除 API；收藏保持硬删除，二者均不承诺安全擦除。

### 日记文档

- [设计说明](docs/diary-design.md)：日期编排、权限隔离、session、安全边界与迁移决策。
- [接口文档](docs/diary-api.md)：Agent `/api/diaries`、浏览器 `/private/diaries` 和 `/session`，分页、ETag、If-Match 及错误处理。
- [部署与迁移](docs/diary-operations.md)：双环境配置、bcrypt/TOTP 设置、HTTP 内网反代与可选 HTTPS、旧库 dry-run/apply、一致备份与回滚和验证入口；不代表生产已迁移或上线。

没有未删除日记时，新篇取业务时区今天（默认 `Asia/Shanghai`），否则按未删除日记的最大日期加一天；删除末篇后可能重用日期，这是预期行为。全部软删不代表物理空表。编辑仅允许正文，PATCH/DELETE 的 `If-Match` 可选：不传操作最新记录，提供时格式错误返回 400、未删除记录版本过时返回 412；不再因缺头返回 428。网页仍带版本防冲突，不自动合并。已删除 ID 在前置校验通过后返回 404。当前单个 API Key 同时拥有收藏写权限和全部日记读写权限，不能细分授权。

## 运行

从 [Releases](../../releases) 下载对应平台的二进制，配置认证与运行环境后启动。release 无需 Rust 运行时，数据和凭据仍需外部持久化。

```bash
# 下载并重命名（示例，按实际 release 资产名调整）
chmod +x memento

./memento hash-password
./memento totp-secret
# 在受保护的 .env.production 中填写生成的 hash、secret 和外部 origin 后启动
MEMENTO_ENV=production ./memento
```

无参数 server 启动先由系统 `MEMENTO_ENV` 选择模式，只接受 `development` 或 `production`，未设置默认 `production`。仅从进程 cwd 加载选定的 `.env.development` 或 `.env.production`，不加载通用 `.env`、不向父目录查找；已有系统环境优先，文件内不能切换模式。CLI `hash-password`、`totp-secret`、`init-db` 不加载任何 dotenv 文件。

未设置或仅含空白的 `MEMENTO_API_KEY` 会禁用全部外部 key 鉴权接口（收藏写入、日记 API、`/api/auth/verify`），启动日志只提示禁用，不输出密钥；收藏公开读取不受影响，浏览器日记可独立使用。需要 Agent/API 访问时显式设置固定强 key。

### 最短内网登录

开发直连适合可信本机/内网测试，不需要域名或证书。在源码目录执行本地凭据工具，参照 `.env.development.example` 在 cwd 配置 `.env.development`：

```bash
cargo run -- hash-password
cargo run -- totp-secret
# 安全填写 hash/secret，并在本地验证器登记 secret 后
MEMENTO_ENV=development cargo run
```

开发示例监听 `0.0.0.0:23457`、数据库 `./memento.dev.db`，访问 `http://<机器内网IP>:23457/diary`；`0.0.0.0` 不是访问 URL。默认账号 `admin`，每次登录均先校验密码，再输入 TOTP。hash 用单引号包围，避免 `$` 插值；不把密码或 TOTP secret 写入命令参数、日志或 Git。二进制开发启动对应 `MEMENTO_ENV=development ./memento`。

开发模式不要求 Origin/Host 比对，Cookie 为 HttpOnly、SameSite=Lax、无 Secure。生产模式必须显式配置 canonical HTTP(S) `MEMENTO_PUBLIC_ORIGIN`，浏览器 unsafe 请求精确校验 Origin，Cookie 为 HttpOnly、SameSite=Strict；HTTPS origin 才带 Secure。两模式都要求密码 + TOTP，登录后 unsafe 请求保留 session 绑定 CSRF，由前端自动处理，无用户配置项。

生产示例 `.env.production.example` 监听 `127.0.0.1:23457`、数据库 `./memento.db`，通过 [HTTP 内网 Nginx 示例](deploy/nginx/memento.conf) 的 8080 访问，origin 填 `http://机器IP:8080`，无需域名/证书。公网或不可信网络推荐 [可选 HTTPS 示例](deploy/nginx/memento-https.conf)，但应用不强制所有 production 使用 HTTPS。配置文件放置上下文与前置条件见运维文档；不会自动部署。

### 配置

通过系统环境或所选环境文件配置，系统值优先。使用 `.env.development.example` / `.env.production.example`，不要使用旧通用 `.env.example` 作为当前启动契约。样例 hash/secret 为空，必须自行生成填写，没有可用默认弱凭据。

| 变量 | 默认值 | 说明 |
|---|---|---|
| `MEMENTO_ENV` | `production` | 系统环境选择 `development` / `production`；dotenv 不可切换模式。 |
| `MEMENTO_API_KEY` | 未设置/空白则禁用 key API | 收藏写入及日记全部读写共用的 key；需要外部 API 时显式设置，浏览器可独立使用。 |
| `MEMENTO_DB_PATH` | 按环境 | 开发默认 `./memento.dev.db`，生产默认 `./memento.db`，相对进程 cwd。 |
| `MEMENTO_BIND` | 按环境 | 开发默认 `0.0.0.0:23457`，生产默认 `127.0.0.1:23457`，不仅是样例值。 |
| `RUST_LOG` | `info` | tracing 日志过滤器，例如 `memento=debug,tower_http=debug`。 |
| `MEMENTO_SITE_NAME` | `memento` | 站点名称，用于页面标题与左上角品牌名。 |
| `MEMENTO_SLOGAN` | `所有的美好都值得被珍藏与分享。` | 首页副标题（slogan）。 |
| `MEMENTO_ICON` | 内置 🗂️ emoji SVG | favicon，可填 URL 或 data URI。 |
| `MEMENTO_LOGIN_USERNAME` | `admin` | 单个浏览器账号。 |
| `MEMENTO_PASSWORD_HASH` | 无，必填 | bcrypt hash；旧 Argon2 hash 不兼容，须重新生成，不改收藏/日记数据。 |
| `MEMENTO_TOTP_SECRET` | 无，必填 | Base32，解码至少 20 字节；两模式均必需。 |
| `MEMENTO_SESSION_TTL_DAYS` | `7` | 正整数天，不滑动续期；不设 32 个有效 session 上限。 |
| `MEMENTO_PUBLIC_ORIGIN` | 生产必填 | 精确 canonical HTTP(S) origin，不含路径、query、fragment 或用户信息；开发不做 Origin/Host 比对。 |
| `MEMENTO_DIARY_TIMEZONE` | `Asia/Shanghai` | 没有未删除日记时新篇使用的业务时区；不改变收藏日期规则。 |

本地命令：`memento hash-password`（终端无回显、二次确认）、`memento hash-password --stdin`（测试/集成）、`memento totp-secret`、`memento init-db <path>`。hash 使用 bcrypt `DEFAULT_COST=12`，密码非空且不超过 72 UTF-8 字节，防算法截断，不另设至少 12 字符规则。`totp-secret` 生成新 20 字节 Base32 secret，仅显示到用户终端；本地安全配置验证器为 6 位、SHA1、30 秒，容许前后各 1 时间步，不依赖域名。完整操作见[运维文档](docs/diary-operations.md)。

密码成功仅返回 5 分钟 challenge，不设置 Cookie；同一 `/session` 再提交 challenge/code 才创建 session。最多 5 次错误 OTP、64 个并行待验证 challenge，仅限制短期挑战而非长期会话。用户名/hash/TOTP 变动使旧 session 失效，origin 变动不踢旧 session。没有恢复码或自助账号管理，丢失设备须由运维更换 secret 并重新配置验证器。

TOTP 成功使用的时间步持久化，只有更大的时间步才能再次登录；同一验证码不能跨 challenge、重启或注销重复使用。窗口内因此返回 401 时，请等下一个验证码并重新开始登录；若已接受超前一步，须等更大的时间步，时钟回拨也可能暂时拒绝。没有手动全局绕过开关。

已有数据库在启用 WAL 前经过只读 schema 预检，拒绝外来日记库、未来版本和残缺 schema；初始化时在事务内再次验证。当前 schema v3：v2 添加可空 `deleted_at TEXT`，v3 添加 `browser_totp_state(credential_hash TEXT PRIMARY KEY, last_used_step INTEGER NOT NULL)`。支持空库/既有收藏库及 v1/v2 升级，迁移历史为 1、2、3，保留日记、会话、收藏、导入 ledger 和 ID 序列。TOTP 时间步更新与 session 插入在同一个 IMMEDIATE 事务中提交，失败一起回滚；这是防重用状态，不是审计或账号平台。导入器要求完整 v3，只检查新表元数据、不读其中真实状态。Unix 新建数据库使用 0600 权限，旧文件权限须由运维检查。

```bash
# 已完成 .env.production 的认证、origin 与持久化配置
MEMENTO_ENV=production ./memento
```

## API

以下主要描述收藏 API；日记与浏览器 session 见[独立接口文档](docs/diary-api.md)。应用 JSON 响应使用统一信封（204、图片、静态资源及部分提取器拒绝不适用）：

```json
{ "success": true, "data": { }, "error": null }
```

出错时 `success` 为 `false`、`data` 为 `null`、`error` 为提示信息（详细错误只记录在服务端日志）。

`FavoriteDto`（list / get / create / update 都返回这个结构，不含图片字节）：

```json
{
  "id": 151,
  "type": "game",
  "name": "哈迪斯2 Hades II",
  "url": "https://www.douban.com/game/36185144/",
  "aka": ["黑帝斯2"],
  "genres": ["动作", "角色扮演"],
  "release_date": "2025-09-25",
  "rating": null,
  "summary": null,
  "extra": { "developer": "Supergiant Games", "platforms": ["PC", "PS5"] },
  "has_image": true,
  "image_url": "/api/favorites/151/image",
  "sort_date": "2025-09-25",
  "created_at": "2026-03-12T01:25:56Z",
  "updated_at": "2026-04-15T15:13:45Z"
}
```

### 公开端点

无需鉴权，任意来源可读。

| 方法与路径 | 说明 |
|---|---|
| `GET /` | 内嵌的 `index.html` |
| `GET /static/{file}` | 内嵌静态资源 |
| `GET /api/health` | 健康检查 |
| `GET /api/favorites` | 列表，按 `sort_date DESC, id DESC` 排序，`data` 为 `{ items, page, per_page, total }` |
| `GET /api/favorites/{id}` | 单条，不存在返回 404 |
| `GET /api/favorites/{id}/image` | 海报原始字节，无图返回 404 |

`GET /api/favorites` 的查询参数：

| 参数 | 类型 | 取值 / 约束 | 默认 |
|---|---|---|---|
| `type` | string | `all` / `game` / `movie` / `book`（也接受 `全部` `游戏` `电影` `图书`） | `all` |
| `page` | int | ≥ 1 | `1` |
| `per_page` | int | 1..100 | `24` |
| `q` | string | 对 `name` 的大小写不敏感子串匹配，最长 256 字符 | 无 |

### 鉴权端点

均需请求头 `X-API-Key: <key>`，缺失或错误返回 401。

| 方法与路径 | 说明 | 成功响应 |
|---|---|---|
| `GET /api/auth/verify` | 校验 `X-API-Key` 是否正确 | `200` + `{ "valid": true }` |
| `POST /api/favorites` | 创建一条收藏 | `201` + `FavoriteDto` |
| `PUT /api/favorites/{id}` | 按 `id` 部分更新（不是按 `name`） | `200` + `FavoriteDto` |
| `DELETE /api/favorites/{id}` | 删除 | `204` 空 body |
| `POST /api/favorites/{id}/image` | 替换海报 | `200` + `FavoriteDto` |

`GET /api/auth/verify` 走同一鉴权中间件：key 正确返回 `200`（`data.valid = true`），错误/缺失返回 `401`——客户端（机器人 / agent）可用它在提交前先校验 key：

```bash
curl -s -o /dev/null -w "%{http_code}\n" \
  -H "X-API-Key: $MEMENTO_API_KEY" \
  http://localhost:23457/api/auth/verify   # 200 = 正确，401 = 错误
```

`POST` / `PUT` 的请求体为 JSON（`Content-Type: application/json`）。下表为全部可接受字段；列「创建」「更新」标注该字段在两个端点下是否必填：

| 字段 | 类型 | 创建 | 更新 | 约束 / 说明 |
|---|---|---|---|---|
| `type` | string | 必填 | 可选 | `game` / `movie` / `book`，也接受 `游戏` / `电影` / `图书`，统一以英文存储 |
| `name` | string | 必填 | 可选 | 标题，非空 |
| 海报 | — | **必填** | 可选 | 见下方 `image_base64` / `image_url`，创建时二者必须且只能给其一；更新不传则沿用原图 |
| `image_base64` | string | 二选一 | 可选 | 裸 base64，或完整 `data:image/...;base64,...` |
| `image_url` | string | 二选一 | 可选 | 公网 `http(s)` 图片地址，由服务端抓取（私网 / 回环地址会被拒） |
| `url` | string | 可选 | 可选 | 来源链接，必须为 `http(s)`，否则 400 |
| `aka` | string \| string[] | 可选 | 可选 | 别名，字符串会原样存，数组按数组存 |
| `genres` | string \| string[] | 可选 | 可选 | 类型 / 题材；传字符串会包成单元素数组 |
| `release_date` | string | 可选 | 可选 | 发行日期或出版年份，格式自由 |
| `rating` | number | 可选 | 可选 | 0..10，超出范围 400 |
| `summary` | string | 可选 | 可选 | 简介 |
| `sort_date` | string | 可选 | 可选 | 排序与展示用日期，创建时不给则取当天 |
| `extra` | object | 可选 | 可选 | 类型专属字段的对象；更新时与已有内容合并，不整体替换 |

类型专属字段既可放进 `extra` 对象，也可直接写在请求体顶层，服务端会折叠进 `extra`（顶层与 `extra` 同名时以 `extra` 内的为准）。仅以下键会被识别：

| 类型 | 顶层 / `extra` 可用字段 |
|---|---|
| `game` | `developer` (string)、`publisher` (string)、`platforms` (string[]) |
| `movie` | `director` (string)、`writers` (string[])、`cast` (string[])、`country` (string)、`language` (string)、`duration` (string)、`imdb` (string) |
| `book` | `author` (string)、`publisher` (string)、`isbn` (string)、`pages` (int)、`price` (string)、`binding` (string)、`series` (string) |

`POST /api/favorites/{id}/image` 不走 JSON：请求体可以是原始图片字节（用 `Content-Type` 头声明 mime），或 `multipart/form-data` 的 `image` 字段。图片按真实字节嗅探格式，只接受 jpeg / png / gif / webp / bmp / avif，单图上限 10 MiB。

### 示例

创建（类型专属字段直接写在顶层，服务端会折叠进 `extra`）：

```bash
curl -X POST http://localhost:23457/api/favorites \
  -H "Content-Type: application/json" \
  -H "X-API-Key: $MEMENTO_API_KEY" \
  -d '{
    "type": "game",
    "name": "哈迪斯2 Hades II",
    "url": "https://www.douban.com/game/36185144/",
    "genres": ["动作", "角色扮演"],
    "release_date": "2025-09-25",
    "developer": "Supergiant Games",
    "platforms": ["PC", "PS5"],
    "image_url": "https://example.com/poster.jpg"
  }'
```

海报二选一：由服务端抓取的 `image_url`（如上），或内联的 `image_base64`（裸 base64，或完整的 `data:image/jpeg;base64,...`）。两者只能给一个，都缺失会返回 400。

更新：

```bash
curl -X PUT http://localhost:23457/api/favorites/151 \
  -H "Content-Type: application/json" \
  -H "X-API-Key: $MEMENTO_API_KEY" \
  -d '{ "rating": 9.4, "summary": "Roguelike 续作。" }'
```

单独替换海报：

```bash
# 原始字节，Content-Type 头声明 mime
curl -X POST http://localhost:23457/api/favorites/151/image \
  -H "X-API-Key: $MEMENTO_API_KEY" \
  -H "Content-Type: image/jpeg" \
  --data-binary @poster.jpg

# 或 multipart 表单
curl -X POST http://localhost:23457/api/favorites/151/image \
  -H "X-API-Key: $MEMENTO_API_KEY" \
  -F "image=@poster.jpg;type=image/jpeg"
```

状态码：201 创建成功，400 输入校验失败，401 key 缺失或错误，404 目标不存在。

## 数据模型

三种类型存在同一张 `favorites` 表里。共有字段是独立的列（可索引、排序、过滤），类型专属字段放在一个 JSON `extra` 列里，新增字段或类型不需要改表结构。图片以 `BLOB` 存在 `image` 列。

```sql
CREATE TABLE favorites (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    type          TEXT NOT NULL CHECK (type IN ('game','movie','book')),
    name          TEXT NOT NULL,
    url           TEXT,
    aka           TEXT,          -- 别名，存为 JSON（输入可传字符串或数组）
    genres        TEXT,          -- JSON 字符串数组
    release_date  TEXT,          -- 发行日期或出版年份
    rating        REAL,          -- 可选，0..10
    summary       TEXT,          -- 可选简介
    extra         TEXT NOT NULL DEFAULT '{}',  -- 类型专属字段的 JSON 对象
    image         BLOB,          -- 海报字节
    image_mime    TEXT,          -- 如 'image/jpeg'
    sort_date     TEXT NOT NULL, -- 排序与展示用日期，默认取创建日期
    created_at    TEXT NOT NULL, -- ISO-8601
    updated_at    TEXT NOT NULL
);
```

`extra` 中按类型可放的字段（全部可选）：

| 类型 | `extra` 字段 |
|---|---|
| `game` | `developer`、`publisher`、`platforms`（数组） |
| `movie` | `director`、`writers`（数组）、`cast`（数组）、`country`、`language`、`duration`、`imdb` |
| `book` | `author`、`publisher`、`isbn`、`pages`（整数）、`price`、`binding`、`series` |

类型字段接受中文或英文：`游戏`/`game`、`电影`/`movie`、`图书`/`book`，统一以英文存储。

## 构建与开发

需要 Rust（edition 2021，rustc/cargo 1.96 及以上）。SQLite 经 bundled feature 自行编译，无系统依赖。

```bash
# 在当前仓库根目录执行

MEMENTO_ENV=development cargo run # 开发运行，先完成开发配置
cargo build --release      # 生产构建，产物为 target/release/memento
cargo test                 # 运行测试
cargo clippy --all-targets # lint
cargo fmt                  # 格式化
```

源码结构：`main.rs` 分派 CLI 或启动服务，`lib.rs` 装配路由与中间件，`handlers/` 处理 HTTP，`models.rs` 做收藏请求体校验与归一化，`repo.rs` 是收藏 SQL 数据访问，`db.rs` 管理连接池与 schema 版本升级，`image.rs` 负责图片解码 / 抓取 / 嗅探，`auth.rs` 做 API Key 鉴权；`browser.rs` 负责登录/session，`diary.rs` 负责日记业务。前端在 `static/`，为零依赖原生 HTML/CSS/JS；release 内嵌，修改后需重新构建并重启，默认 debug 从文件系统读取。

可选隔离回归（先构建 release，均使用临时库与合成凭据）：

```bash
python3 -B tests/browser_smoke.py
python3 -B tests/nginx_smoke.py /path/to/nginx
```

浏览器检查需要 agent-browser CLI 和浏览器；Nginx 检查需要可用的 Nginx 可执行文件和 openssl（仅生成临时自签名证书），省略路径时使用 PATH 中的 nginx。脚本不安装或操作系统 Nginx 服务。完整执行证据、覆盖率口径与部署限制见[验证摘要](docs/diary-operations.md#本轮验证)。

## 其他

### scripts

- [`scripts/submit_example.py`](scripts/submit_example.py)：最小的写接口客户端（标准库 urllib，零依赖），可直接拿来做机器人 handler 里的 `submit_favorite(...)`。
- [`scripts/seed_import.py`](scripts/seed_import.py)：从旧站点（`http://147.79.20.135:23456`）分页拉取全部收藏并回填到本应用，含中英类型映射、海报下载和 base64 注入。默认逐条插入，重复运行会产生重复条目，`--skip-existing` 按 `name` 做尽力去重。注意：因创建强制要求海报，旧站中无图的条目会被服务端拒绝（计入 `failed`）。
- [`scripts/AGENT_GUIDE.md`](scripts/AGENT_GUIDE.md)：给 AI agent（openclaw / nanobot 等）的接入指南，说明如何决定字段、组织 payload 并调用创建 / 更新接口。
- [`scripts/import_diaries.py`](scripts/import_diaries.py)：旧日记库离线保真导入，默认只读 dry-run，`--apply` 才写独立目标；先停服、一致备份并 `init-db`，不能直接用旧源库启动服务。见[迁移流程](docs/diary-operations.md#离线迁移)。

```bash
# 先完成开发认证配置，并给服务端和脚本安全注入同一 API Key
MEMENTO_ENV=development ./memento &

python3 scripts/submit_example.py --base http://localhost:23457
python3 scripts/seed_import.py --old http://147.79.20.135:23456 --new http://localhost:23457 --skip-existing
```

### 安全说明

- 需要外部 key API 时显式设置固定强 `MEMENTO_API_KEY`；未设置/空白会禁用这些接口，不输出或使用临时密钥。浏览器登录独立配置。
- 收藏读取公开、写入需要 `X-API-Key`（常量时间比较）；日记 key API 全部读写受保护，浏览器私有写入两模式均要求 CSRF，生产另要求精确 Origin。
- `url` 字段服务端强制为 `http(s)`，拒绝 `javascript:` / `data:`。
- 抓取 `image_url` 前会解析域名并拒绝指向私网 / 回环 / 链路本地 / CGNAT 的地址，且禁用重定向（防 SSRF）。
- 上传或抓取的图片按真实魔数（jpeg/png/gif/webp/bmp/avif）判定 MIME，不信任客户端声明的 Content-Type。
- 收藏写请求体上限约 20 MiB，单图原始字节上限 10 MiB；日记 JSON body 64 KiB，登录 body 8 KiB。
- 所有响应带 `X-Content-Type-Options: nosniff`、`X-Frame-Options: DENY`、`Referrer-Policy: no-referrer`。
- 不要把 API Key、密码/hash、TOTP secret/code、cookie、数据库、WAL/SHM、备份或实际环境文件提交进版本库。日记/session 响应 no-store；代理禁缓存，不记录搜索 query、认证头和正文。应用不再做 IP 分桶或全局 hash 限流，生产示例由 Nginx 对 `/session` 密码/OTP 入口限流，拒绝为 429。CORS 仅保留旧收藏路由组，不开放 cookie 跨域权限，详见运维文档。
