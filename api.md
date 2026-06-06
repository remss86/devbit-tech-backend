# DevBit API 文档

> **当前版本**: `v1`  
> **基础路径**: `http://127.0.0.1:7878`  
> **最后更新**: 2026-06-07

---

## 版本说明

| 版本 | 状态   | 说明                           |
|------|--------|--------------------------------|
| v1   | 当前   | 初始版本，覆盖认证、论坛、好友等功能 |

所有 API 路径均以 `/api/` 为前缀（认证端点同时支持无前缀的兼容路径）。版本号通过路径中的 `/api/` 隐含为 v1。

---

## 通用约定

### 请求格式
- 所有请求与响应均使用 **JSON** 格式（除非特别说明，如头像上传使用 `multipart/form-data`，头像获取直接返回二进制）。
- 请求头 `Content-Type: application/json`（JSON 请求）。
- 字符编码统一使用 UTF-8。

### 认证机制
- 用户认证通过 **JWT**（JSON Web Token）实现。
- 客户端可通过以下两种方式传递令牌：
  1. **Authorization Header**: `Authorization: Bearer <token>`
  2. **Cookie**: 登录时服务器通过 `Set-Cookie` 下发 `auth_token` cookie（`HttpOnly; SameSite=Lax; Max-Age=86400`）。
- 标注为"需认证"的接口必须提供有效 JWT。
- 标注为"可选认证"的接口：未登录也可调用，但部分字段（如 `likedByMe`）会返回默认值。

### 速率限制

| 端点类型     | 限制         | 说明                  |
|-------------|-------------|----------------------|
| 认证相关端点  | 5 req / 60s | 注册、登录、发送验证码    |
| 通用端点     | 10 req / 60s | 其他所有 API           |

- 超出限制返回 `429 Too Many Requests`，响应头 `Retry-After: 60`。
- 限制基于客户端 IP 的滑动时间窗口。

### 错误响应格式

```json
{
  "error": "错误描述信息"
}
```

| HTTP 状态码 | 含义                          |
|------------|-------------------------------|
| 200        | 请求成功                        |
| 204        | 请求成功，无返回内容（如删除操作）   |
| 400        | 请求参数有误                     |
| 401        | 未认证或认证失败                  |
| 403        | 权限不足                        |
| 404        | 资源不存在                      |
| 409        | 资源冲突（如邮箱已注册）           |
| 413        | 上传文件过大                     |
| 415        | 不支持的媒体类型                  |
| 429        | 请求频率超限                     |
| 500        | 服务器内部错误                   |
| 502        | 上游服务错误（如 SMTP 发送失败）   |

---

## 数据模型（v1）

### User（用户 - 认证系统）

```json
{
  "id": 1,
  "name": "string",
  "email": "string",
  "avatarUrl": "string | null",
  "isAdmin": true
}
```

### ForumUser（论坛用户）

```json
{
  "id": 1,
  "name": "string",
  "avatar": "CD",
  "avatarUrl": "string | null",
  "isAdmin": true
}
```

> `avatar` 为根据用户名自动生成的两位字符头像标识（管理员 id=1 为 "CD"，id=2 为 "EH"）。  
> `avatarUrl` 为用户上传的自定义头像相对路径，如 `/api/avatars/uuid.png`。

### ForumPost（帖子）

```json
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
```

### ForumComment（评论）

```json
{
  "id": 1,
  "postId": 5,
  "author": { ForumUser },
  "content": "string",
  "createdAt": "2025-01-01T00:00:00+00:00"
}
```

### ForumMessage（私信）

```json
{
  "id": 1,
  "sender": { ForumUser },
  "recipient": { ForumUser },
  "content": "string",
  "createdAt": "2025-01-01T00:00:00+00:00",
  "isRead": false
}
```

### ForumBootstrap（论坛初始化数据包）

```json
{
  "users": [ ForumUser ],
  "posts": [ ForumPost ],
  "comments": [ ForumComment ],
  "messages": [ ForumMessage ]
}
```

### FriendInfo（好友信息）

```json
{
  "user": { ForumUser },
  "createdAt": "2025-01-01T00:00:00+00:00"
}
```


---

## API 端点

### 1. 身份认证 (Authentication)

#### 1.1 发送验证码

- **端点**: `POST /api/register/send_code`（兼容: `POST /register/send_code`）
- **版本**: v1
- **认证**: 无需
- **速率限制**: 严格（5 req / 60s）

