# memento 写接口接入指南（给 AI agent）

面向 openclaw / nanobot 这类需要代用户向海报墙提交收藏的 agent。目标是：拿到一条电影 / 游戏 / 图书信息后，组织出正确的 payload，调用创建或更新接口，并能看懂返回结果。

## 准备

- 服务地址，例如 `http://localhost:23457`（下文记作 `BASE`）。
- 写鉴权 key，放在请求头 `X-API-Key`。key 通常来自环境变量 `MEMENTO_API_KEY`。
- 读接口不需要 key，写接口（创建 / 更新 / 删除 / 上传图片）都需要。

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
