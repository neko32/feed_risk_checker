//! エントリポイント。CLI引数を解釈し、生成→判定→振り分けパイプラインを実行する。
//!
//! テスト容易性のため、純粋な判断ロジック（スケールの解決・HITL確認プロンプトの要否・
//! 入力文字列のyes/no判定）は副作用（標準入出力・ファイルI/O・ネットワーク）から
//! 分離し、下部の `logic` モジュールにまとめている。`main`/`run_command` 自体は
//! 薄いI/Oの皮であり、ユニットテストはしない方針（統合的な動作確認は手動実行・
//! `scripts_local` 経由で行う）。

use std::io::Write;
use std::sync::Arc;

use clap::Parser;

use feed_risk_checker::api::judge::ComplianceJudge;
use feed_risk_checker::api::typesafe_judge::TypeSafeAiJudge;
use feed_risk_checker::cli::{Cli, Command, Scale};
use feed_risk_checker::config::Config;
use feed_risk_checker::core::generator::{GeneratorConfig, generate_feeds};
use feed_risk_checker::core::progress::ProgressReporter;
use feed_risk_checker::core::repository::SqliteRepository;
use feed_risk_checker::core::worker::run_pipeline;
use indicatif::{ProgressBar, ProgressStyle};

use logic::{resolve_total_feeds, resolve_user_count, should_prompt_for_confirmation};

fn main() {
    tracing_subscriber::fmt::init();
    dotenvy::dotenv().ok();

    let cli = Cli::parse();
    let config = Config::from_env();

    match cli.command {
        Command::Run {
            scale,
            total_feeds,
            user_count,
            ng_seed_rate,
            yes,
        } => run_command(scale, total_feeds, user_count, ng_seed_rate, yes, &config),
    }
}

fn run_command(
    scale: Scale,
    total_feeds_override: Option<u64>,
    user_count_override: Option<u32>,
    ng_seed_rate: f64,
    yes: bool,
    config: &Config,
) {
    let total_feeds = resolve_total_feeds(scale, total_feeds_override);
    let user_count = resolve_user_count(scale, user_count_override);

    // HITL（Human-In-The-Loop）ゲート: full スケール実行は明示的な承認が無い限り走らせない。
    if should_prompt_for_confirmation(scale, yes) && !confirm_full_run(total_feeds, user_count) {
        println!("Cancelled. Review the small-scale test results before retrying.");
        return;
    }

    let gen_config = GeneratorConfig {
        total_feeds,
        user_count,
        ng_seed_rate,
    };
    let (seeded_feeds, gen_report) = generate_feeds(&gen_config);
    println!(
        "Generation complete: total={}, NG seeds embedded={} (seed_rate={:.2}%)",
        gen_report.total_generated,
        gen_report.seeded_ng_count,
        gen_report.seeded_ng_rate() * 100.0
    );
    // 生成時に埋め込んだ正解ラベル(SeedLabel)は捨てずに最後まで持ち歩く。
    // Accuracy/Precision/Recall計算とDB保存（期待ラベル列）に使う。
    // ローカルKev(LM Studio)からTypeSafe AI（System One, model="jev-latest"）に
    // 判定エンジンを差し替え済み。APIキーは環境変数 `API_KEY_JEV` から読む
    // （値は絶対にログ・標準出力に出さない）。
    let typesafe_api_key = config.typesafe_api_key.clone().expect(
        "API_KEY_JEV environment variable must be set (TypeSafe AI API key). \
         See docs/api.md for how to obtain one.",
    );
    let judge: Arc<dyn ComplianceJudge> = Arc::new(TypeSafeAiJudge::new(
        config.typesafe_base_url.clone(),
        config.typesafe_model.clone(),
        typesafe_api_key,
        config.retry_count,
    ));
    let repository =
        SqliteRepository::open(&config.sqlite_path).expect("failed to open sqlite database");
    // 1回の実行=1スナップショットとする運用のため、前回実行分の残データをクリアしてから
    // 開始する（残したままだと、生成フィードIDが毎回0から振り直されるため、2回目以降の
    // 保存がすべて主キー重複で失敗する）。
    repository
        .reset()
        .expect("failed to reset sqlite tables for a clean run");
    println!("Cleared data from any previous run — starting fresh.");

    println!(
        "Judging started: worker_count={}, provider=TypeSafe AI, model={}, db={}",
        config.worker_count, config.typesafe_model, config.sqlite_path
    );
    let progress = build_progress_bar(gen_report.total_generated);
    let start = std::time::Instant::now();
    let stats = run_pipeline(
        seeded_feeds,
        judge,
        repository,
        config.worker_count,
        progress,
    );
    let elapsed = start.elapsed();

    println!("=== Run Summary ===");
    println!("Total          : {}", stats.total);
    println!("OK             : {}", stats.ok_count);
    println!("NG (DLQ)       : {}", stats.ng_count);
    println!("ERROR (DLQ)    : {}", stats.error_count);
    println!("Judged NG rate : {:.2}%", stats.ng_rate() * 100.0);
    println!(
        "Accuracy       : {:.2}% (over {} classified, excluding judge_error)",
        stats.accuracy() * 100.0,
        stats.classified_count()
    );
    println!("Precision      : {:.2}%", stats.precision() * 100.0);
    println!("Recall         : {:.2}%", stats.recall() * 100.0);
    println!(
        "Tokens total   : {} (avg {:.1}/request over {} requests with usage data)",
        stats.total_tokens(),
        stats.average_tokens_per_call(),
        stats.usage_sample_count
    );
    println!("Elapsed time   : {:.2}s", elapsed.as_secs_f64());
    println!("SQLite         : {}", config.sqlite_path);
    println!("View results with the viewer (viewer/viewer.py).");
}

