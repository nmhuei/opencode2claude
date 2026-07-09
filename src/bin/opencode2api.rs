#[path = "../main.rs"]
mod cli_entry;

#[tokio::main]
async fn main() {
    cli_entry::run_cli().await;
}
