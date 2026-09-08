# memento 写接口接入指南（给 AI agent）

面向 openclaw / nanobot 这类代用户提交收藏或操作私有日记的 agent。下文收藏流程保持原有协议；日记请使用末尾的独立流程，不要复用收藏 payload 或浏览器登录。

## 准备

- 服务地址，例如 `http://localhost:23457`（下文记作 `BASE`）。
- 写鉴权 key，放在请求头 `X-API-Key`。key 通常来自环境变量 `MEMENTO_API_KEY`。
- 服务启动未设置或仅配置空白 key 时，全部外部 key 鉴权接口禁用，日志不会给出临时 key。请由运维显式配置并安全分发固定 key；浏览器登录仍可独立使用，但不是 Agent 的鉴权替代。
- 收藏读接口不需要 key，收藏写接口（创建 / 更新 / 删除 / 上传图片）都需要；日记 API 的读写全部需要 key。公网/不可信网络推荐 HTTPS，生产 HTTP 内网反代也受支持，不要求先有域名/证书。

服务端无参数启动由系统 `MEMENTO_ENV` 选择 development/production，默认 production，仅加载 cwd 的选定 `.env.development`/`.env.production`，不读通用 `.env` 或父目录，系统值优先且文件不可切模式。开发使用 `MEMENTO_ENV=development cargo run`，生产示例使用 `MEMENTO_ENV=production ./memento` 经 Nginx 接入；两模式浏览器都必须密码 + TOTP，但这不改变 Agent 的 key 协议。Python helper 不自动加载这些环境文件，所需 key 应另行安全注入，不能从浏览器配置提取凭据。详见[运维文档](../docs/diary-operations.md)。

最省事的做法是直接复用 [`submit_example.py`](submit_example.py) 里的 `submit_favorite(...)`，它已经处理好了 payload 组织、鉴权头和错误信封解析：

```python
from submit_example import submit_favorite, MementoError

try:
    dto = submit_favorite(
        "movie", "奥本海默 Oppenheimer",
        url="https://movie.douban.com/subject/35556001/",
        genres=["剧情", "传记"],
        release_date="2023-08-30",
        director="克里斯托弗·诺兰",
        image_url="https://example.com/poster.jpg",
    )
    # dto 是创建后的记录，dto["id"] 是后续更新要用的主键
except MementoError as e:
    # e.status: 400 校验失败 / 401 key 问题 / 404 不存在
    ...
```

## 决定字段

创建一条收藏的必填项：

- `type`：`game` / `movie` / `book`（也接受中文 `游戏` / `电影` / `图书`）。
- `name`：标题。
- 一张海报：`image_url` 或 `image_base64` 二选一（见下文「海报图片」）。两者都缺会返回 400。

其余都可选。按你掌握的信息尽量填，没有就不要放进 payload（不要传空字符串占位）。

通用可选字段：

| 字段 | 类型 | 说明 |
|---|---|---|
| `url` | string | 来源链接，必须是 `http(s)`，否则 400。 |
| `aka` | string 或 string 数组 | 别名。 |
| `genres` | string 或 string 数组 | 类型 / 题材标签。 |
| `release_date` | string | 发行日期或出版年份，格式自由（如 `2023-08-30` 或 `2008`）。 |
| `rating` | number | 0 到 10，超出范围会 400。 |
| `summary` | string | 简介。 |
| `sort_date` | string | 排序用日期，不给则取创建日期。 |

类型专属字段直接写在 payload 顶层，服务端会自动折叠进 `extra`，**不要自己再包一层 `extra`**：

| 类型 | 可用字段 |
|---|---|
| `game` | `developer`、`publisher`、`platforms`（数组） |
| `movie` | `director`、`writers`（数组）、`cast`（数组）、`country`、`language`、`duration`、`imdb` |
| `book` | `author`、`publisher`、`isbn`、`pages`（整数）、`price`、`binding`、`series` |

只有上表里的键会被识别为类型字段；传别的键 `submit_favorite` 会直接报错，避免你写错字段名而静默丢失。

## 海报图片

创建时必须带海报。两种内联来源二选一（不能都给）：

