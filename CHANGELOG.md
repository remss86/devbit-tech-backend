# 更新日志

## [0.6.0] - 2026-10-25
### 新增
- **好友工作台完整后端**：`GET /api/v2/social/summary`、好友备注/星标/分组、`GET/PATCH /friends/{id}/metadata`、分组 CRUD，以及好友列表的 `q/status/starred/groupId` 筛选与 `total`。
- **会话工作台**：`GET /api/v2/social/conversations`、`GET/POST /conversations/{id}/messages`、会话置顶/免打扰/归档、按消息 ID 推进已读；发送使用 `clientMessageId` 幂等，重复请求返回同一条消息。
- **账号恢复 V2**：`POST /api/v2/account/password-reset/code` 与 `/confirm`；统一受理提示避免账号枚举，验证码单次有效、5 次失败锁定、60 秒重发冷却、密码重置后旧密码立即失效。
- **帖子嵌套回复**：`forum_comments.parent_id` 迁移；评论接口返回 `parentId`/`replyTo`，支持最多 5 层嵌套，删除父评论级联删除回复。
- **帖子转发**：`PUT /api/forum/posts/{id}/repost` 切换转发，`ForumPost` 新增 `repostCount`/`repostedByMe`；不复制原帖，评论和点赞保持单份。
- **陌生人私信限制（服务端持久化）**：非好友向同一对象只能发送一条私信，直到对方回复；刷新页面、清除历史或换客户端都不能绕过。旧 `/api/forum/messages` 与新会话接口执行同一规则，返回 `MESSAGE_LIMIT_REACHED`。

### 修复
- 好友列表接收偏好过滤现在与发送端点使用同一规则。
- `clientMessageId` 唯一约束防止网络超时重试产生重复消息。
- 新增 `sqlx` 的 `uuid` 特性以支持消息幂等键。

### 测试
- 新增 5 个 PostgreSQL 集成测试：陌生人单条限制及回复后解锁、会话幂等/已读/偏好、备注与分组权限、嵌套评论与转发、密码重置单次有效与旧密码失效；总计 53 个测试。

## [0.5.0] - 2026-10-25
### 变更
- 删除旧版单向好友模型：移除 `/api/forum/friends` 的列表、添加、删除接口；这些路径现在返回 `404`。
- 新增迁移 `202610250004_remove_legacy_friends.sql`：先把旧数据归档到 `friends_legacy_archive`，再删除 `friends` 表。
- V2 用户关系不再返回 `contact`；只保留 `self | none | outgoing_pending | incoming_pending | friends | blocked`。
- 前后端只支持 `NUXT_PUBLIC_SOCIAL_MODE=v2` 或 `mock`；旧的 `legacy` 单向联系人模式已不可用。
- 部署前请执行 `pg_dump -t friends` 备份；确认新版本正常后，可手动删除 `friends_legacy_archive`。

### 测试
- 新增迁移与路由删除测试；总计 48 个测试。

## [0.4.0] - 2026-10-25
### 安全
- **移除硬编码管理员**：不再把用户 ID 1/2/3 视为管理员。新增 `users.is_admin`，启动时加载；新注册用户恒为 `false`。迁移不会自动提升任何人，审计后需显式 `UPDATE` 并重启。
- **注册输入加固**：昵称 1–50 字符、邮箱规范化且不超过 254 字符、密码至少 8 位并同时包含字母和数字、验证码必须为 6 位数字。
- **验证码防爆破**：最多 5 次失败尝试，超过后作废；同一邮箱 60 秒内只能发送一次验证码（`429` + `Retry-After`）。
- **密码哈希升级**：PBKDF2-HMAC-SHA256 迭代从 100,000 提升到 600,000；旧哈希在登录成功时自动升级；并发哈希数受限。
- **部署前必须轮换 SMTP 授权码**：旧提交历史中存在已暴露的 SMTP 凭据，删除当前文件不能消除历史泄露。详见 `SECURITY-AUDIT.md`。
- 注册失败不再把所有数据库错误都伪装成 `409`，仅邮箱唯一冲突返回 `409`。

### 测试
- 新增注册校验、验证码锁定、邮箱冷却与管理员默认值的集成测试；总计 47 个测试。

## [0.3.1] - 2026-10-25
### 修复
- 修复“操作过于频繁”误报：旧版所有 REST 请求（包括每个头像）共用 60 次/分钟/IP 的额度，SPA 一次刷新或多人共用 NAT 就会耗尽。
- 通用接口改为读/写分离，并按登录用户计数（匿名时按 IP）：读 600 次/分钟、写 120 次/分钟。
- 认证接口调整为 20 次/分钟/IP。
- 头像、健康检查、WebSocket 升级不再计入限流；头像增加一年 `immutable` 缓存。
- 429 响应改为统一错误信封，并返回 `Retry-After` 与 `X-RateLimit-*` 头；可通过 `RATE_LIMIT_*_PER_MINUTE` 环境变量调整阈值。

## [0.3.0] - 2026-10-25
### 新增
- `GET /api/v2/social/feed`：已确认好友的帖子动态流，按发布时间倒序、游标分页。
- `GET /api/v2/social/users/{id}/profile`：公开资料（关系、`canMessage`、最近 5 篇帖子）。
- 好友动态与资料读取复用现有 `ForumPost` 结构和点赞/评论统计。
- 集成测试覆盖好友动态只包含好友、陌生人帖子隔离、本人资料 `self` 关系。

### 说明
- 备注、分组、bio/interests、会话工作台仍为后续迭代；`profiles` 能力当前返回 `recentPosts`，不伪造未实现的字段。

## [0.2.0] - 2026-10-25
### 新增
- Social V2：双向好友、好友申请收发/接受/拒绝/撤回、屏蔽、接收偏好、在线快照、会话历史可见性，统一 `/api/v2/social` 契约与错误信封。
- 私信服务端权限：发送前统一校验双向屏蔽与接收方 `messagePolicy`；`canMessage` 仅作为界面提示。
- WebSocket 社交事件：`friend_request_received`、`friend_request_updated`、`friendship_changed`、`conversation_changed`。
- 帖子、评论、私信的内容长度与空值校验；WebSocket 升级时校验账号仍然存在。
- 数据库迁移 `202610250001_social_v2.sql`，并为论坛常用查询补充索引。
- 5 个 PostgreSQL 集成测试，CI 增加 PostgreSQL 18 服务。

### 修复
- `main` 的 rustfmt 检查失败（PR #11）。
- 历史私信可按会话清除自己的可见记录，不影响对方。
- 关闭已被 `main` 取代且无法合并的旧 PR #10，并在其中说明后续处理。

## [0.0.0] - 2026-4-5
- 如何在用户在线时更新网站内容，不影响用户体验？ --> 双进程缓切换
## [0.0.0] - 2026-4-7
- 🚀实现用户注册登录功能，我们克服了重大阻碍！
## [0.0.0] - 2026-5-15
- 恢复注册功能