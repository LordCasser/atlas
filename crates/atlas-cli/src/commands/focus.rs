//! `atlas focus` — manage focus analysis state.

use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum FocusCommand {
    /// Show bootstrap progress and active focus closures.
    Status,
    /// Pre-warm a focus closure for a symbol.
    Expand {
        /// Symbol name to pre-warm.
        symbol: String,
        /// Language of the symbol.
        #[arg(long, default_value = "c")]
        language: String,
    },
    /// List known gaps for a symbol's closure.
    Gaps {
        symbol: String,
    },
}

impl FocusCommand {
    pub fn run(&self) -> anyhow::Result<()> {
        match self {
            FocusCommand::Status => {
                println!("Focus status: bootstrap in progress");
                println!("  Tier 0 (FileInventory): checking...");
                println!("  Tier 1 (SymbolHints): pending");
                println!("  Active closures: 0");
                println!("  Queued jobs: 0");
                Ok(())
            }
            FocusCommand::Expand { symbol, language } => {
                println!(
                    "Pre-warming focus for symbol '{}' (language: {})",
                    symbol, language
                );
                println!("  This feature requires a running atlas server.");
                println!("  Use 'atlas open' first to start the background runtime.");
                Ok(())
            }
            FocusCommand::Gaps { symbol } => {
                println!("Known gaps for '{}':", symbol);
                println!("  (no closures built yet — use 'atlas focus expand' first)");
                Ok(())
            }
        }
    }
}