- `image_url`：给一个公网可访问的图片 URL，服务端会去抓取。注意指向私网 / 回环地址会被拒绝（防 SSRF）。
- `image_base64`：裸 base64，或完整的 `data:image/jpeg;base64,...`。适合你本地已经有图片字节的情况。

服务端按图片真实字节判定格式，只接受 jpeg / png / gif / webp / bmp / avif。单图上限 10 MiB。

优先用 `image_url`（让服务端抓）最省事；只有当你手里已经是字节、或源站禁止热链时才用 base64。如果一时拿不到海报，先想办法补一张再创建——创建接口不接受无图条目。

更新（PUT）时图片是可选的：不传沿用原图，传则替换。也可以随时单独替换海报：`POST BASE/api/favorites/{id}/image`，body 为原始字节（用 `Content-Type` 头声明 mime）或 multipart 的 `image` 字段。

## 创建

`POST BASE/api/favorites`，body 是上面拼好的 JSON。成功返回 201，`data` 是完整记录，记下 `data["id"]`——更新和删除都靠它。

```bash
curl -X POST "$BASE/api/favorites" \
  -H "Content-Type: application/json" \
  -H "X-API-Key: $MEMENTO_API_KEY" \
  -d '{
    "type": "book",
    "name": "三体",
    "author": "刘慈欣",
    "isbn": "9787536692930",
    "pages": 302,
    "release_date": "2008",
    "image_url": "https://example.com/cover.jpg"
  }'
```

## 更新

`PUT BASE/api/favorites/{id}`，按 `id` 定位（不是按 `name`）。只传你要改的字段，没传的保持不变。`extra` 里的类型字段是**合并**的：更新 `rating` 不会清掉已有的 `developer`。

```bash
curl -X PUT "$BASE/api/favorites/151" \
  -H "Content-Type: application/json" \
  -H "X-API-Key: $MEMENTO_API_KEY" \
  -d '{ "rating": 9.4, "summary": "补一句简介。" }'
```

要换海报，可以在 PUT 的 body 里带 `image_url` / `image_base64`，或单独调图片端点。

## 创建还是更新？

服务端不会自动去重，重复创建同名条目会产生两条记录。建议的判断流程：

1. 先查是否已存在：`GET BASE/api/favorites?q=<名称>&type=<类型>`，在返回的 `items` 里按 `name` 比对。
2. 命中已有记录 → 用它的 `id` 走 `PUT` 更新。
3. 没有 → 走 `POST` 创建。

## 返回与错误

所有 JSON 响应都是统一信封：`{ "success": bool, "data": ..., "error": ... }`。

| 状态码 | 含义 | 处理 |
|---|---|---|
| 201 | 创建成功 | 读 `data`，保存 `data["id"]` |
| 200 | 更新 / 替换图片成功 | 读 `data` |
| 204 | 删除成功 | 无 body |
| 400 | 输入校验失败 | 读 `error`，按提示修正字段后重试 |
| 401 | key 缺失或错误 | 检查 `X-API-Key` / `MEMENTO_API_KEY` |
| 404 | 目标不存在 | PUT / DELETE / 图片端点的 `id` 不对 |

`error` 是面向人的简短提示，可直接转述给用户；不要在失败时盲目重试 400 / 401。

## 私有日记

完整契约见 [日记 API](../docs/diary-api.md)，部署和离线旧库迁移见 [运维文档](../docs/diary-operations.md)。`submit_favorite(...)` 只处理收藏，不是日记客户端；`seed_import.py` 也不是日记迁移工具。

### 权限边界

- Agent 使用 `/api/diaries`，所有请求（包括列表和详情）都带 `X-API-Key`。本期 key 同时授予全部日记读写和收藏写权限，没有日记专用 key、只读 scope 或按 Agent 分权。
- 不调用 `/session` 获取 cookie，不使用 `/private/diaries`，不收集用户浏览器密码、TOTP、cookie 或 CSRF token。那两组路径是同源浏览器协议，不是第三方登录体系。
- 只在用户授权范围内读取正文和操作记录，避免把全文、搜索 query、认证头或响应体写入工具日志。不要在 shell 参数/history 中放真实正文，不开启请求 trace。

### 操作流程

