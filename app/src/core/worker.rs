//! マルチスレッドワーカープール。
//!
//! 設計方針:
//! - 判定（ジャッジ呼び出し）はI/Oバウンドなので `worker_count` 本のOSスレッドで並列実行する。
//! - DBへの書き込みは単一の「ライタースレッド」に集約し、SQLiteへの同時書き込み競合を避ける。
//!   ワーカー → (crossbeam channel) → ライタースレッド、という一方向パイプライン構成。

use std::sync::Arc;
use std::thread;

use crossbeam_channel::unbounded;

use crate::api::judge::{ComplianceJudge, JudgeVerdict, TokenUsage};
use crate::core::feed::Feed;
use crate::core::generator::{SeedLabel, SeededFeed};
use crate::core::progress::ProgressReporter;
use crate::core::repository::FeedRepository;

/// 1回の実行のサマリ統計。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunStats {
    pub total: u64,
    pub ok_count: u64,
    pub ng_count: u64,
    pub error_count: u64,
    /// ジャッジがトークン使用量を報告した呼び出しの累計入力トークン数。
    pub total_input_tokens: u64,
    /// ジャッジがトークン使用量を報告した呼び出しの累計出力トークン数。
    pub total_output_tokens: u64,
    /// トークン使用量を報告した呼び出しの件数（平均の分母。プロバイダ次第で
    /// 全件には満たない場合があるため `total` とは別に数える）。
    pub usage_sample_count: u64,
    /// 正解ラベル=NG（Risky種） かつ 判定=NG（正しく検出できた）。
    pub true_positive: u64,
    /// 正解ラベル=OK（Benign種） かつ 判定=NG（誤検知）。
    pub false_positive: u64,
    /// 正解ラベル=OK（Benign種） かつ 判定=OK（正しく見逃さなかった）。
    pub true_negative: u64,
    /// 正解ラベル=NG（Risky種） かつ 判定=OK（見逃し）。
    pub false_negative: u64,
}

impl RunStats {
    /// 実際にジャッジがNGと判定した比率（`judge_error` によるDLQ送りは含まない）。
    #[must_use]
    pub fn ng_rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            // 件数は数十万件規模（f64の52bit仮引数で正確に表現できる範囲）に収まる想定のため、
            // u64->f64変換による精度損失は実運用上問題にならない。
            #[allow(clippy::cast_precision_loss)]
            {
                self.ng_count as f64 / self.total as f64
            }
        }
    }

    /// 入力+出力トークンの合計。
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.total_input_tokens + self.total_output_tokens
    }

    /// トークン使用量が取れた呼び出し1件あたりの平均トークン数。1件も取れていなければ`0.0`。
    #[must_use]
    pub fn average_tokens_per_call(&self) -> f64 {
        if self.usage_sample_count == 0 {
            0.0
        } else {
            #[allow(clippy::cast_precision_loss)]
            {
                self.total_tokens() as f64 / self.usage_sample_count as f64
            }
        }
    }

    /// 分類の成否を判定できた件数（`judge_error` によるDLQ送りは判定自体が
    /// 行えていないため除外する）。Accuracy/Precision/Recallの計算に使う。
    #[must_use]
    pub fn classified_count(&self) -> u64 {
        self.true_positive + self.false_positive + self.true_negative + self.false_negative
    }

    /// 正解率: 全判定件数（エラー除く）のうち、期待ラベルと一致した割合。
    #[must_use]
    pub fn accuracy(&self) -> f64 {
        let denom = self.classified_count();
        if denom == 0 {
            0.0
        } else {
            #[allow(clippy::cast_precision_loss)]
            {
                (self.true_positive + self.true_negative) as f64 / denom as f64
            }
        }
    }

    /// 精度（適合率）: NGと判定したもののうち、実際にNG（種がRisky）だった割合。
    /// 「オオカミ少年」率の逆。NG判定が0件なら`0.0`。
    #[must_use]
    pub fn precision(&self) -> f64 {
        let denom = self.true_positive + self.false_positive;
        if denom == 0 {
            0.0
        } else {
            #[allow(clippy::cast_precision_loss)]
            {
                self.true_positive as f64 / denom as f64
            }
        }
    }

    /// 再現率: 実際にNG（種がRisky）だったもののうち、正しくNGと判定できた割合。
    /// 「見逃し」の少なさ。実際のNGが0件なら`0.0`。
    #[must_use]
    pub fn recall(&self) -> f64 {
        let denom = self.true_positive + self.false_negative;
        if denom == 0 {
            0.0
        } else {
            #[allow(clippy::cast_precision_loss)]
            {
                self.true_positive as f64 / denom as f64
            }
        }
    }
}