**请求体**:
```json
{
  "email": "user@example.com"
}
```

**成功响应** `200`:
```json
{
  "message": "Verification code sent. Please check your email.",
  "expiresInSeconds": 600,
  "developmentCode": "123456"
}
```

- `developmentCode`: 仅当未配置 SMTP 环境变量（`SMTP_USERNAME` 为空）时返回，方便开发测试。
- 验证码为 6 位数字，有效期 10 分钟。

**错误响应**:
- `400` — 邮箱格式无效

---

#### 1.2 用户注册

- **端点**: `POST /api/register`（兼容: `POST /register`）
- **版本**: v1
- **认证**: 无需
- **速率限制**: 严格（5 req / 60s）

**请求体**:
```json
{
  "name": "张三",
  "email": "user@example.com",
  "code": "123456",
  "password": "your_password"
}
```

**成功响应** `200`:
```json
{
  "id": 1,
  "name": "张三",
  "email": "user@example.com"
}
```

**错误响应**:
- `400` — 必填字段为空
- `401` — 验证码无效或已过期
- `409` — 邮箱已被注册

> 密码使用 PBKDF2-SHA256 哈希存储（100,000 次迭代，32 字节输出）。注册成功后对应验证码立即清除。

---

#### 1.3 登录

- **端点**: `POST /api/login`（兼容: `POST /login`）
- **版本**: v1
- **认证**: 无需
- **速率限制**: 严格（5 req / 60s）

**请求体**:
```json
{
  "email": "user@example.com",
  "password": "your_password"
}
```

**成功响应** `200`:
```json
{
  "token": "eyJhbGciOiJIUzI1NiIs...",
  "user": {
    "id": 1,
    "name": "张三",
    "email": "user@example.com",
    "avatarUrl": null,
    "isAdmin": false
  }
}
```

**响应头**:
```
Set-Cookie: auth_token=<jwt>; Path=/; HttpOnly; SameSite=Lax; Max-Age=86400
```

- JWT 有效期 24 小时。
- 若数据库中密码存储为旧版明文格式，登录成功时自动升级为 PBKDF2-SHA256 哈希。

**错误响应**:
- `401` — 邮箱不存在或密码错误

---

#### 1.4 获取当前用户

- **端点**: `GET /api/me`（兼容: `GET /me`）
- **版本**: v1
- **认证**: 必需
- **速率限制**: 通用（10 req / 60s）

**成功响应** `200`:
```json
{
  "id": 1,
  "name": "张三",
  "email": "user@example.com",
  "avatarUrl": "/api/avatars/abc123.png",
  "isAdmin": false
}
```

**错误响应**:
- `401` — 未提供有效认证令牌



---

#### 1.5 上传头像

- **端点**: `POST /api/me/avatar`（兼容: `POST /me/avatar`）
- **版本**: v1
- **认证**: 必需
- **速率限制**: 通用（10 req / 60s）
- **Content-Type**: `multipart/form-data`

**请求参数**:
| 字段名  | 类型 | 说明                         |
|---------|------|------------------------------|
| avatar  | File | 图片文件，字段名必须为 `avatar` |

**约束**:
- 允许的扩展名: `png`, `jpg`, `jpeg`, `gif`, `webp`
- 最大文件大小: 2 MB
- 上传新头像会自动删除旧头像文件

**成功响应** `200`:
```json
{
  "id": 1,
  "name": "张三",
  "email": "user@example.com",
  "avatarUrl": "/api/avatars/uuid.png",
  "isAdmin": false
}
```

**错误响应**:
- `400` — 未提供文件或文件名为空
- `401` — 未认证
- `413` — 文件超过 2 MB
- `415` — 不支持的文件类型

---

#### 1.6 获取头像文件

- **端点**: `GET /api/avatars/{filename}`（兼容: `GET /avatars/{filename}`）
- **版本**: v1
- **认证**: 无需
- **速率限制**: 通用（10 req / 60s）

**成功响应** `200`: 返回图片二进制数据，`Content-Type` 根据扩展名自动设置。

**错误响应**:
- `404` — 文件不存在或路径包含非法字符（防目录遍历）

---

#### 1.7 登出

- **端点**: `POST /api/logout`（兼容: `POST /logout`）
- **版本**: v1
- **认证**: 无需
- **速率限制**: 通用（10 req / 60s）

