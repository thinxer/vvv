use anyhow::Result;

use apple_screen_sharing::rfb::srp::{DEFAULT_ITERATIONS, VerifierRecord};
use std::path::PathBuf;
#[derive(clap::Args)]
pub struct PasswordArgs {
    #[arg(long)]
    user: Option<String>,
    #[arg(long)]
    password_file: Option<PathBuf>,
    #[arg(long)]
    srp_file: PathBuf,
    #[arg(long, default_value_t=DEFAULT_ITERATIONS)]
    iterations: u32,
}
pub fn set_password(args: PasswordArgs) -> Result<()> {
    let user = match args.user {
        Some(user) => user,
        None => apple_screen_sharing::setup::current_user()?,
    };
    let password = apple_screen_sharing::setup::read_password(args.password_file.as_deref())?;
    VerifierRecord::generate(&user, &password, args.iterations).save(&args.srp_file)?;
    println!("Saved verifier for {user} in {}", args.srp_file.display());
    Ok(())
}
