# DevBit Tech Backend

The Rust/Axum API for DevBit Tech. It provides account registration and login, avatar uploads, forum posts and comments, direct messages, friends, health checks, and real-time WebSocket notifications.

The detailed HTTP contract is in [api.md](./api.md).

## Requirements

- Rust 1.88 or newer
- PostgreSQL 14 or newer

## Quick start

1. Copy `.env.example` to `.env`.
2. Create the PostgreSQL database referenced by `DATABASE_URL`.
3. Start the service:

```bash
cargo run
```

By default the server listens on `127.0.0.1:7878`. Change `BIND_ADDR` when a different interface or port is required.

The service applies the versioned SQLx migrations in `migrations/` at startup. Migrations are idempotent with the schema used by older releases, so an existing installation can be upgraded in place.

## Environment variables

| Variable | Default | Purpose |
| --- | --- | --- |
| `NODE_ENV` | unset (local debug mode) | Use `development` or `test` locally and `production` in production. |
| `BIND_ADDR` | `127.0.0.1:7878` | Socket address on which Axum listens. |
| `DATABASE_URL` | `postgres://postgres:@localhost:5432/users` | PostgreSQL connection URL. |
| `JWT_SECRET` | `devbit-local-secret` in debug development | JWT signing secret. A non-empty, non-default value is mandatory when `NODE_ENV=production` and for every release build. |
| `SMTP_USERNAME` | unset | SMTP sender address. It may be unset with explicit `development`/`test` mode (or unset `NODE_ENV` in a debug build), where the generated code is returned in JSON; otherwise registration returns `503`. |
| `SMTP_PASSWORD` | unset | SMTP password or application token; required with `SMTP_USERNAME`. |
| `SMTP_SERVER` | `smtp.qq.com` | SMTP relay host. |
| `SMTP_PORT` | `465` | SMTP relay port. |
| `RUST_LOG` | `info` | Tracing filter. |

Generate a deployment secret with a secure random source. For example:

```bash
openssl rand -base64 48
```

Local debug builds remain easy to run with the example secret. A release binary deliberately refuses to start with a missing, empty, or default `JWT_SECRET`, even if `NODE_ENV` was accidentally omitted.

Verification codes are included in responses only when `NODE_ENV` is explicitly `development`/`test`, or when `NODE_ENV` is unset in a debug build. They are never exposed by a release build with an unset environment.

## Project layout

```text
src/
  main.rs        process startup, tracing, bind, and serve
  config.rs      environment parsing and production validation
  server.rs      router construction, health, and HTTP middleware
  account.rs     registration, login, current user, and avatars
  auth.rs        JWT, cookies, administrator mapping, and password hashing
  forum.rs       forum, messages, and friends
  ws.rs          authenticated WebSocket connections and presence
  rate_limit.rs  trusted-proxy-aware sliding-window rate limiting
  database.rs    PostgreSQL pool and SQLx migration runner
migrations/      versioned PostgreSQL schema migrations
```

## Authentication

Login returns the JWT in the existing JSON response and sets it in the `auth_token` HttpOnly cookie. REST endpoints accept either `Authorization: Bearer <token>` or that cookie.

WebSocket clients can authenticate in either of two compatible ways:

- let the browser include the `auth_token` cookie on the `/ws` or `/api/ws` upgrade; the server immediately emits `{"type":"auth_ok","user_id":...}`;
- send `{"type":"auth","token":"..."}` as the first application message.

Multiple tabs share one presence transition: only the first connection broadcasts `user_online`, and `user_offline` is broadcast after the final connection closes.

## Reverse proxy and rate limiting

The service trusts `X-Real-IP` and then `X-Forwarded-For` only when the TCP peer is a loopback address, which matches the repository's local Nginx deployment. Forwarding headers from direct non-loopback clients are ignored.

When Nginx runs on the same host, configure it to overwrite the client headers, for example:

```nginx
proxy_set_header X-Real-IP $remote_addr;
proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
```

Authentication endpoints are limited to 5 requests per 60 seconds per client IP. Other REST endpoints are limited to 10 requests per 60 seconds. Health and WebSocket upgrade routes are not rate limited.

## Quality checks

Run the same checks expected before merging:

```bash
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo check --all-targets
```

## License

Apache-2.0. See [LICENSE](./LICENSE).
