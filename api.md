---

## 通用说明

- 所有请求与响应均使用 JSON 格式（除非另有说明）。
- 用户认证通过 JWT 实现，客户端可以在请求头中携带 `Authorization: Bearer <token>`，或由服务器在登录时通过 `Set-Cookie` 下发的 `auth_token` cookie 自动附带。
- 未注明“需要认证”的接口表示可选认证（部分字段会根据用户身份变化）或无需认证。
- 所有论坛相关的接口均以 `/api/forum` 开头，认证相关接口存在带 `/api` 前缀和不带前缀的两套路由，功能完全相同。

---

## 1. 身份认证

### 1.1 发送验证码
- **函数名**：`send_verification_code`
- **URL**：`POST /register/send_code` 或 `POST /api/register/send_code`
- **接收数据**：
  ```json
  {
    "email": "string"
  }
  ```
- **返回数据**：
  ```json
  {
    "message": "string",
    "expires_in_seconds": 600,
    "development_code": "string"   // 仅开发环境且未配置SMTP时出现
  }
  ```
- **数据类型**：`SendCodeRequest` → `SendCodeResponse`
- **功能**：向指定邮箱发送 6 位数字验证码（有效期 10 分钟）。若未配置 SMTP 环境变量，则直接返回验证码供开发测试使用。

### 1.2 用户注册
- **函数名**：`create_user`
- **URL**：`POST /register` 或 `POST /api/register`
- **需要认证**：否
- **接收数据**：
  ```json
  {
    "name": "string",
    "email": "string",
    "code": "string",       // 验证码
    "password": "string"
  }
  ```
- **返回数据**：
  ```json
  {
    "id": 1,
    "name": "string",
    "email": "string"
  }
  ```
- **数据类型**：`CreateUserRequest` → `CreateUserResponse`
- **功能**：验证邮箱和验证码后创建新用户。密码经 PBKDF2‑SHA256 哈希存储。成功后清除对应验证码。

### 1.3 登录
- **函数名**：`login_check`
- **URL**：`POST /login` 或 `POST /api/login`
- **需要认证**：否
- **接收数据**：
  ```json
  {
    "email": "string",
    "password": "string"
  }
  ```
- **返回数据**：
  - HTTP 头部：`Set-Cookie: auth_token=...; Path=/; HttpOnly; SameSite=Lax; Max-Age=86400`
  - JSON 主体：
    ```json
    {
      "token": "jwt_string",
      "user": {
        "id": 1,
        "name": "string",
        "email": "string",
        "isAdmin": true
      }
    }
    ```
- **数据类型**：`LoginRequest` → `LoginResponse`（+ Cookie）
- **功能**：验证邮箱和密码，若成功返回 JWT 令牌及用户信息。若密码存储格式为旧版明文，会自动升级为哈希。

### 1.4 获取当前用户
- **函数名**：`current_user`
- **URL**：`GET /me` 或 `GET /api/me`
- **需要认证**：是（通过 Bearer Token 或 Cookie）
- **接收数据**：无（从请求头解析）
- **返回数据**：
  ```json
  {
    "id": 1,
    "name": "string",
    "email": "string",
    "isAdmin": true
  }
  ```
- **数据类型**：`User`
- **功能**：返回当前登录用户的信息。

### 1.5 登出
- **函数名**：`logout`
- **URL**：`POST /logout` 或 `POST /api/logout`
- **需要认证**：否（但通常会携带 Cookie）
- **接收数据**：无
- **返回数据**：
  - HTTP 头部：`Set-Cookie: auth_token=; ... Max-Age=0`（清除 cookie）
  - JSON 主体：`{ "success": true }`
- **数据类型**：`LogoutResponse`
- **功能**：清除客户端认证 Cookie，实现登出。

---

## 2. 论坛数据初始化

### 2.1 获取论坛全部初始数据
- **函数名**：`bootstrap`
- **URL**：`GET /api/forum/bootstrap`
- **需要认证**：可选（用于获取当前用户的私信）
- **接收数据**：无（可附带认证令牌）
- **返回数据**：
  ```json
  {
    "users": [ ForumUser ],
    "posts": [ ForumPost ],
    "comments": [ ForumComment ],
    "messages": [ ForumMessage ]
  }
  ```
