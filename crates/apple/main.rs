mod cli;
#[path = "service.rs"]
mod server;
fn main() -> anyhow::Result<()> {
    cli::run()
}