/// full スケール実行前の対話的確認（HITLゲート）。y/yes以外はすべて拒否とみなす。
fn confirm_full_run(total_feeds: u64, user_count: u32) -> bool {
    print!("Run at full volume (total_feeds={total_feeds}, user_count={user_count})? [y/N]: ");
    std::io::stdout().flush().ok();
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        return false;
    }
    logic::parse_confirmation(&input)
}

/// `indicatif` ベースの進捗バーを構築する。`total` 件に対する進捗を表示する。
fn build_progress_bar(total: u64) -> Arc<dyn ProgressReporter> {
    let bar = ProgressBar::new(total);
    if let Ok(style) = ProgressStyle::with_template(
        "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} ({percent}%, ETA {eta})",
    ) {
        bar.set_style(style.progress_chars("#>-"));
    }
    Arc::new(bar)
}

/// 副作用から分離した純粋なロジック。ユニットテストはこちらに対して行う。
mod logic {
    use feed_risk_checker::cli::Scale;

    /// `--total-feeds` 指定があればそれを優先し、無ければスケールのデフォルト値を使う。
    pub fn resolve_total_feeds(scale: Scale, total_feeds_override: Option<u64>) -> u64 {
        total_feeds_override.unwrap_or_else(|| scale.default_total_feeds())
    }

    /// `--user-count` 指定があればそれを優先し、無ければスケールのデフォルト値を使う。
    pub fn resolve_user_count(scale: Scale, user_count_override: Option<u32>) -> u32 {
        user_count_override.unwrap_or_else(|| scale.default_user_count())
    }

    /// full スケールかつ `--yes` が指定されていない場合のみ、HITL確認プロンプトを表示する。
    pub fn should_prompt_for_confirmation(scale: Scale, yes: bool) -> bool {
        scale == Scale::Full && !yes
    }

    /// 標準入力からの1行を受け取り、"y"/"yes"（大小文字無視・前後空白無視）のみを承認とみなす。
    pub fn parse_confirmation(input: &str) -> bool {
        matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn resolve_total_feeds_prefers_override() {
            assert_eq!(resolve_total_feeds(Scale::Small, Some(42)), 42);
        }

        #[test]
        fn resolve_total_feeds_falls_back_to_scale_default() {
            assert_eq!(resolve_total_feeds(Scale::Small, None), 1_000);
            assert_eq!(resolve_total_feeds(Scale::Full, None), 100_000);
        }

        #[test]
        fn resolve_user_count_prefers_override() {
            assert_eq!(resolve_user_count(Scale::Full, Some(7)), 7);
        }

        #[test]
        fn resolve_user_count_falls_back_to_scale_default() {
            assert_eq!(resolve_user_count(Scale::Small, None), 5);
            assert_eq!(resolve_user_count(Scale::Full, None), 1_000);
        }

        #[test]
        fn small_scale_never_prompts() {
            assert!(!should_prompt_for_confirmation(Scale::Small, false));
            assert!(!should_prompt_for_confirmation(Scale::Small, true));
        }

        #[test]
        fn full_scale_prompts_unless_yes_flag_given() {
            assert!(should_prompt_for_confirmation(Scale::Full, false));
            assert!(!should_prompt_for_confirmation(Scale::Full, true));
        }

        #[test]
        fn parse_confirmation_accepts_y_variants() {
            assert!(parse_confirmation("y\n"));
            assert!(parse_confirmation("Y"));
            assert!(parse_confirmation("yes\n"));
            assert!(parse_confirmation("  YES  "));
        }

        #[test]
        fn parse_confirmation_rejects_anything_else() {
            assert!(!parse_confirmation("n"));
            assert!(!parse_confirmation(""));
            assert!(!parse_confirmation("\n"));
            assert!(!parse_confirmation("yeah"));
        }
    }
}