**成功响应** `200`:
```json
{
  "success": true
}
```

**响应头**:
```
Set-Cookie: auth_token=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT
```

---

### 2. 论坛 - 初始化

#### 2.1 获取论坛全部初始数据 (Bootstrap)

- **端点**: `GET /api/forum/bootstrap`
- **版本**: v1
- **认证**: 可选
- **速率限制**: 通用（10 req / 60s）

**成功响应** `200`:
```json
{
  "users": [ ForumUser ],
  "posts": [ ForumPost ],
  "comments": [ ForumComment ],
  "messages": [ ForumMessage ]
}
```

- 一次性返回论坛所需全部初始数据。
- `messages` 字段：已登录返回当前用户的所有私信，未登录返回空数组 `[]`。
- 帖子按置顶优先 + 创建时间倒序排列。

---

### 3. 论坛 - 用户

#### 3.1 获取所有用户列表

- **端点**: `GET /api/forum/users`
- **版本**: v1
- **认证**: 无需
- **速率限制**: 通用（10 req / 60s）

**成功响应** `200`: `ForumUser[]`（按 id 升序排列）

---

#### 3.2 搜索用户

- **端点**: `GET /api/forum/users/search?q={keyword}`
- **版本**: v1
- **认证**: 必需
- **速率限制**: 通用（10 req / 60s）

**查询参数**:
| 参数 | 类型   | 必需 | 说明     |
|------|--------|------|----------|
| q    | string | 是   | 搜索关键词 |

**成功响应** `200`: `ForumUser[]`（最多 20 条，按前缀匹配优先 + 匹配位置 + 字母序排列）

**错误响应**:
- `401` — 未认证

> 搜索转义 LIKE 通配符（`%`, `_`, `\`），防止注入。

---

### 4. 论坛 - 帖子

#### 4.1 获取帖子列表

- **端点**: `GET /api/forum/posts?category={category}`
- **版本**: v1
- **认证**: 可选
- **速率限制**: 通用（10 req / 60s）

**查询参数**:
| 参数     | 类型   | 必需 | 说明                         |
|----------|--------|------|------------------------------|
| category | string | 否   | 分类过滤。空或 `all` 返回全部 |

**成功响应** `200`: `ForumPost[]`（按置顶优先 + 创建时间倒序）

---

#### 4.2 获取单个帖子详情

- **端点**: `GET /api/forum/posts/{id}`
- **版本**: v1
- **认证**: 可选
- **速率限制**: 通用（10 req / 60s）

**路径参数**:
| 参数 | 类型 | 说明    |
|------|------|---------|
| id   | int  | 帖子 ID |

- 访问时 `viewCount` 自动 +1。

**成功响应** `200**: `ForumPost`

**错误响应**:
- `404` — 帖子不存在

---

#### 4.3 搜索帖子

- **端点**: `GET /api/forum/posts/search?q={keyword}`
- **版本**: v1
- **认证**: 可选
- **速率限制**: 通用（10 req / 60s）

**查询参数**:
| 参数 | 类型   | 必需 | 说明     |
|------|--------|------|----------|
| q    | string | 是   | 搜索关键词 |

- 对标题和内容进行不区分大小写的模糊匹配。
- 空搜索词或纯空格返回空数组。

**成功响应** `200**: `ForumPost[]`

---

#### 4.4 创建帖子

- **端点**: `POST /api/forum/posts`
- **版本**: v1
- **认证**: 必需
- **速率限制**: 通用（10 req / 60s）

**请求体**:
```json
{
  "title": "帖子标题",
  "content": "帖子内容（支持 Markdown）",
  "category": "general",
  "tags": ["rust", "web"]
}
```

| 字段     | 类型     | 必需 | 默认值      |
|----------|----------|------|-------------|
| title    | string   | 是   | —           |
| content  | string   | 是   | —           |
| category | string   | 否   | `"general"` |
| tags     | string[] | 否   | `[]`        |

**成功响应** `200`: `ForumPost`（新创建的帖子）

**错误响应**:
- `401` — 未认证

---

#### 4.5 删除帖子

- **端点**: `DELETE /api/forum/posts/{id}`
- **版本**: v1
- **认证**: 必需（帖子作者或管理员）
- **速率限制**: 通用（10 req / 60s）

**成功响应**: `204 No Content`

**错误响应**:
- `401` — 未认证
- `403` — 非作者且非管理员
- `404` — 帖子不存在

> 删除帖子时关联的评论和点赞会级联删除（数据库外键 `ON DELETE CASCADE`）。

---

#### 4.6 切换置顶状态

- **端点**: `PUT /api/forum/posts/{id}/pin`
- **版本**: v1
- **认证**: 必需（管理员）
- **速率限制**: 通用（10 req / 60s）

**成功响应** `200**: `ForumPost`（更新后的帖子）

