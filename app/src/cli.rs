//! CLI引数定義。
//!
//! `run --scale small` （デフォルト）: 1,000件 / 5ユーザの小規模テスト実行。
//! `run --scale full`: 100,000件 / 1,000ユーザの本番ボリューム実行。
//!   HITL（Human-In-The-Loop）ゲートとして、`--yes` を付けない限り
//!   実行前に対話的な確認プロンプトを表示する。

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "feed_risk_checker",
    about = "LinkedIn POC: a compliance-judging pipeline for high-volume message feeds (powered by the 'Kev' decision model)"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Generate feeds, judge them with Kev (LM Studio), and route to `SQLite` / DLQ.
    Run {
        /// Run scale. small = 1,000 feeds / 5 users, full = 100,000 feeds / 1,000 users.
        #[arg(long, value_enum, default_value = "small")]
        scale: Scale,

        /// Total number of feeds to generate (overrides the --scale default when given).
        #[arg(long)]
        total_feeds: Option<u64>,

        /// Number of users to simulate (overrides the --scale default when given).
        #[arg(long)]
        user_count: Option<u32>,

        /// Fraction of feeds seeded as compliance violations (0.0-1.0).
        #[arg(long, default_value_t = 0.03)]
        ng_seed_rate: f64,

        /// Skip the interactive confirmation prompt for --scale full (for automated/CI runs).
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    Small,
    Full,
}

impl Scale {
    #[must_use]
    pub fn default_total_feeds(&self) -> u64 {
        match self {
            Scale::Small => 1_000,
            Scale::Full => 100_000,
        }
    }

    #[must_use]
    pub fn default_user_count(&self) -> u32 {
        match self {
            Scale::Small => 5,
            Scale::Full => 1_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_scale_defaults() {
        assert_eq!(Scale::Small.default_total_feeds(), 1_000);
        assert_eq!(Scale::Small.default_user_count(), 5);
    }

    #[test]
    fn full_scale_defaults() {
        assert_eq!(Scale::Full.default_total_feeds(), 100_000);
        assert_eq!(Scale::Full.default_user_count(), 1_000);
    }

    #[test]
    fn cli_parses_run_with_defaults() {
        let cli = Cli::parse_from(["feed_risk_checker", "run"]);
        match cli.command {
            Command::Run { scale, yes, .. } => {
                assert_eq!(scale, Scale::Small);
                assert!(!yes);
            }
        }
    }

    #[test]
    fn cli_parses_run_full_with_yes() {
        let cli = Cli::parse_from(["feed_risk_checker", "run", "--scale", "full", "--yes"]);
        match cli.command {
            Command::Run { scale, yes, .. } => {
                assert_eq!(scale, Scale::Full);
                assert!(yes);
            }
        }
    }

    #[test]
    fn cli_parses_explicit_overrides() {
        let cli = Cli::parse_from([
            "feed_risk_checker",
            "run",
            "--total-feeds",
            "42",
            "--user-count",
            "7",
        ]);
        match cli.command {
            Command::Run {
                total_feeds,
                user_count,
                ..
            } => {
                assert_eq!(total_feeds, Some(42));
                assert_eq!(user_count, Some(7));
            }
        }
    }
}
