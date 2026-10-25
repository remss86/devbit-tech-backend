# 更新日志

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