enum WorkOutcome {
    /// `u64` はジャッジ呼び出し（リトライ込み）に要した時間（ミリ秒）。
    Ok(Option<TokenUsage>, u64),
    Ng(String, Option<TokenUsage>, u64),
    Error(String, u64),
}

struct WorkResult {
    feed: Feed,
    /// 生成時に埋め込んだ正解ラベル。Accuracy/Precision/Recall計算とDB保存に使う。
    seed_label: SeedLabel,
    outcome: WorkOutcome,
}

/// フィード群を `worker_count` 本のスレッドで並列にジャッジし、
/// 結果を単一のDBライタースレッド経由で `repository` に保存する。
///
/// `feeds` は `core::generator::generate_feeds` が付与した正解ラベル付きの
/// `SeededFeed`。judge呼び出し自体はラベルを使わないが、判定後にAccuracy/Precision/
/// Recallを計算し、DBへ「期待されるOK/NG」として保存するために最後まで持ち歩く。
///
/// `judge` は呼び出し元から所有権を受け取り、各ワーカースレッドへ `Arc::clone` で
/// 配布する（スレッド間で共有するハンドルの受け渡しのため、値渡しが自然）。
///
/// `progress` は1件処理完了ごとに `inc(1)` が呼ばれる（ユニットテストでは
/// `core::progress::noop()` を渡し、表示処理を完全に無効化する）。
///
/// 戻り値は実行サマリ (`RunStats`)。
///
/// # Panics
/// ワーカースレッドまたはDBライタースレッドがパニックした場合、`join` の結果を
/// `expect` しているためこの関数もパニックする。
#[allow(clippy::needless_pass_by_value)]
pub fn run_pipeline<R>(
    feeds: Vec<SeededFeed>,
    judge: Arc<dyn ComplianceJudge>,
    repository: R,
    worker_count: usize,
    progress: Arc<dyn ProgressReporter>,
) -> RunStats
where
    R: FeedRepository + Send + 'static,
{
    let worker_count = worker_count.max(1);
    let total = feeds.len() as u64;

    let (work_tx, work_rx) = unbounded::<SeededFeed>();
    let (result_tx, result_rx) = unbounded::<WorkResult>();

    for feed in feeds {
        work_tx
            .send(feed)
            .expect("work channel receiver dropped unexpectedly before feeds were sent");
    }
    // 送信済みなので送信側は破棄してよい。これによりワーカー側の `iter()` は
    // キューが空になった時点で終了できる。
    drop(work_tx);

    let writer_handle = thread::spawn(move || {
        let mut stats = RunStats::default();
        for result in &result_rx {
            apply_result(&repository, &mut stats, result);
            progress.inc(1);
        }
        progress.finish();
        stats
    });

    let mut worker_handles = Vec::with_capacity(worker_count);
    for _ in 0..worker_count {
        let work_rx = work_rx.clone();
        let result_tx = result_tx.clone();
        let judge = Arc::clone(&judge);
        worker_handles.push(thread::spawn(move || {
            for seeded_feed in &work_rx {
                let call_start = std::time::Instant::now();
                let judge_result = judge.judge(&seeded_feed.feed);
                // リトライ（バックオフ込み）も含めた、呼び出し元から見た総所要時間。
                #[allow(clippy::cast_possible_truncation)]
                let latency_ms = call_start.elapsed().as_millis() as u64;
                let outcome = match judge_result {
                    Ok(outcome) => match outcome.verdict {
                        JudgeVerdict::Ok => WorkOutcome::Ok(outcome.usage, latency_ms),
                        JudgeVerdict::Ng { reason } => {
                            WorkOutcome::Ng(reason, outcome.usage, latency_ms)
                        }
                    },
                    Err(e) => WorkOutcome::Error(e.to_string(), latency_ms),
                };
                let result = WorkResult {
                    feed: seeded_feed.feed,
                    seed_label: seeded_feed.seed_label,
                    outcome,
                };
                // ライタースレッドが先に終了している（通常は起きない）場合は黙って抜ける。
                if result_tx.send(result).is_err() {
                    break;
                }
            }
        }));
    }
    // ワーカーごとにクローンを配ったので元の送信側ハンドルは不要。
    drop(result_tx);
    drop(work_rx);

    for handle in worker_handles {
        handle.join().expect("worker thread panicked");
    }

    let mut stats = writer_handle.join().expect("writer thread panicked");
    stats.total = total;
    stats
}