- **数据类型**：`ForumBootstrap`
- **功能**：一次性返回论坛所需的所有初始数据：用户列表、所有帖子、所有评论、以及当前用户的私信记录（未登录时 messages 为空）。

---

## 3. 用户相关

### 3.1 获取用户列表
- **函数名**：`list_users`
- **URL**：`GET /api/forum/users`
- **需要认证**：否
- **接收数据**：无
- **返回数据**：`ForumUser[]`
  ```json
  [
    {
      "id": 1,
      "name": "string",
      "avatar": "CD",
      "isAdmin": true
    }
  ]
  ```
- **数据类型**：`Vec<ForumUser>`
- **功能**：返回所有注册用户的信息，其中 `avatar` 由名字自动生成，管理员固定为 id 1 和 2。

---

## 4. 帖子相关

### 4.1 获取帖子列表
- **函数名**：`list_posts`
- **URL**：`GET /api/forum/posts`
- **需要认证**：可选（会影响 `likedByMe` 字段）
- **接收数据**：查询参数 `?category=string`（可选，留空或 `all` 表示全部）
- **返回数据**：`ForumPost[]`
  ```json
  [
    {
      "id": 1,
      "title": "string",
      "content": "string",
      "author": { ForumUser },
      "category": "general",
      "tags": ["rust", "axum"],
      "createdAt": "2025-01-01T00:00:00+00:00",
      "updatedAt": "2025-01-01T00:00:00+00:00",
      "viewCount": 10,
      "commentCount": 3,
      "likeCount": 5,
      "likedByMe": false,
      "isPinned": false,
      "isLocked": false
    }
  ]
  ```
- **数据类型**：`Vec<ForumPost>`
- **功能**：按置顶优先、时间倒序返回帖子列表，可按分类过滤。

### 4.2 搜索帖子
- **函数名**：`search_posts`
- **URL**：`GET /api/forum/posts/search`
- **需要认证**：可选
- **接收数据**：查询参数 `?q=搜索词`
- **返回数据**：`ForumPost[]`（结构同 4.1）
- **功能**：对帖子标题和内容进行模糊搜索（不区分大小写）。

### 4.3 获取单个帖子详情
- **函数名**：`get_post`
- **URL**：`GET /api/forum/posts/{id}`
- **需要认证**：可选
- **接收数据**：路径参数 `id`（帖子 ID）
- **返回数据**：`ForumPost`（结构同 4.1）
- **功能**：返回帖子详细信息，并自动将 `viewCount` 加 1。

### 4.4 创建帖子
- **函数名**：`create_post`
- **URL**：`POST /api/forum/posts`
- **需要认证**：是
- **接收数据**：
  ```json
  {
    "title": "string",
    "content": "string",
    "category": "string",       // 可选，默认 "general"
    "tags": ["string"]          // 可选，默认 []
  }
  ```
- **返回数据**：`ForumPost`（新创建的帖子对象）
- **功能**：登录用户发布新帖子。

### 4.5 删除帖子
- **函数名**：`delete_post`
- **URL**：`DELETE /api/forum/posts/{id}`
- **需要认证**：是（且为帖子作者或管理员）
- **接收数据**：路径参数 `id`
- **返回数据**：`204 No Content` 成功，否则 `403 Forbidden` 或 `404 Not Found`
- **功能**：删除指定帖子，同时删除关联的评论和点赞。

### 4.6 切换置顶状态
- **函数名**：`toggle_pin`
- **URL**：`PUT /api/forum/posts/{id}/pin`
- **需要认证**：是（管理员）
- **接收数据**：路径参数 `id`
- **返回数据**：更新后的 `ForumPost`
- **功能**：切换帖子的置顶状态，管理员专用。

### 4.7 切换锁定状态
- **函数名**：`toggle_lock`
- **URL**：`PUT /api/forum/posts/{id}/lock`
- **需要认证**：是（管理员）
- **接收数据**：路径参数 `id`
- **返回数据**：更新后的 `ForumPost`
- **功能**：切换帖子的锁定状态，锁定的帖子禁止评论。

