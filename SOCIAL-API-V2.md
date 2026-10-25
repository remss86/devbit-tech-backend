# 社交与账号恢复 V2 接入契约

版本：前端草案 1（后端 0.2.0 已适配核心）· 2026-10-25。核心端点已在 `/api/v2/social` 实现，逐项状态见文末适配清单；未适配能力保持关闭。现有接口仍见 [api.md](./api.md)。前端类型源位于 `shared/social.ts`，HTTP 适配位于 `app/utils/socialService.ts`。

## 模式及启用顺序

| 配置 | 默认值 | 含义 |
|---|---|---|
| `NUXT_PUBLIC_SOCIAL_MODE` | `v2` | `v2` 使用双向好友及申请；`mock` 只用于开发；`legacy` 已在后端 0.5.0 移除 |
| `NUXT_PUBLIC_SOCIAL_BLOCKS_ENABLED` | `false` | V2 屏蔽能力 |
| `NUXT_PUBLIC_SOCIAL_PREFERENCES_ENABLED` | `false` | V2 接收偏好 |
| `NUXT_PUBLIC_SOCIAL_PRESENCE_ENABLED` | `false` | V2 在线快照 |
| `NUXT_PUBLIC_SOCIAL_HISTORY_ENABLED` | `false` | V2 仅清除自己的历史可见性 |
| `NUXT_PUBLIC_ACCOUNT_RECOVERY_MODE` | `disabled` | `disabled`、`v2` 或开发用 `mock` |

`v2` 要求用户搜索、好友列表、申请收发处理及解除好友整组完成；其余能力分别开启。后端 0.5.0 起 `legacy` 单向联系人接口和 `friends` 表已删除，客户端必须使用 `v2`；真实请求失败不会切换数据源，404 也不触发旧写接口或 Mock 回退。构建和运行均禁止生产 Mock。

Mock 按 Nuxt 应用和当前账号独立创建，刷新后重置。`/dev-preview` 只在开发且社交 Mock 模式开放，自动使用虚构预览账号；帖子是只读样例，社交及私信在内存中运行，不发送真实写请求。Mock 演示单个当前用户的视角；邮件投递、跨账号同步、权限执行及真实推送必须由后端联调确认。

## 通用约定

- 浏览器请求前缀默认 `/api`，适配层内部路径从 `/v2` 开始。
- Cookie 鉴权、同源凭证及现有 CSRF/鉴权策略沿用后端实现。所有写权限必须由后端校验。
- JSON 字段 `camelCase`；用户和申请 ID 为正安全整数；时间为带时区的 ISO 8601 字符串。
- 列表返回 `{ "items": [], "nextCursor": null }`，不是直接数组。游标为不透明字符串，前端原样传回。建议每页 20，固定稳定排序并避免重复记录。
- 成功读取/修改返回 200；创建申请可返回 201；无响应体的操作可返回 204。POST 重复提交不得创建重复申请。前端关闭自动重试，单次超时 10 秒。
- HTTP 错误必须保留真实状态；业务码决定友好提示。前端不根据英文 message 推断关系。

```json
{
  "error": {
    "code": "RATE_LIMITED",
    "message": "操作过于频繁",
    "fieldErrors": { "email": "请填写有效邮箱" },
    "retryAfterSeconds": 60
  }
}
```

`fieldErrors`、`retryAfterSeconds` 可省略。表单仅应用对应表单已知的字段错误；未知错误码使用 HTTP 状态及操作的通用提示，不直接展示服务端堆栈。

| HTTP | code | 语义与前端行为 |
|---|---|---|
| 400/422 | `VALIDATION_FAILED` | 字段校验失败，修正后重试 |
| 400 | `INVALID_CODE` | 验证码错误或过期，重新获取 |
| 400 | `BAD_CURSOR` | 游标失效，重新加载第一页 |
| 401 | `UNAUTHENTICATED` | 登录已过期 |
| 403 | `FORBIDDEN` | 关系、屏蔽或偏好禁止此操作 |
| 404 | `USER_NOT_FOUND` | 用户不存在，刷新状态 |
| 409 | `RELATIONSHIP_CHANGED` | 关系已变化，保留错误并刷新列表/搜索 |
| 409 | `REQUEST_NOT_PENDING` | 申请已被处理，刷新列表/搜索 |
| 429 | `RATE_LIMITED` | 频率限制，后端提供重试时间 |
| 501/503 | `FEATURE_NOT_READY` | 能力未开放，不能显示成功 |
| 5xx | `INTERNAL_ERROR` | 服务故障，保留输入并允许重试 |