1. 查找：`GET BASE/api/diaries`。参数为 `page`（默认 1）、`per_page`（默认 24，最多 100）、`q`（正文字面子串）、`start_date`/`end_date`（包含边界的 YYYY-MM-DD）、`sort=asc|desc`（默认 desc）。`data` 是 `{items,total,page,per_page}`，每个 item 包含完整正文。搜索、日期、total、分页和详情都只含未删除记录（deleted_at IS NULL）；DTO 仍为原六字段，不暴露 deleted_at。
2. 确认对象：`GET BASE/api/diaries/{id}` 获取最新 `content`、`version` 和 `ETag`。同日期可能有多篇历史日记，不要把日期当唯一键；收藏 ID 和日记 ID 也不能混用。
3. 创建：`POST BASE/api/diaries`，仅发送 JSON `{"content":"经用户确认的正文"}`，成功 201。正文 trim 后非空且最多 10000 个 Unicode 标量值，整份 JSON 最多 64 KiB。POST/PATCH 不可传 ID、日期、timestamp、version、deleted_at 或 extra，也不能恢复已删记录。
4. 编辑：`PATCH BASE/api/diaries/{id}`，仅发送新的完整正文，带刚读取版本的 `If-Match: "<version>"`，成功 200 并返回新版本和 ETag。不是收藏的 PUT，也不是局部文字 patch；日期不可改。
5. 删除：仅在用户明确授权后 `DELETE BASE/api/diaries/{id}`，带最新 If-Match，成功 204 空 body。它是软删除，不再显示但正文、原日期/created_at 仍保留；deleted_at 为 UTC RFC3339 毫秒时间，updated_at 同值，version 加一。本期无恢复、回收站或永久清除 API，不是安全擦除；收藏仍硬删。

没有未删除日记时，新篇日期取业务时区今天（默认 Asia/Shanghai）；否则取未删除最大日期加一天。删末篇后可能重用该日期，删中间项不补洞，全部软删后重新取今天，但物理表仍有记录，不应向用户报告为日期分配 bug。历史空白正文、重复日期和 null/旧字符串 timestamp 按原值读取；新提交必须符合当前正文规范，不自动“修复”历史数据。

### 冲突与重试

- PATCH/DELETE 的 If-Match 可选，无头允许操作最新未删除记录，不再返回缺头 428。为保护用户修改，Agent 推荐继续先读后带版本，网页也仍传版本；合法格式如 `If-Match: "1"`，裸数字或通配符等非法头返回 400。
- 412 表示未删除记录已被其他客户端修改。保留用户草稿，重新读最新正文，展示差异并取得确认后再提交；禁止仅替换版本号自动覆盖或删除。
- 404 表示记录不存在或已软删除（前置校验通过后），PATCH/重复 DELETE 同样如此；不要自动 POST 重建，尤其是用户主动删除的记录。已删 ID 的非法 If-Match 仍为 400，省略不触发缺头错误；同版本并发 DELETE 仅一个 204、另一个 404。版本溢出为 409 且不删除。
- POST 没有幂等键或正文去重，网络超时后先查询确认是否已创建，不能盲重试生成下一日期的重复日记。PATCH/DELETE 响应不明时同样重新读取核对。
- 400/413/415 修正请求再发；401 检查 key，不回退到浏览器密码；409 和内部错误停止自动写入并报告脱敏信息。

不调用不存在的 stats、goal、audit、恢复、回收站、永久清除或独立 JSON 导入导出端点。旧库迁移只使用 `import_diaries.py` 的独立停服流程：目标须先初始化/升级至完整 schema v3（迁移历史 1、2、3），含 browser_totp_state 元数据校验；导入器不读该表真实状态。默认 dry-run，--apply 才写目标。source 五字段不变，新导入显式 deleted_at=NULL；匹配 ledger 则跳过，不覆盖编辑、不清软删或复活。软删 diary_id 仍指原行，ON DELETE SET NULL 仅兼容历史物理删除。首次导入要求物理空目标日记表，全部软删不满足；所有未入 ledger 的源 ID 必须大于目标日记 sqlite_sequence 的导入前 high watermark，历史硬删本地 ID 也不能复用。不能用它代替在线批量 POST，不能靠删 ledger、目标数据或重置 sequence 处理冲突；计数以实际 dry-run 输出为准。
