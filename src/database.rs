use sqlx::{Pool, Postgres};
pub async fn db_init() -> Result<Pool<Postgres>, sqlx::Error> {
    let pool = Pool::<Postgres>::connect("postgres://postgres:@localhost:5432/postgres").await?;
    println!("Hello, world!");
    match sqlx::query("CREATE DATABASE users").execute(&pool).await {
        Ok(_) => println!("数据库users创建成功."),
        Err(_) => println!("数据库users已存在."),
    }
    let pool = Pool::<Postgres>::connect("postgres://postgres:@localhost:5432/users").await?;
    match sqlx::query(
        "CREATE TABLE IF NOT EXISTS users (
            id SERIAL PRIMARY KEY,
            name TEXT NOT NULL,
            email TEXT NOT NULL UNIQUE,
            password VARCHAR(255) NOT NULL DEFAULT ''
        )",
    )
    .execute(&pool)
    .await
    {
        Ok(_) => println!("表users创建成功."),
        Err(e) => println!("表users: {}", e),
    }
    match sqlx::query(
        "CREATE TABLE IF NOT EXISTS verify_code (email TEXT NOT NULL, code VARCHAR(6) NOT NULL)",
    )
    .execute(&pool)
    .await
    {
        Ok(_) => println!("表verify_code创建成功."),
        Err(e) => println!("表verify_code: {}", e),
    }

    // Forum tables
    match sqlx::query(
        "CREATE TABLE IF NOT EXISTS forum_posts (
            id SERIAL PRIMARY KEY,
            title TEXT NOT NULL,
            content TEXT NOT NULL,
            author_id INT NOT NULL REFERENCES users(id),
            category VARCHAR(32) NOT NULL DEFAULT 'general',
            tags TEXT[] NOT NULL DEFAULT '{}',
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            view_count INT NOT NULL DEFAULT 0,
            is_pinned BOOLEAN NOT NULL DEFAULT false,
            is_locked BOOLEAN NOT NULL DEFAULT false
        )",
    )
    .execute(&pool)
    .await
    {
        Ok(_) => println!("表forum_posts创建成功."),
        Err(e) => println!("表forum_posts: {}", e),
    }

    match sqlx::query(
        "CREATE TABLE IF NOT EXISTS forum_comments (
            id SERIAL PRIMARY KEY,
            post_id INT NOT NULL REFERENCES forum_posts(id) ON DELETE CASCADE,
            author_id INT NOT NULL REFERENCES users(id),
            content TEXT NOT NULL,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )",
    )
    .execute(&pool)
    .await
    {
        Ok(_) => println!("表forum_comments创建成功."),
        Err(e) => println!("表forum_comments: {}", e),
    }

    match sqlx::query(
        "CREATE TABLE IF NOT EXISTS forum_messages (
            id SERIAL PRIMARY KEY,
            sender_id INT NOT NULL REFERENCES users(id),
            recipient_id INT NOT NULL REFERENCES users(id),
            content TEXT NOT NULL,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            is_read BOOLEAN NOT NULL DEFAULT false
        )",
    )
    .execute(&pool)
    .await
    {
        Ok(_) => println!("表forum_messages创建成功."),
        Err(e) => println!("表forum_messages: {}", e),
    }

    match sqlx::query(
        "CREATE TABLE IF NOT EXISTS forum_post_likes (
            post_id INT NOT NULL REFERENCES forum_posts(id) ON DELETE CASCADE,
            user_id INT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            PRIMARY KEY (post_id, user_id)
        )",
    )
    .execute(&pool)
    .await
    {
        Ok(_) => println!("表forum_post_likes创建成功."),
        Err(e) => println!("表forum_post_likes: {}", e),
    }

    Ok(pool)
}
