//! `cc-admin`: one-off admin tasks on the server host (create her account, set the cap).
//! Run on the host only; reads DATABASE_URL from the environment.

use anyhow::Context;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Career Compass admin")]
struct Cli {
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    database_url: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run pending migrations.
    Migrate,
    /// Create an account. She sets her own password on her first approved device.
    CreateUser {
        email: String,
        #[arg(long, default_value = "seeker")]
        role: String,
    },
    /// Set the monthly cap in EUR.
    SetCap { eur: f64 },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let pool = cc_db::connect(&cli.database_url).await.context("connect")?;
    cc_db::migrate(&pool).await?;
    match cli.cmd {
        Cmd::Migrate => println!("migrations up to date"),
        Cmd::CreateUser { email, role } => {
            let role = cc_core::Role::parse(&role).context("role must be seeker or admin")?;
            let id = cc_db::create_user(&pool, &email, role).await?;
            println!("created user {id}");
        }
        Cmd::SetCap { eur } => {
            anyhow::ensure!(eur.is_finite() && eur >= 0.0, "cap must be a non-negative number");
            cc_db::set_cap_eur(&pool, eur).await?;
            println!("cap set to EUR {eur:.2}");
        }
    }
    Ok(())
}