## 数据结构

公开用户沿用 `ForumUser`：`id`、`name`、`avatar`、可选 `avatarUrl`、`isAdmin`。不包含邮箱或私密资料。

```json
{
  "id": 42,
  "name": "示例成员",
  "avatar": "示",
  "isAdmin": false,
  "relationship": "incoming_pending",
  "requestId": 81,
  "canMessage": true
}
```

`SocialUser.relationship`：`self | none | outgoing_pending | incoming_pending | friends | blocked`。`contact` 已在 0.5.0 随单向联系人模型一起移除，不再返回。两个 pending 状态必须附有效 `requestId`。`canMessage` 是当前用户向此人发送私信的最终权限，综合双方屏蔽及**接收方**偏好，由后端返回。

`Friendship`：`{ user: SocialUser, createdAt: ISODate }`。

```json
{
  "id": 81,
  "sender": { "id": 42, "name": "示例成员", "avatar": "示", "isAdmin": false },
  "recipient": { "id": 7, "name": "当前用户", "avatar": "当", "isAdmin": false },
  "message": "想一起交流图形实验",
  "status": "pending",
  "createdAt": "2026-10-17T08:00:00Z",
  "updatedAt": "2026-10-17T08:00:00Z"
}
```

`FriendRequest.status`：`pending | accepted | rejected | cancelled`。列表当前只请求 pending；处理后的响应可返回终态。申请与双方关系的更新应在同一事务中完成。

## 社交端点

前缀 `/api/v2/social`。下表响应中的 Page、SocialUser、Friendship、FriendRequest 见以上结构。

| 方法与路径 | 请求 | 成功响应 | 行为 |
|---|---|---|---|
| `GET /users` | query `q`, `cursor?` | `Page<SocialUser>` | 搜索公开昵称，返回与当前用户的关系和权限 |
| `GET /friends` | query `cursor?` | `Page<Friendship>` | 双方确认的好友；屏蔽用户在屏蔽列表管理 |
| `GET /friend-requests` | query `direction=incoming\|outgoing`, `status=pending`, `cursor?` | `Page<FriendRequest>` | 当前用户收/发的待处理申请 |
| `POST /friend-requests` | `{ recipientId, message? }` | `FriendRequest` | 重复发出同一待处理申请返回已有申请；自己、屏蔽或拒收不能申请 |
| `POST /friend-requests/{id}/accept` | 无 body | `FriendRequest` accepted | 仅接收方可接受；原子建立双向关系 |
| `POST /friend-requests/{id}/reject` | 无 body | `FriendRequest` rejected | 仅接收方可拒绝 |
| `DELETE /friend-requests/{id}` | 无 body | 204 | 仅发送方撤回 pending 申请 |
| `DELETE /friends/{userId}` | 无 body | 204 | 双向解除；聊天记录保留；建议重复移除幂等 |
| `GET /blocks` | query `cursor?` | `Page<SocialUser>` | 仅当前用户的屏蔽列表 |
| `PUT /blocks/{userId}` | 无 body | 204 | 幂等屏蔽；限制新私信与申请，保留历史 |
| `DELETE /blocks/{userId}` | 无 body | 204 | 幂等解除屏蔽，再查询实际关系 |
| `GET /preferences` | 无 | `SocialPreferences` | 返回生效值，不能由首次打开页面覆盖 |
| `PATCH /preferences` | `Partial<SocialPreferences>` | `SocialPreferences` | 返回保存后的完整设置 |
| `GET /presence` | query `userIds=1,2,3` | `PresenceSnapshot` | 仅有权限查看的用户；未知不能伪装为离线 |
| `DELETE /conversations/{userId}/history` | 无 body | 204 | 仅改变当前用户历史可见性，不删除对方记录 |

