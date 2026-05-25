```markdown
# DevBit Tech Backend

Rust 后端服务，为 DevBit Tech 提供 RESTful API，包括用户认证、论坛等功能。

## 技术栈

- **语言**: Rust (edition 2024)
- **Web 框架**: [Axum](https://github.com/tokio-rs/axum)
- **数据库**: PostgreSQL + [SQLx](https://github.com/launchbadge/sqlx) (异步)
- **认证**: JWT + PBKDF2-SHA256 密码哈希
- **邮件**: [Lettre](https://github.com/lettre/lettre) (SMTP)
- **其他**: `dotenv`, `chrono`, `base64`, `rand`, `sha2`, `hmac`, `serde`

## 项目结构

```
devbit-tech-backend/
├── Cargo.toml
├── .env.example          # 环境变量模板
├── src/
│   ├── main.rs           # 入口、认证路由
│   ├── forum.rs          # 论坛相关路由与业务逻辑
│   ├── database.rs       # 数据库连接池与表初始化
│   └── lib.rs            # 模块声明
```

## 前置要求

- Rust 1.75+（推荐最新稳定版）
- PostgreSQL 14+
- SMTP 邮件服务（开发环境可跳过，详见[环境变量说明](#环境变量)）

## 构建与运行

### 1. 克隆仓库

```bash
git clone <your-repo-url>
cd devbit-tech-backend
```

### 2. 配置环境变量

复制 `.env.example` 并重命名为 `.env`，按需修改：

```bash
cp .env.example .env
```

### 3. 启动数据库

确保 PostgreSQL 正在运行，并创建一个数据库供项目使用（如 `devbit`）。

### 4. 运行服务

```bash
# 开发模式（推荐）
cargo run

# 生产模式（编译优化）
cargo run --release
```

服务将监听 `127.0.0.1:7878`。

## 环境变量

| 变量名               | 必需？   | 默认值               | 说明                                             |
|----------------------|----------|----------------------|--------------------------------------------------|
| `DATABASE_URL`       | **是**   | -                    | PostgreSQL 连接字符串，格式：`postgres://user:password@localhost/dbname` |
| `JWT_SECRET`         | 否       | `devbit-local-secret` | JWT 签名密钥（生产环境务必设置为复杂随机字符串）   |
| `SMTP_USERNAME`      | 否       | -                    | 发件邮箱地址（留空则跳过真实邮件发送，仅生成验证码） |
| `SMTP_PASSWORD`      | 否       | -                    | 发件邮箱密码/授权码                                |
| `SMTP_SERVER`        | 否       | `smtp.qq.com`        | SMTP 服务器地址                                |
| `SMTP_PORT`          | 否       | `465`                | SMTP 端口                                        |
| `NODE_ENV`           | 否       | `development`        | 若为 `production`，则不在 API 响应中返回开发环境验证码 |

> **开发提示**：若不设置 `SMTP_USERNAME`，发送验证码接口会将验证码直接返回在响应中（`development_code` 字段），方便调试。

## 数据库表（自动初始化）

服务启动时会自动创建以下表（若不存在）：

- **users** – 用户账户
  - `id SERIAL PRIMARY KEY`
  - `name VARCHAR`
  - `email VARCHAR UNIQUE`
  - `password TEXT` (PBKDF2-SHA256 格式)
  - `created_at TIMESTAMPTZ`
- **verify_code** – 邮箱验证码
  - `email VARCHAR`
  - `code VARCHAR`
  - `expires_at TIMESTAMPTZ`
- **论坛相关表**（由 `forum.rs` 管理，如 `posts`, `comments` 等）

## API 端点

所有路径均支持带前缀 `/api/` 的版本（如 `/api/register`），用于反向代理转发。

| 方法   | 路径                        | 说明                    | 认证需求 |
|--------|-----------------------------|-------------------------|----------|
| POST   | `/register/send_code`       | 发送邮箱验证码           | 无       |
| POST   | `/register`                 | 用户注册                | 验证码   |
| POST   | `/login`                    | 用户登录，设置 Cookie    | 无       |
| GET    | `/me`                       | 获取当前登录用户信息     | JWT      |
| POST   | `/logout`                   | 登出，清除 Cookie       | JWT      |
| GET    | `/posts` (等)               | 论坛相关接口（见 forum 模块） | 视接口而定 |

### 注册流程

1. `POST /register/send_code` – 提供 `{"email": "user@example.com"}`，系统发送 6 位数字验证码。
2. `POST /register` – 提供 `{"name": "用户名", "email": "...", "password": "...", "code": "验证码"}` 完成注册。

### 登录与认证

- 登录成功后返回 JWT token，并同时设置名为 `auth_token` 的 HttpOnly Cookie。
- 后续请求可通过以下两种方式之一传递身份：
  - Cookie：自动随请求发送 `auth_token`。
  - Header：`Authorization: Bearer <token>`

### 管理员逻辑

当前代码中 `user_id == 1 || user_id == 2` 被视作管理员（仅用于前端 UI 控制，非严格权限系统）。

## 部署架构

典型生产环境部署（与前端配合）：

```
                 ┌─────────────┐
                 │   Nginx     │
                 │ :80 / :443  │
                 └──────┬──────┘
          /              │            /api/
 ┌──────────────────┐    │  ┌──────────────────────┐
 │  Nuxt 3 Frontend │◄───┴──►│  Rust Backend (Axum) │
 │  (localhost:3000) │       │  (localhost:7878)    │
 │                   │       │  PostgreSQL 数据库    │
 └──────────────────┘       └──────────────────────┘
```

Nginx 将 `/api/` 请求代理到后端 `127.0.0.1:7878`，其余静态资源及页面请求代理到前端 `127.0.0.1:3000`。

## 开发相关

- 密码哈希使用 PBKDF2-SHA256（10万次迭代），哈希格式：`pbkdf2-sha256$iterations$base64salt$base64hash`。
- 验证码有效期 10 分钟。
- JWT 令牌有效期 24 小时。

## 许可证

[Apache License](./LICENSE)
```