**错误响应**:
- `401` — 未认证
- `403` — 非管理员
- `404` — 帖子不存在

---

#### 4.7 切换锁定状态

- **端点**: `PUT /api/forum/posts/{id}/lock`
- **版本**: v1
- **认证**: 必需（管理员）
- **速率限制**: 通用（10 req / 60s）

- 锁定的帖子禁止发表新评论。

**成功响应** `200**: `ForumPost`（更新后的帖子）

**错误响应**:
- `401` — 未认证
- `403` — 非管理员
- `404` — 帖子不存在

---

#### 4.8 点赞 / 取消点赞

- **端点**: `PUT /api/forum/posts/{id}/like`
- **版本**: v1
- **认证**: 必需
- **速率限制**: 通用（10 req / 60s）

- 切换操作：若已点赞则取消，若未点赞则点赞。

**成功响应** `200**: `ForumPost`（包含最新的 `likeCount` 和 `likedByMe`）

**错误响应**:
- `401` — 未认证
- `404` — 帖子不存在

---

#### 4.9 获取我的帖子

- **端点**: `GET /api/forum/posts/myposts`
- **版本**: v1
- **认证**: 必需
- **速率限制**: 通用（10 req / 60s）

**成功响应** `200**: `ForumPost[]`（当前用户的所有帖子）

**错误响应**:
- `401` — 未认证

---

#### 4.10 修改帖子

- **端点**: `PUT /api/forum/posts/myposts/modify_post/{id}`
- **版本**: v1
- **认证**: 必需（帖子作者或管理员）
- **速率限制**: 通用（10 req / 60s）

**路径参数**:
| 参数 | 类型 | 说明    |
|------|------|---------|
| id   | int  | 帖子 ID |

**请求体**:
```json
{
  "id": 1,
  "title": "string",
  "content": "修改后的内容",
  "author": { ForumUser },
  "category": "general",
  "tags": ["rust"],
  "createdAt": "...",
  "updatedAt": "...",
  "viewCount": 10,
  "commentCount": 3,
  "likeCount": 5,
  "likedByMe": false,
  "isPinned": false,
  "isLocked": false
}
```

> 实际仅 `content` 字段会被更新，`updatedAt` 自动设为当前时间。其余字段在请求体中可传但不生效。

**成功响应** `200`（无响应体）

**错误响应**:
- `401` — 未认证
- `403` — 非作者且非管理员
- `404` — 帖子不存在

---

### 5. 论坛 - 评论

#### 5.1 获取帖子的评论列表

- **端点**: `GET /api/forum/posts/{id}/comments`
- **版本**: v1
- **认证**: 无需
- **速率限制**: 通用（10 req / 60s）

**成功响应** `200**: `ForumComment[]`（按创建时间升序）

---

#### 5.2 发表评论

- **端点**: `POST /api/forum/posts/{id}/comments`
- **版本**: v1
- **认证**: 必需
- **速率限制**: 通用（10 req / 60s）

**请求体**:
```json
{
  "content": "评论内容"
}
```

**成功响应** `200**: `ForumComment`

**错误响应**:
- `401` — 未认证
- `403` — 帖子已锁定
- `404` — 帖子不存在

---

#### 5.3 删除评论

- **端点**: `DELETE /api/forum/comments/{id}`
- **版本**: v1
- **认证**: 必需（评论作者或管理员）
- **速率限制**: 通用（10 req / 60s）

**成功响应**: `204 No Content`

**错误响应**:
- `401` — 未认证
- `403` — 非作者且非管理员
- `404` — 评论不存在

---

### 6. 论坛 - 私信

#### 6.1 获取私信列表

- **端点**: `GET /api/forum/messages`
- **版本**: v1
- **认证**: 必需
- **速率限制**: 通用（10 req / 60s）

**成功响应** `200**: `ForumMessage[]`（与当前用户相关的所有私信，按时间升序）

