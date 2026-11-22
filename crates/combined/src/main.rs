use vvv_remote as server;
#[path = "../../apple/cli.rs"]
mod cli;
fn main() -> anyhow::Result<()> {
    cli::run()
}
