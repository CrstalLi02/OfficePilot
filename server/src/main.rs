#[tokio::main]
async fn main() -> anyhow::Result<()> {
    walioffice::cli_main().await
}