/// 1件の判定結果をDBへ保存し、`stats`（件数・混同行列・トークン集計）を更新する。
/// DBライタースレッドから結果を受け取るたびに呼ばれる。
fn apply_result<R: FeedRepository>(repository: &R, stats: &mut RunStats, result: WorkResult) {
    let expected = result.seed_label.expected_str();
    match result.outcome {
        WorkOutcome::Ok(usage, latency_ms) => {
            if let Err(e) = repository.save_ok(&result.feed, expected, usage.as_ref(), latency_ms) {
                tracing::error!("failed to save OK feed {}: {e}", result.feed.id);
            }
            stats.ok_count += 1;
            record_usage(stats, usage);
            match result.seed_label {
                SeedLabel::Benign => stats.true_negative += 1,
                SeedLabel::Risky => stats.false_negative += 1,
            }
        }
        WorkOutcome::Ng(reason, usage, latency_ms) => {
            if let Err(e) =
                repository.save_dlq(&result.feed, &reason, expected, usage.as_ref(), latency_ms)
            {
                tracing::error!("failed to save DLQ feed {}: {e}", result.feed.id);
            }
            stats.ng_count += 1;
            record_usage(stats, usage);
            match result.seed_label {
                SeedLabel::Risky => stats.true_positive += 1,
                SeedLabel::Benign => stats.false_positive += 1,
            }
        }
        WorkOutcome::Error(reason, latency_ms) => {
            // judge_errorは判定自体ができていないため、Accuracy/Precision/Recallの
            // 集計対象には含めない（混同行列には加算しない）。
            let dlq_reason = format!("judge_error: {reason}");
            if let Err(e) =
                repository.save_dlq(&result.feed, &dlq_reason, expected, None, latency_ms)
            {
                tracing::error!("failed to save errored feed {}: {e}", result.feed.id);
            }
            stats.error_count += 1;
        }
    }
}

