# memento

[![CI](https://github.com/chancelyg/memento/actions/workflows/ci.yml/badge.svg)](https://github.com/chancelyg/memento/actions/workflows/ci.yml)

电影 / 游戏 / 图书三合一的个人海报墙。一个 Rust 单文件二进制 Web 应用：数据和前端都打包进可执行文件，读取公开、写入用 API Key 鉴权。

数据存在本地 SQLite 文件里（含海报图片本身），前端静态资源在编译期内嵌进二进制，所以部署只需要拷贝一个文件。新增、修改收藏都通过 HTTP API 完成，方便由脚本、Telegram 机器人或 agent 提交。

特性：

- 三种类型（电影、游戏、图书）共用一面墙，支持按类型过滤、按名称搜索、分页。
- 单文件二进制：SQLite 经 `rusqlite` bundled feature 编译，无系统依赖；前端经 `rust-embed` 内嵌。
- 读接口公开，写接口（POST / PUT / DELETE / 上传图片）需 `X-API-Key`。
- 海报图片支持三种入库方式：base64、由服务端抓取的 URL、直接上传字节。
- 暗色前端，含详情弹窗、懒加载、明暗主题切换。

## 运行

从 [Releases](../../releases) 下载对应平台的二进制，赋予可执行权限后直接运行即可，无需安装运行时或额外依赖。

```bash
# 下载并重命名（示例，按实际 release 资产名调整）
chmod +x memento

export MEMENTO_API_KEY="$(openssl rand -hex 24)"   # 写接口鉴权 key
./memento
```

启动后访问 `http://<host>:23457/`。若未设置 `MEMENTO_API_KEY`，程序会生成一个临时随机 key 并在启动日志中以 WARN 打印一次（`generated ephemeral API key: ...`）——仅适合本地试用，生产环境务必显式设置。

### 配置

全部通过环境变量配置，程序不会自动读取 `.env` 文件（`.env.example` 仅供你手动复制并 `source`/`export`）。

| 变量 | 默认值 | 说明 |
|---|---|---|
| `MEMENTO_API_KEY` | 未设置则随机生成 | 写接口鉴权 key。未设置时仅用于本地开发，生产环境必须显式设置。 |
| `MEMENTO_DB_PATH` | `./memento.db` | SQLite 文件路径。 |
| `MEMENTO_BIND` | `0.0.0.0:23457` | 监听地址。 |
| `RUST_LOG` | `info` | tracing 日志过滤器，例如 `memento=debug,tower_http=debug`。 |

```bash
export MEMENTO_API_KEY="$(openssl rand -hex 24)"
export MEMENTO_DB_PATH=/var/lib/memento/memento.db
export MEMENTO_BIND=0.0.0.0:23457
./memento
```

## API

所有 JSON 端点返回统一信封：

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
| `POST /api/favorites` | 创建一条收藏 | `201` + `FavoriteDto` |
| `PUT /api/favorites/{id}` | 按 `id` 部分更新（不是按 `name`） | `200` + `FavoriteDto` |
| `DELETE /api/favorites/{id}` | 删除 | `204` 空 body |
| `POST /api/favorites/{id}/image` | 替换海报 | `200` + `FavoriteDto` |

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
cd /root/codes/memento

cargo run                  # 开发运行
cargo build --release      # 生产构建，产物为 target/release/memento
cargo test                 # 运行测试
cargo clippy --all-targets # lint
cargo fmt                  # 格式化
```

源码结构：`main.rs` 装配路由与中间件，`handlers/` 处理 HTTP，`models.rs` 做请求体校验与归一化，`repo.rs` 是纯 SQL 数据访问，`db.rs` 管理连接池与建表，`image.rs` 负责图片解码 / 抓取 / 嗅探，`auth.rs` 做 API Key 鉴权。前端在 `static/`，为零依赖的原生 HTML/CSS/JS，编译期内嵌——改完需要重新构建并重启进程才会生效。

## 其他

### scripts

- [`scripts/submit_example.py`](scripts/submit_example.py)：最小的写接口客户端（标准库 urllib，零依赖），可直接拿来做机器人 handler 里的 `submit_favorite(...)`。
- [`scripts/seed_import.py`](scripts/seed_import.py)：从旧站点（`http://147.79.20.135:23456`）分页拉取全部收藏并回填到本应用，含中英类型映射、海报下载和 base64 注入。默认逐条插入，重复运行会产生重复条目，`--skip-existing` 按 `name` 做尽力去重。注意：因创建强制要求海报，旧站中无图的条目会被服务端拒绝（计入 `failed`）。
- [`scripts/AGENT_GUIDE.md`](scripts/AGENT_GUIDE.md)：给 AI agent（openclaw / nanobot 等）的接入指南，说明如何决定字段、组织 payload 并调用创建 / 更新接口。

```bash
export MEMENTO_API_KEY="$(openssl rand -hex 24)"
./memento &

python3 scripts/submit_example.py --base http://localhost:23457
python3 scripts/seed_import.py --old http://147.79.20.135:23456 --new http://localhost:23457 --skip-existing
```

### 安全说明

- 生产环境必须显式设置 `MEMENTO_API_KEY`；未设置时生成的临时 key 仅适合本地开发。
- 写接口都需要 `X-API-Key`（常量时间比较，避免计时侧信道），读接口公开。
- `url` 字段服务端强制为 `http(s)`，拒绝 `javascript:` / `data:`。
- 抓取 `image_url` 前会解析域名并拒绝指向私网 / 回环 / 链路本地 / CGNAT 的地址，且禁用重定向（防 SSRF）。
- 上传或抓取的图片按真实魔数（jpeg/png/gif/webp/bmp/avif）判定 MIME，不信任客户端声明的 Content-Type。
- 写请求体上限约 20 MiB，单图原始字节上限 10 MiB。
- 所有响应带 `X-Content-Type-Options: nosniff`、`X-Frame-Options: DENY`、`Referrer-Policy: no-referrer`。
- 不要把 `MEMENTO_API_KEY`、`*.db`、`.env` 提交进版本库（见 `.gitignore`）。如需限流或收紧 CORS，建议在前置反代层处理。