已处理的申请再次 accept/reject/cancel 返回 409 `REQUEST_NOT_PENDING`。反向申请存在时返回 409 `RELATIONSHIP_CHANGED`，前端刷新后显示接受/拒绝，不能偷偷自动接受。后端应明确申请留言长度、昵称查询长度、游标失效及频控限制。

屏蔽不隐式销毁历史好友关系；解除屏蔽后重新获取关系，不能由前端猜测是否恢复好友。申请 pending 与屏蔽的最终处置由后端统一执行，响应不得允许被屏蔽者继续新发送。

```json
{ "messagePolicy": "friends", "friendRequestPolicy": "everyone" }
```

私信接收偏好 `everyone | friends | nobody`，好友申请接收偏好 `everyone | nobody`。接收设置不限制主动发给允许接收的其他成员。

```json
{
  "users": [{ "userId": 42, "status": "online" }, { "userId": 43, "status": "unknown" }],
  "generatedAt": "2026-10-17T08:00:00Z"
}
```

在线值 `online | offline | unknown`；缺项视为未确认。前端在断线时清空快照，重连后重新获取，不把断线解释成全部离线。

## 复用的私信接口

`GET /api/forum/messages`、`POST /api/forum/messages { recipientId, content }`、`PUT /api/forum/messages/conversation/{userId}/read`、`PUT /api/forum/messages/{id}/read` 维持现有协议，返回结构见 `shared/forum.ts`。

V2 模式也由统一服务调用这些端点；后端必须同步在实际发送端点执行双向屏蔽与接收偏好校验。`canMessage` 只控制界面，不能替代服务端鉴权。会话历史清除后，原消息列表端点必须遵循当前用户可见性边界，且新消息仍能送达。前端只有在会话打开、窗口展开、页面可见时才标记该会话已读。

## 实时事件

沿用现有 `/api/ws` 连接及原有消息事件结构；新增顶层 `type`：

```json
{ "type": "friend_request_received", "requestId": 81, "userId": 42 }
```

| type | 主要触发 | 前端处理 |
|---|---|---|
| `friend_request_received` | 收到申请 | 重新获取好友、申请及在线快照 |
| `friend_request_updated` | 接受/拒绝/撤回 | 同上 |
| `friendship_changed` | 建立/解除/屏蔽影响关系 | 同上 |
| `presence_snapshot` | 在线快照失效 | 重新获取 REST 快照 |
| `presence_changed` | 在线变化 | 重新获取 REST 快照 |

`userId`、`requestId` 可选，若提供必须是数字 ID。当前前端将事件作为缓存失效信号，不从事件拼装最终关系；重连也刷新。后端应只向有权限的当前账号推送，不包含私密用户资料。当前未对高频事件批量合并，上线前需确认频率可控。

## 账号恢复

前缀 `/api/v2/account`，与社交模式独立配置。

`POST /password-reset/code`：

```json
{ "email": "member@example.com" }
```

```json
{ "message": "如邮箱可以恢复，验证码将发送到该邮箱。", "expiresInSeconds": 600, "retryAfterSeconds": 60 }
```

前端采用响应重发间隔倒计时；服务端执行实际有效期与频控。生产禁止返回验证码。无论邮箱是否存在，都应返回通用受理信息，避免暴露账号存在性。

`POST /password-reset/confirm`：

```json
{ "email": "member@example.com", "code": "123456", "password": "new-password" }
```

成功 204；验证码错误/过期 400 `INVALID_CODE`，字段不合规则 422 `VALIDATION_FAILED`。确认密码仅在前端验证，不传给后端。成功后进入登录，验证码应作废；会话失效策略由后端落实。开发 Mock 固定验证码 123456，仅验证界面状态，不重置任何真实账号。

## 适配清单

状态说明：**待适配**＝前端结构已预留，未确认后端实现；**可联调**＝后端已实现并提供测试环境；**已验证**＝在该环境完成端到端验收。Mock 通过不改变下列状态。