/// トークン使用量を `stats` に積算する。`usage` が `None` の場合は何もしない。
fn record_usage(stats: &mut RunStats, usage: Option<TokenUsage>) {
    if let Some(u) = usage {
        stats.total_input_tokens += u.input_tokens;
        stats.total_output_tokens += u.output_tokens;
        stats.usage_sample_count += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::judge::{JudgeError, MockJudge};
    use crate::core::feed::{FeedId, MessageBody, TimeSentUtc, UserName};
    use crate::core::progress::noop;
    use crate::core::repository::InMemoryRepository;

    /// テスト用に `SeededFeed` をn件生成する。正解ラベルは全件同じ `label` にする
    /// （混同行列を意識しないテストでは `SeedLabel::Benign` を渡せばよい）。
    fn sample_feeds(n: u64, label: SeedLabel) -> Vec<SeededFeed> {
        (0..n)
            .map(|i| SeededFeed {
                feed: Feed::new(
                    FeedId::new(i),
                    UserName::new(format!("user_{i}")).unwrap(),
                    MessageBody::new("hello").unwrap(),
                    TimeSentUtc::new(chrono::Utc::now()),
                ),
                seed_label: label,
            })
            .collect()
    }

    #[test]
    fn all_ok_feeds_go_to_ok_repository() {
        let repo = InMemoryRepository::default();
        let repo_clone = repo.clone();
        let judge: Arc<dyn ComplianceJudge> = Arc::new(MockJudge::always(JudgeVerdict::Ok));

        let stats = run_pipeline(sample_feeds(20, SeedLabel::Benign), judge, repo, 4, noop());

        assert_eq!(stats.total, 20);
        assert_eq!(stats.ok_count, 20);
        assert_eq!(stats.ng_count, 0);
        assert_eq!(stats.error_count, 0);
        assert_eq!(repo_clone.ok_feeds.lock().unwrap().len(), 20);
        assert_eq!(repo_clone.dlq_feeds.lock().unwrap().len(), 0);
    }

    #[test]
    fn all_ng_feeds_go_to_dlq() {
        let repo = InMemoryRepository::default();
        let repo_clone = repo.clone();
        let judge: Arc<dyn ComplianceJudge> = Arc::new(MockJudge::always(JudgeVerdict::Ng {
            reason: "insider leak".into(),
        }));

        let stats = run_pipeline(sample_feeds(10, SeedLabel::Benign), judge, repo, 3, noop());

        assert_eq!(stats.ng_count, 10);
        assert_eq!(stats.ok_count, 0);
        let dlq = repo_clone.dlq_feeds.lock().unwrap();
        assert_eq!(dlq.len(), 10);
        assert!(
            dlq.iter()
                .all(|(_, reason, _, _, _)| reason == "insider leak")
        );
    }

    #[test]
    fn judge_errors_are_routed_to_dlq_with_error_prefix() {
        let repo = InMemoryRepository::default();
        let repo_clone = repo.clone();
        let judge: Arc<dyn ComplianceJudge> = Arc::new(MockJudge::sequence(vec![Err(
            JudgeError::Http("connection refused".into()),
        )]));

        let stats = run_pipeline(sample_feeds(5, SeedLabel::Benign), judge, repo, 2, noop());

        assert_eq!(stats.error_count, 5);
        assert_eq!(stats.ok_count, 0);
        assert_eq!(stats.ng_count, 0);
        let dlq = repo_clone.dlq_feeds.lock().unwrap();
        assert_eq!(dlq.len(), 5);
        assert!(
            dlq.iter()
                .all(|(_, reason, _, _, _)| reason.starts_with("judge_error:"))
        );
    }

    #[test]
    fn mixed_verdicts_are_counted_correctly() {
        let repo = InMemoryRepository::default();
        let judge: Arc<dyn ComplianceJudge> = Arc::new(MockJudge::sequence(vec![
            Ok(JudgeVerdict::Ok),
            Ok(JudgeVerdict::Ng {
                reason: "bad".into(),
            }),
        ]));

        let stats = run_pipeline(sample_feeds(2, SeedLabel::Benign), judge, repo, 1, noop());

        assert_eq!(stats.total, 2);
        assert_eq!(stats.ok_count + stats.ng_count, 2);
    }

    #[test]
    fn empty_feed_list_produces_zero_stats() {
        let repo = InMemoryRepository::default();
        let judge: Arc<dyn ComplianceJudge> = Arc::new(MockJudge::always(JudgeVerdict::Ok));

        let stats = run_pipeline(Vec::new(), judge, repo, 4, noop());

        assert_eq!(stats, RunStats::default());
    }

    #[test]
    fn perfect_classifier_yields_100_percent_accuracy_precision_recall() {
        // Riskyの種はすべてNGと判定され、Benignの種はすべてOKと判定される「完璧な」
        // 判定器を想定する。
        let repo = InMemoryRepository::default();
        let judge: Arc<dyn ComplianceJudge> = Arc::new(MockJudge::sequence(vec![
            Ok(JudgeVerdict::Ng {
                reason: "bad".into(),
            }),
            Ok(JudgeVerdict::Ok),
        ]));
        let feeds = vec![
            SeededFeed {
                feed: sample_feeds(1, SeedLabel::Risky).remove(0).feed,
                seed_label: SeedLabel::Risky,
            },
            SeededFeed {
                feed: sample_feeds(1, SeedLabel::Benign).remove(0).feed,
                seed_label: SeedLabel::Benign,
            },
        ];

        let stats = run_pipeline(feeds, judge, repo, 1, noop());

        assert_eq!(stats.true_positive, 1);
        assert_eq!(stats.false_positive, 0);
        assert_eq!(stats.true_negative, 1);
        assert_eq!(stats.false_negative, 0);
        assert_eq!(stats.classified_count(), 2);
        assert!((stats.accuracy() - 1.0).abs() < f64::EPSILON);
        assert!((stats.precision() - 1.0).abs() < f64::EPSILON);
        assert!((stats.recall() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn confusion_matrix_counts_false_positive_and_false_negative_correctly() {
        // feed0: Risky種なのにOKと判定された（見逃し = False Negative）
        // feed1: Benign種なのにNGと判定された（誤検知 = False Positive）
        let repo = InMemoryRepository::default();
        let judge: Arc<dyn ComplianceJudge> = Arc::new(MockJudge::sequence(vec![
            Ok(JudgeVerdict::Ok),
            Ok(JudgeVerdict::Ng {
                reason: "oops".into(),
            }),
        ]));
        let feeds = vec![
            SeededFeed {
                feed: sample_feeds(1, SeedLabel::Risky).remove(0).feed,
                seed_label: SeedLabel::Risky,
            },
            SeededFeed {
                feed: sample_feeds(1, SeedLabel::Benign).remove(0).feed,
                seed_label: SeedLabel::Benign,
            },
        ];

        let stats = run_pipeline(feeds, judge, repo, 1, noop());

        assert_eq!(stats.true_positive, 0);
        assert_eq!(stats.false_positive, 1);
        assert_eq!(stats.true_negative, 0);
        assert_eq!(stats.false_negative, 1);
        assert_eq!(stats.classified_count(), 2);
        // 2件中0件が正解ラベルと一致。
        assert!(stats.accuracy().abs() < f64::EPSILON);
        assert!(stats.precision().abs() < f64::EPSILON);
        assert!(stats.recall().abs() < f64::EPSILON);
    }

    #[test]
    fn judge_errors_are_excluded_from_confusion_matrix() {
        // judge_errorは判定自体が行えていないため、Accuracy等の分母（classified_count）
        // に含めてはならない。
        let repo = InMemoryRepository::default();
        let judge: Arc<dyn ComplianceJudge> = Arc::new(MockJudge::sequence(vec![Err(
            JudgeError::Http("timeout".into()),
        )]));

        let stats = run_pipeline(sample_feeds(3, SeedLabel::Risky), judge, repo, 2, noop());

        assert_eq!(stats.error_count, 3);
        assert_eq!(stats.classified_count(), 0);
        assert!(stats.accuracy().abs() < f64::EPSILON);
    }

    /// 呼び出しごとに `delay` だけスリープしてから固定の判定結果を返すテスト用ジャッジ。
    /// `latency_ms` の計測が実際に機能していることを検証するために使う。
    struct SlowJudge {
        delay: std::time::Duration,
        verdict: JudgeVerdict,
    }

    impl ComplianceJudge for SlowJudge {
        fn judge(
            &self,
            _feed: &Feed,
        ) -> Result<crate::api::judge::JudgeOutcome, crate::api::judge::JudgeError> {
            std::thread::sleep(self.delay);
            Ok(crate::api::judge::JudgeOutcome {
                verdict: self.verdict.clone(),
                usage: None,
            })
        }
    }

    #[test]
    fn latency_ms_reflects_actual_judge_call_duration() {
        let repo = InMemoryRepository::default();
        let repo_clone = repo.clone();
        let judge: Arc<dyn ComplianceJudge> = Arc::new(SlowJudge {
            delay: std::time::Duration::from_millis(50),
            verdict: JudgeVerdict::Ok,
        });

        run_pipeline(sample_feeds(1, SeedLabel::Benign), judge, repo, 1, noop());

        let ok_feeds = repo_clone.ok_feeds.lock().unwrap();
        let recorded_latency_ms = ok_feeds[0].3;
        // スリープ分（50ms）より短くなることはない。実行環境のスケジューリング遅延を
        // 考慮して、上限は緩く（1秒未満）だけ確認する。
        assert!(
            recorded_latency_ms >= 50,
            "expected latency >= 50ms, got {recorded_latency_ms}ms"
        );
        assert!(recorded_latency_ms < 1000);
    }

    #[test]
    fn precision_and_recall_are_zero_when_denominator_is_zero() {
        // NG判定も実際のRisky種も1件も無ければ、precision/recallは0除算せず0.0を返す。
        let stats = RunStats::default();
        assert!(stats.precision().abs() < f64::EPSILON);
        assert!(stats.recall().abs() < f64::EPSILON);
        assert!(stats.accuracy().abs() < f64::EPSILON);
    }

    /// `inc`/`finish` の呼び出し回数を記録するテスト用スパイ。
    struct SpyProgress {
        inc_calls: std::sync::Mutex<Vec<u64>>,
        finished: std::sync::atomic::AtomicBool,
    }

    impl Default for SpyProgress {
        fn default() -> Self {
            Self {
                inc_calls: std::sync::Mutex::new(Vec::new()),
                finished: std::sync::atomic::AtomicBool::new(false),
            }
        }
    }

    impl crate::core::progress::ProgressReporter for SpyProgress {
        fn inc(&self, delta: u64) {
            self.inc_calls.lock().expect("lock poisoned").push(delta);
        }

        fn finish(&self) {
            self.finished
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[test]
    fn progress_is_incremented_once_per_feed_and_finished_at_the_end() {
        let repo = InMemoryRepository::default();
        let judge: Arc<dyn ComplianceJudge> = Arc::new(MockJudge::always(JudgeVerdict::Ok));
        let spy = Arc::new(SpyProgress::default());

        let stats = run_pipeline(
            sample_feeds(7, SeedLabel::Benign),
            judge,
            repo,
            3,
            spy.clone(),
        );

        assert_eq!(stats.total, 7);
        assert_eq!(spy.inc_calls.lock().unwrap().len(), 7);
        assert!(spy.inc_calls.lock().unwrap().iter().all(|&d| d == 1));
        assert!(spy.finished.load(std::sync::atomic::Ordering::SeqCst));
    }
}