---

#### 6.2 发送私信

- **端点**: `POST /api/forum/messages`
- **版本**: v1
- **认证**: 必需
- **速率限制**: 通用（10 req / 60s）

**请求体**:
```json
{
  "recipientId": 2,
  "content": "私信内容"
}
```

**成功响应** `200**: `ForumMessage`

**错误响应**:
- `401` — 未认证
- `404` — 接收者不存在

---

#### 6.3 标记单条私信已读

- **端点**: `PUT /api/forum/messages/{id}/read`
- **版本**: v1
- **认证**: 必需（接收者本人）
- **速率限制**: 通用（10 req / 60s）

**成功响应**: `204 No Content`

**错误响应**:
- `401` — 未认证
- `404` — 消息不存在或当前用户非接收者

---

#### 6.4 标记与某用户的全部私信已读

- **端点**: `PUT /api/forum/messages/conversation/{partnerId}/read`
- **版本**: v1
- **认证**: 必需
- **速率限制**: 通用（10 req / 60s）

- 将对方发来且未读的所有私信标记为已读。

**成功响应**: `204 No Content`

**错误响应**:
- `401` — 未认证
- `404` — 无符合条件的未读消息

---

### 7. 论坛 - 好友

#### 7.1 获取好友列表

- **端点**: `GET /api/forum/friends`
- **版本**: v1
- **认证**: 必需
- **速率限制**: 通用（10 req / 60s）

**成功响应** `200**: `FriendInfo[]`（按好友名字母序排列）

---

#### 7.2 添加好友

- **端点**: `POST /api/forum/friends`
- **版本**: v1
- **认证**: 必需
- **速率限制**: 通用（10 req / 60s）

**请求体**:
```json
{
  "friendId": 3
}
```

**成功响应** `200**: `FriendInfo`

**错误响应**:
- `400` — 尝试添加自己为好友
- `401` — 未认证
- `404` — 目标用户不存在

> 好友关系为单向（A 添加 B 为好友，不代表 B 添加 A）。重复添加会更新 `createdAt`。

---

#### 7.3 删除好友

- **端点**: `DELETE /api/forum/friends/{friendId}`
- **版本**: v1
- **认证**: 必需
- **速率限制**: 通用（10 req / 60s）

**成功响应**: `204 No Content`

**错误响应**:
- `401` — 未认证
- `404` — 好友关系不存在

---

## 附录

### A. 环境变量

| 变量名          | 必需           | 说明                       | 默认值                                      |
|-----------------|----------------|----------------------------|---------------------------------------------|
| `DATABASE_URL`  | 否             | PostgreSQL 连接字符串        | `postgres://postgres:@localhost:5432/users` |
| `JWT_SECRET`    | 否（生产必需）  | JWT 签名密钥                | `devbit-local-secret`                       |
| `SMTP_USERNAME` | 否             | SMTP 发件邮箱地址            | —                                           |
| `SMTP_PASSWORD` | 条件必需        | SMTP 邮箱密码/授权码         | —                                           |
| `SMTP_SERVER`   | 否             | SMTP 服务器地址              | `smtp.qq.com`                               |
| `SMTP_PORT`     | 否             | SMTP 服务器端口              | `465`                                       |

### B. 密码安全

- 密码使用 **PBKDF2-SHA256** 哈希存储。
- 参数：100,000 次迭代，32 字节输出。
- 存储格式：`pbkdf2-sha256$<iterations>$<base64_salt>$<base64_hash>`。
- 旧版明文密码在用户登录时自动升级为哈希格式。

### C. 数据库表结构

| 表名                | 说明     |
|---------------------|----------|
| `users`             | 用户账户  |
| `verify_code`       | 邮箱验证码 |
| `forum_posts`       | 论坛帖子  |
| `forum_comments`    | 帖子评论  |
| `forum_messages`    | 私信      |
| `forum_post_likes`  | 帖子点赞  |
| `friends`           | 好友关系  |

### D. 头像系统

- 默认头像由用户名自动生成两位大写字母标识。
- 自定义头像上传至 `uploads/avatars/` 目录。
- 头像文件通过 `/api/avatars/{filename}` 端点获取。
- URL 存储格式：`/api/avatars/{uuid}.{ext}`。
- 上传新头像时自动删除旧文件。