| 工作项 | 当前状态 | 后端验收要点 |
|---|---|---|
| V2 用户搜索/好友列表 | 可联调 | 分页、关系、requestId、canMessage |
| 申请收发及处理 | 可联调 | 双向原子关系、幂等、并发与权限 |
| 解除好友 | 可联调 | 双向解除、保留会话 |
| 历史单向联系人 | 已删除 | 0.5.0 迁移 004 归档到 `friends_legacy_archive` 后删除 `friends` 表 |
| 屏蔽 | 可联调 | 实际发送/申请均拦截、解除后查询 |
| 接收偏好 | 可联调 | 接收方权限、持久化、错误码 |
| 在线状态 | 可联调 | 真实在线来源、可见范围、断线未知 |
| 自己的历史可见性 | 可联调 | 不影响对方、读取端点遵循边界 |
| 社交 WebSocket 事件 | 可联调 | 鉴权、重连恢复、控制推送频率 |
| 账号恢复与邮件 | 待适配 | 通用提示、验证码单次有效、频控与会话策略 |

### 后端实现说明（0.5.0）

已实现的前缀为 `/api/v2/social`（同时兼容无 `/api` 前缀）：

- `GET /users`、`GET /friends`、`GET /friend-requests`、`GET /blocks`：游标分页，`limit` 默认 20、最大 50；`nextCursor` 为不透明字符串，失效时返回 400 `BAD_CURSOR`。
- `POST /friend-requests`、`POST /friend-requests/{id}/accept`、`POST /friend-requests/{id}/reject`、`DELETE /friend-requests/{id}`：同一对用户同一时刻只保留一条 pending 申请；重复发送返回已有申请；接受时在单个事务中建立对称关系并把其余 pending 申请标记为 cancelled。
- `DELETE /friends/{userId}`、`PUT/DELETE /blocks/{userId}`：幂等；屏蔽会取消双方 pending 申请，但保留历史好友关系。
- `GET/PATCH /preferences`：`messagePolicy` 为 `everyone | friends | nobody`，`friendRequestPolicy` 为 `everyone | nobody`；未保存时返回默认值。
- `GET /presence?userIds=...`：只返回本人与已确认好友，最多 100 个；在线值来自 WebSocket 连接，未连接为 `offline`。
- `GET /users/{userId}/profile`：返回 `SocialUser` 字段、查看者关系、`canMessage` 以及最近 5 篇帖子的 `recentPosts`；`bio`/`interests` 尚未实现并省略。
- `GET /feed`：已确认好友的帖子动态流，返回现有 `ForumPost` 结构，游标分页。
- `DELETE /conversations/{userId}/history`：只改变当前账号的可见性，不删除消息。
- 私信 `POST /api/forum/messages` 在服务端执行双向屏蔽和接收方偏好校验，违规返回 403。
- 错误响应统一为 `{"error":{"code","message","fieldErrors?","retryAfterSeconds?"}}`，业务码沿用本文上表；未知用户 404 `USER_NOT_FOUND`，申请不存在 404 `REQUEST_NOT_FOUND`。
- WebSocket 事件在 `{type, userId?, requestId?}` 信封内发送；前端应把事件当作缓存失效信号并重新读取鉴权接口。

尚未适配：好友备注/分组、公开资料的 bio/interests 字段、会话工作台与发送幂等、账号恢复与邮件。它们不会因为本次后端升级而自动启用。

联调顺序：在测试环境使用 v2 完成核心整组 → 逐项启用扩展开关 → 两个独立账号验证关系、权限、历史与事件 → 默认生产构建与部署验收。旧版单向联系人已删除，不再提供 legacy 回退。

## 好友中心与私信工作台扩展契约

以下扩展中，公开资料 `GET /users/{userId}/profile` 与好友动态 `GET /feed` 已提供只读接口；备注、分组、汇总、会话工作台等仍待适配。路径前缀 `/api/v2/social`，身份来自会话 Cookie，路径中的用户 ID 均为会话另一方 ID，不能指定数据所有者。沿用正安全整数 ID、ISO 8601 时间、camelCase、真实 HTTP 错误及不透明分页游标。所有扩展默认关闭，不会因 404/网络故障退回 Mock 或旧写接口。

### 能力开关

