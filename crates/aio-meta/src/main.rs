#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = aio_meta::Config::from_env_and_dir(None);
    println!(
        "AIO Metadata (Rust); TMDB key {}; language {}",
        config.redacted_key(),
        config.language
    );
    aio_meta::serve_forever(config).await
}
