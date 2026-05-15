# DevBit Forum API

基于 Rust 的高性能社区论坛后端服务，采用 [Axum](https://github.com/tokio-rs/axum) 框架构建，提供完整的 RESTful API 接口。

## 项目简介

DevBit Forum API 是一个轻量级、高性能的论坛系统后端，支持用户认证、帖子管理、评论、私信、邮件验证码等核心功能。项目采用现代化的 Rust 技术栈，具有出色的并发处理能力和安全性保障。

## 技术栈

| 类别 | 技术 |
|---|---|
| 框架 | Axum — 基于 Tokio 的高性能 Web 框架 |
| 数据库 | PostgreSQL + SQLx — 类型安全的异步数据库操作 |
| 认证 | JWT (JSON Web Token) — 基于 jsonwebtoken 实现 |
| 密码哈希 | PBKDF2-SHA256 (100,000 次迭代) |
| 邮件服务 | Lettre — 支持 SMTP 的邮件发送 |
| 序列化 | Serde — 高性能序列化/反序列化框架 |
| 时间处理 | Chrono — 日期时间处理库 |

## 功能特性

- ✅ 用户注册与登录
- ✅ JWT 身份认证（24 小时有效期）
- ✅ 邮箱验证码发送（SMTP，10 分钟有效期）
- ✅ 论坛帖子 CRUD 操作
- ✅ 帖子分类与标签系统
- ✅ 全文搜索（标题 + 内容 LIKE 匹配）
- ✅ 浏览量统计（每次访问 +1）
- ✅ 帖子点赞/取消点赞
- ✅ 帖子置顶/锁定（仅管理员）
- ✅ 评论系统（锁定帖子拒绝新评论）
- ✅ 私信系统（发送/已读标记/批量已读）
- ✅ Bootstrap 数据预加载
- ✅ 类型安全的数据库查询
- ✅ 数据库表自动创建（`CREATE TABLE IF NOT EXISTS`）

## 快速开始

### 环境要求

- Rust 1.70+
- PostgreSQL 14+
- SMTP 邮件服务（可选，开发环境可跳过）

### 安装步骤

**1. 克隆项目**

```bash
git clone https://github.com/EpsilonHunter/devbit-tech-backend.git
cd devbit-tech-backend
```

**2. 配置环境变量**

创建 `.env` 文件：

```env
DATABASE_URL=postgres://username:password@localhost/devbit_forum
JWT_SECRET=your_jwt_secret_key_here
SMTP_USERNAME=your_email@qq.com
SMTP_PASSWORD=your_smtp_authorization_code
SMTP_SERVER=smtp.qq.com
SMTP_PORT=465
```

> **注意**：不配置 SMTP 时，验证码仅在开发模式下可用（通过 API 响应体直接返回）。

**3. 编译运行**

```bash
cargo build --release
cargo run --release
```

服务默认运行在 `http://127.0.0.1:7878`。数据库表会在首次启动时自动创建。

## API 接口

所有接口前缀：`/api`

### 认证接口

| 方法 | 路径 | 认证 | 说明 |
|---|---|---|---|
| `POST` | `/api/login` | 无需 | 用户登录，返回 JWT Token |
| `POST` | `/api/logout` | 无需 | 清除认证 Cookie |
| `GET` | `/api/me` | 必需 | 获取当前用户信息 |
| `POST` | `/api/register/send_code` | 无需 | 发送邮箱验证码 |
| `POST` | `/api/register` | 无需 | 注册新用户 |

### 论坛接口

| 方法 | 路径 | 认证 | 说明 |
|---|---|---|---|
| `GET` | `/api/forum/bootstrap` | 可选 | Bootstrap 数据预加载 |
| `GET` | `/api/forum/users` | 无需 | 用户列表 |
| `GET` | `/api/forum/posts` | 可选 | 帖子列表（支持 `?category=`） |
| `GET` | `/api/forum/posts/search` | 可选 | 搜索帖子（`?q=`） |
| `GET` | `/api/forum/posts/:id` | 可选 | 帖子详情（浏览量 +1） |
| `POST` | `/api/forum/posts` | 必需 | 创建帖子 |
| `DELETE` | `/api/forum/posts/:id` | 必需 | 删除帖子（作者/管理员） |
| `PUT` | `/api/forum/posts/:id/pin` | 必需 | 切换置顶（仅管理员） |
| `PUT` | `/api/forum/posts/:id/lock` | 必需 | 切换锁定（仅管理员） |
| `PUT` | `/api/forum/posts/:id/like` | 必需 | 切换点赞 |
| `GET` | `/api/forum/posts/:id/comments` | 无需 | 评论列表 |
| `POST` | `/api/forum/posts/:id/comments` | 必需 | 添加评论 |
| `DELETE` | `/api/forum/comments/:id` | 必需 | 删除评论（作者/管理员） |
| `GET` | `/api/forum/messages` | 必需 | 当前用户私信列表 |
| `POST` | `/api/forum/messages` | 必需 | 发送私信 |
| `PUT` | `/api/forum/messages/:id/read` | 必需 | 标记单条已读 |
| `PUT` | `/api/forum/messages/conversation/:partnerId/read` | 必需 | 批量标记会话已读 |

### 帖子分类

| 值 | 说明 |
|---|---|
| `general` | 综合讨论 |
| `tech` | 技术交流 |
| `devbit` | DevBit 专区 |
| `help` | 求助问答 |
| `showcase` | 作品展示 |
| `announcement` | 公告通知 |

### 详细 API 文档

完整的前端 API 文档请参阅 [前端 doc/API.md](../devbit-tech/frontend/doc/API.md)。

## 项目结构

```
src/
├── main.rs          # 主入口、认证路由、JWT 工具函数、密码哈希
├── forum.rs         # 论坛路由（帖子/评论/私信/搜索/bootstrap）
├── database.rs      # 数据库连接池初始化 & 表自动创建
└── lib.rs           # 模块声明（pub mod forum）
```

## 数据库表

以下表会在首次启动时自动创建：

| 表名 | 说明 |
|---|---|
| `users` | 用户账户 |
| `verify_code` | 邮箱验证码（LOWER(email) 唯一索引） |
| `forum_posts` | 论坛帖子 |
| `forum_comments` | 帖子评论（CASCADE 删除） |
| `forum_messages` | 私信 |
| `forum_post_likes` | 帖子点赞关联 |

## 安全特性

- JWT 令牌有效期 24 小时
- 密码使用 PBKDF2-SHA256 哈希（100,000 次迭代）
- 验证码有效期 10 分钟，使用后立即删除
- Bearer Token + Cookie 双重认证支持
- 登录时自动升级旧格式密码哈希
- 管理员权限检查（id 1 或 2 为管理员）
- 帖子/评论操作权限校验（作者或管理员）

## 开发团队

| 角色 | 成员 |
|---|---|
| 后端开发 | EpsilonHunter |
| 前端开发 | Clearders |

许可证
本项目采用 Apache License 2.0 开源协议。

text
Copyright 2024 EpsilonHunter

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
贡献指南
欢迎提交 Issue 和 Pull Request！

Fork 本仓库

创建特性分支 (git checkout -b feature/AmazingFeature)

提交更改 (git commit -m 'Add some AmazingFeature')

推送到分支 (git push origin feature/AmazingFeature)

创建 Pull Request

联系方式
Issues: GitHub Issues

邮箱: 2043399410@qq.com

<div align="center"> Made with ❤️ by DevBit Team </div>