| 环境变量（NUXT_PUBLIC_SOCIAL_ 前缀） | 对应能力 | 启用条件 |
|---|---|---|
| `DIRECTORY_ENABLED` | directory | 服务端好友筛选、汇总均完成 |
| `PROFILES_ENABLED` | profiles | 公开资料、`recentPosts` 及当前查看者关系/私信权限（bio/interests 未提供） |
| `METADATA_ENABLED` | metadata | 私人备注与星标持久化 |
| `GROUPS_ENABLED` | groups | 分组 CRUD、单一归属，需同时启用 metadata |
| `CONVERSATIONS_ENABLED` | conversations | 会话列表、历史分页、发送幂等、偏好及标记已读整组完成 |
| `RECEIPTS_ENABLED` | receipts | 对方已读状态可用且会推送变化，需同时启用 conversations |

后端 0.5.0 起不再提供 legacy 模式，上述扩展只在 v2 下按开关启用；Mock 在开发环境完整启用。未开放的备注、分组、申请、在线及会话整理展示明确状态。旧私信仍调用原 `/forum/messages` 系列，不声称具备服务端幂等或对方已读能力。

### 好友、分组与公开资料

| 方法与路径 | 请求 | 返回 |
|---|---|---|
| `GET /friends` | `cursor?`, `q?`, `status=online?`, `starred=true?`, `groupId?` | `Page<Friendship>`，可附当前查询的 `total` |
| `GET /summary` | 无 | `{ total, online, starred, incoming }`，均为当前账号完整集合数量 |
| `GET /users/{userId}/profile` | 无 | `SocialProfile` |
| `PATCH /friends/{userId}/metadata` | `{ note?, starred?, groupId? }` | 完整 `FriendMetadata` |
| `GET /friends/{userId}/metadata` | 无 | 当前账号的 `FriendMetadata`，用于打开尚未加载到好友列表的成员资料 |
| `GET /groups` | 无 | `FriendGroup[]` |
| `POST /groups` | `{ name }` | 创建后的 `FriendGroup` |
| `PATCH /groups/{id}` | `{ name }` | 更新后的 `FriendGroup` |
| `DELETE /groups/{id}` | 无 | 204 |

`q` 匹配昵称和当前账号私人备注，忽略首尾空格与大小写；条件组合取交集。省略 `groupId` 表示所有分组；查询字符串 `groupId=null` 表示未分组。好友顺序为建立关系时间倒序、用户 ID 倒序，游标绑定查询条件；筛选变化从第一页开始。`total` 缺失时前端不把当前页数量当作完整总数。在线状态不可用时应返回 unknown，不推断离线。

```json
{
  "user": { "id": 1001, "name": "林间代码", "avatar": "林", "isAdmin": false, "relationship": "friends", "canMessage": true },
  "createdAt": "2026-10-18T00:00:00Z",
  "metadata": { "note": "林同学", "starred": true, "groupId": 7 }
}
```

`FriendMetadata` 仅本人可见，备注最长 60 字符；星标独立于分组；每位好友最多属于一个分组。分组 `{ id, name }`，名称去除首尾空格后 1–30 字符。修改必须检查好友关系及分组所有权。删除分组须原子地将其中好友移至未分组，不能解除关系。删除/屏蔽好友保留会话历史；解除屏蔽恢复屏蔽前仍有效的关系，不自动创建新关系。

`SocialProfile` 扩展 `SocialUser`，可选 `bio`、`interests: string[]`、`recentPosts: { id, title }[]`。仅返回公开资料和可访问的公开帖子，不包含邮箱、他人备注、分组或私密内容。缺失字段前端省略。没有 profile 能力时只展示已有成员基本信息，不能伪造扩展资料或权限。

### 会话、历史与发送

