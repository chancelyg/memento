# 管理后台与 YAML 设置设计

本文记录导航、登录、管理后台和 YAML 功能设置的已实现边界。部署、备份与恢复步骤见 [diary-operations.md](diary-operations.md)，浏览器协议见 [diary-api.md](diary-api.md)。

## 账号与范围

memento 仍是严格单用户应用。唯一浏览器账号就是管理员，`MEMENTO_LOGIN_USERNAME` 仅能修改这个账号的登录名；多个有效 session 只表示同一管理员在多台设备或多个浏览器登录，不表示存在多个用户。当前不引入用户表、多用户、邀请、RBAC、角色、权限分配或账号管理平台。

管理员登录后既可使用私有日记，也可修改公开站点信息。API Key 仍只按原边界授权收藏写入和日记 API，不代表管理员身份，不能替代后台的 Cookie session、CSRF 或 production Origin 校验。

## 配置边界

| 载体 | 内容 | 边界 |
|---|---|---|
| Env / 所选 dotenv | 运行、安全、秘密与业务规则 | 模式、监听地址、路径、日志、API Key、密码 hash、TOTP secret、session、公开 Origin、日记业务时区等；后台不读取或展示秘密 |
| YAML | 管理员可编辑的非秘密功能设置 | 当前仅公开站点的名称、标语和图标；不是秘密存储，不放密码、key、TOTP 或数据库连接信息 |
| SQLite | 收藏、海报、日记、session、TOTP 防重用状态和导入 ledger | 业务数据库 schema 仍为 v3；YAML 不进入 SQLite，旧日记导入器及其 v3 校验不变 |

server 使用 `MEMENTO_CONFIG_PATH` 选择 YAML。development 默认 `./memento.development.yaml`，production 默认 `./memento.production.yaml`；相对路径均相对进程 cwd，不相对二进制或仓库位置。`hash-password`、`totp-secret`、`init-db` 等 CLI 不加载 YAML。

server 首次启动发现目标文件不存在时创建它，父目录必须事先存在且可写；不会自动创建父目录。Unix 上新文件和每次程序生成的替换文件模式为 0600。仓库的 `memento.example.yaml` 只是可跟踪结构样例，不含秘密；默认运行文件及其同目录临时文件已精确加入 `.gitignore`。自定义路径不一定被忽略，部署者仍须检查 Git 状态和文件权限。

首次创建时，旧 `MEMENTO_SITE_NAME`、`MEMENTO_SLOGAN`、`MEMENTO_ICON` 仅作为一次性 seed，并按字段校验；空白值使用内置默认，无效旧值也按字段回退默认并记录不含原值的警告。只要 YAML 已存在，这三个旧变量就被忽略，属于弃用配置；之后应在 `/admin` 修改。既有 YAML 若语法、版本、字段或值无效，server 拒绝启动且不覆盖原文件，避免静默丢失人工修复线索。

程序保存时重新序列化完整设置，不保留 YAML 注释、键顺序或手工排版。当前按单实例设计：进程启动时读取文件，后台成功保存后更新内存；不监听或热加载外部文件修改。运行中直接编辑 YAML 不会生效，并可能在下次后台保存时被覆盖。修改文件后应在单实例停机状态下校验并重启，不要让多个 memento 进程共享同一 YAML。

## YAML 结构与约束

当前格式版本固定为 1，拒绝未知顶层字段和未知 `site` 字段：

```yaml
version: 1
site:
  name: memento
  slogan: 所有的美好都值得被珍藏与分享。
  icon: https://example.com/icon.png
```

站点字段保存前均 trim：

| 字段 | 约束 |
|---|---|
| `name` | 1..80 个 Unicode 标量值 |
| `slogan` | 0..200 个 Unicode 标量值，可为空 |
| `icon` | 内置默认 SVG data URI；或不超过 2048 字节、带 host、无 userinfo 的 HTTPS URL；或严格 base64 的 png/jpeg/gif/webp/bmp/avif data URI |

图片 data URI 解码后最多 256 KiB，声明 MIME 必须与图片魔数一致；不接受 SVG data URI 作为自定义上传值。整个后台 JSON 请求体最多 512 KiB。YAML 中省略 `site` 或其字段会采用内置默认，但后台页面提交并替换完整的 `name`、`slogan`、`icon` 三字段，不是 PATCH。

## 后台协议与页面流程

| 方法与路径 | 认证 | 行为 |
|---|---|---|
| `GET /private/settings/site` | 有效浏览器 session | 返回 `{name,slogan,icon}`；不需要 CSRF |
| `PUT /private/settings/site` | session + `X-CSRF-Token`；production 另需精确 Origin | 校验并完整替换站点设置，返回归一化后的三字段 |

API Key 不能访问或绕过这两个后台接口。响应使用应用 JSON 信封并经私有路由 `no-store` 层；错误媒体类型为 415、body 超限为 413、JSON/字段/值错误为 400。写入结果未知时先重新 GET，不要盲目重复提交。

`/`、`/diary`、`/login`、`/admin` 共用收藏/日记导航、主题按钮和账户入口。页面加载时查询 `/session`：未登录时账户入口指向 `/login`，已登录时显示“管理”并指向 `/admin`。访问 `/diary` 或 `/admin` 时若无有效 session，前端跳转到带受限 `next` 的 `/login`；登录页只允许回到 `/diary` 或 `/admin`，默认进入 `/admin`。这些 HTML 都只是公开页面壳，私有数据和设置仍由鉴权接口读取。

首页已移除可见名称搜索控件，只保留类型筛选和分页加载；公开 `GET /api/favorites?q=...` 的名称搜索参数仍保留，外部客户端兼容性不变。日记页自己的正文与日期筛选不受影响。

## 原子保存与平台边界

每次保存先在目标同目录以独占新建方式写入 `.<目标文件名>.<pid>.<序号>.tmp`，写完同步临时文件，再以 `rename` 替换目标；`rename` 成功是新 YAML 的提交点，之后才更新进程内快照。提交后会尽力同步父目录，目录同步失败只记录警告。写入或 rename 失败时清理临时文件并保留旧文件与旧内存设置。

同目录临时文件避免常规情况下的跨文件系统 rename，但覆盖 rename、目录同步和崩溃持久性目前只在项目主要 Unix 环境按此语义设计；Windows 等平台的替换行为及完整跨平台崩溃恢复尚未验证。该机制也不提供多实例协调或外部编辑合并。
