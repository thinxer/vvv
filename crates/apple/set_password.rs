use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use wayshare::rfb::srp::{DEFAULT_ITERATIONS, VerifierRecord};
#[derive(Parser)]
struct Args {
    #[arg(long)]
    user: Option<String>,
    #[arg(long)]
    password_file: Option<PathBuf>,
    #[arg(long)]
    srp_file: PathBuf,
    #[arg(long, default_value_t=DEFAULT_ITERATIONS)]
    iterations: u32,
}
fn main() -> Result<()> {
    let args = Args::parse();
    let user = match args.user {
        Some(user) => user,
        None => wayshare::setup::current_user()?,
    };
    let password = wayshare::setup::read_password(args.password_file.as_deref())?;
    VerifierRecord::generate(&user, &password, args.iterations).save(&args.srp_file)?;
    println!("Saved verifier for {user} in {}", args.srp_file.display());
    Ok(())
}