### 4.8 点赞/取消点赞
- **函数名**：`toggle_like`
- **URL**：`PUT /api/forum/posts/{id}/like`
- **需要认证**：是
- **接收数据**：路径参数 `id`
- **返回数据**：更新后的 `ForumPost`（包含最新的 `likeCount` 和 `likedByMe`）
- **功能**：当前登录用户对帖子执行点赞或取消点赞操作。

### 4.9 查看自己的所有帖子
- **函数名**：`my_posts`
- **URL**：`GET /api/forum/posts/myposts`
- **需要认证**：是
- **返回数据**：用户所有的 `ForumPost`
- **功能**：查看自己的所有帖子

### 4.10 修改自己的帖子
- **函数名**：`modify_post`
- **URL**：`PUT /api/forum/posts/myposts/modify_post`
- **需要认证**：是
- **接收数据**：修改后的 `ForumPost`
- **功能**：修改自己的帖子
---

## 5. 评论相关

### 5.1 获取帖子的评论列表
- **函数名**：`list_comments`
- **URL**：`GET /api/forum/posts/{id}/comments`
- **需要认证**：否
- **接收数据**：路径参数 `id`（帖子 ID）
- **返回数据**：`ForumComment[]`
  ```json
  [
    {
      "id": 1,
      "postId": 5,
      "author": { ForumUser },
      "content": "string",
      "createdAt": "2025-01-01T00:00:00+00:00"
    }
  ]
  ```
- **功能**：按时间升序返回指定帖子的所有评论。

### 5.2 发表评论
- **函数名**：`create_comment`
- **URL**：`POST /api/forum/posts/{id}/comments`
- **需要认证**：是
- **接收数据**：
  ```json
  {
    "content": "string"
  }
  ```
- **返回数据**：`ForumComment`（新创建的评论）
- **功能**：登录用户在指定帖子下发表评论。若帖子已锁定，返回 `403 Forbidden`。

### 5.3 删除评论
- **函数名**：`delete_comment`
- **URL**：`DELETE /api/forum/comments/{id}`
- **需要认证**：是（且为评论作者或管理员）
- **接收数据**：路径参数 `id`（评论 ID）
- **返回数据**：`204 No Content` 成功
- **功能**：删除指定评论。

---

## 6. 私信相关

### 6.1 获取私信列表
- **函数名**：`list_messages`
- **URL**：`GET /api/forum/messages`
- **需要认证**：是
- **接收数据**：无
- **返回数据**：`ForumMessage[]`
  ```json
  [
    {
      "id": 1,
      "sender": { ForumUser },
      "recipient": { ForumUser },
      "content": "string",
      "createdAt": "2025-01-01T00:00:00+00:00",
      "isRead": false
    }
  ]
  ```
- **功能**：返回与当前用户相关的所有私信（发出的和收到的）。

### 6.2 发送私信
- **函数名**：`send_message`
- **URL**：`POST /api/forum/messages`
- **需要认证**：是
- **接收数据**：
  ```json
  {
    "recipientId": 2,
    "content": "string"
  }
  ```
- **返回数据**：新创建的 `ForumMessage`
- **功能**：向指定用户发送一条私信。

### 6.3 标记单条私信已读
- **函数名**：`mark_message_read`
- **URL**：`PUT /api/forum/messages/{id}/read`
- **需要认证**：是（接收者本人）
- **接收数据**：路径参数 `id`（消息 ID）
- **返回数据**：`204 No Content` 成功
- **功能**：将指定私信标记为已读，仅允许接收者操作。

### 6.4 标记与某用户的全部私信已读
- **函数名**：`mark_conversation_read`
- **URL**：`PUT /api/forum/messages/conversation/{partner_id}/read`
- **需要认证**：是
- **接收数据**：路径参数 `partner_id`（对话对方用户 ID）
- **返回数据**：`204 No Content` 成功
- **功能**：将当前用户与 `partner_id` 之间所有“对方发来且未读”的私信标记为已读。

---