| 方法与路径 | 请求 | 返回 |
|---|---|---|
| `GET /conversations` | `cursor?` | `Page<ConversationSummary>` |
| `GET /conversations/{userId}/messages` | `cursor?` | `Page<WorkspaceMessage>`，最近消息在前 |
| `POST /conversations/{userId}/messages` | `{ recipientId, content, clientMessageId }` | `WorkspaceMessage` |
| `PATCH /conversations/{userId}/preferences` | `{ pinned?, muted?, archived? }` | 完整 `ConversationPreferences` |
| `PUT /conversations/{userId}/read` | `{ lastReadMessageId }` | 204，仅推进至该读取位置，不标记之后到达的消息 |
| `DELETE /conversations/{userId}/history` | 无 | 204，仅清除请求者的历史可见性 |

```json
{
  "items": [{
    "user": { "id": 1001, "name": "林间代码", "avatar": "林", "isAdmin": false, "relationship": "friends", "canMessage": true },
    "lastMessage": { "id": 3001, "sender": { "id": 1001, "name": "林间代码", "avatar": "林", "isAdmin": false }, "recipient": { "id": 1000, "name": "我", "avatar": "我", "isAdmin": false }, "content": "你好", "createdAt": "2026-10-18T00:00:00Z", "isRead": false },
    "unread": 1,
    "preferences": { "pinned": true, "muted": false, "archived": false }
  }],
  "nextCursor": null
}
```

会话按置顶优先、最后消息时间倒序、另一方 ID 排序；包含归档项，前端对已加载会话做搜索/未读/归档筛选，并保留加载更多入口。偏好属于当前账号：免打扰不投递主动通知，但保留未读计数；新消息不自动解除归档。历史按时间及 ID 倒序分页，前端合并后正序显示；新消息到达时保留用户阅读位置。

`WorkspaceMessage` 沿用 `ForumMessage`，增加可选 `clientMessageId`。新发送请求必须带 UUID，后端按 `(senderId, clientMessageId)` 唯一存储；重复重试返回同一消息，内容不一致返回 409。响应与历史均应回传该标识，供推送/HTTP 超时对账。对方已读仅在 receipts 能力启用时展示；发送中/失败属于前端 outbox 状态，不能当作服务器已送达。

内容最多 10000 字符（文字）；兼容已有附件消息编码：`![文件名](data:image/png;base64,...)` 或 `[文件名](data:application/octet-stream;base64,...)`。单文件最多 10MB，前端每批最多 5 个，后端须独立验证大小、MIME、权限。图片预览仅允许 PNG/JPEG/GIF/WebP/AVIF/BMP；其余为下载附件，禁止将任意 HTML/SVG 注入消息。附件与文字分别作为消息发送，失败项单独保留供重试。本轮未新增文件上传存储服务。

草稿与待发送附件仅保留在当前账号的内存中，不写 localStorage；离开会话保留，退出/切换账号/刷新清除。未开放 conversations 的旧消息端点不支持幂等，网络超时重试可能重复，后端联调应优先完成新端点整组。

### 推送与重连

新增事件沿用 `{ type, userId?, requestId? }` 信封：

- `friend_metadata_changed`：仅向数据所有者推送，刷新好友/分组汇总。
- `friend_group_changed`：分组创建、重命名或删除后向所有者推送。
- `conversation_changed`：新消息或偏好变化后向相关会话参与者推送；私人偏好不得泄露给对方。
- `conversation_read`：读取状态变化，刷新摘要及已加载历史。

事件作为失效提示，前端从鉴权接口重新读取，不将推送内容直接当作可信私有数据。每类读取独立序号，旧响应不得覆盖新响应；账号切换递增会话代次，旧数据、错误和发送结果均丢弃。重连重新读取好友、申请、分组、摘要和已打开历史；断线在线状态归为未确认。生产需验证双账号权限、跨标签同步、关系并发变更、幂等、屏蔽和历史可见性；开发 Mock 无法证明这些后端保证。

### 开发演示与验收入口

开发设置 `NUXT_PUBLIC_SOCIAL_MODE=mock` 后，`/friends`、`/messages`、`/messages/1001`、`/dev-preview` 使用虚构账号。初始有九位好友、两个分组、一条来自向量与光的申请和四段会话。可搜索一页笔记发送带留言申请；接受向量与光；为林间代码设置备注/星标/分组；测试归档、免打扰、发送文字/表情/文件。刷新重置，生产构建与运行禁止 Mock。后端、数据库及部署本轮不变。
