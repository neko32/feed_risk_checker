//! 進捗報告の抽象。
//!
//! `run_pipeline` はターミナルUI（indicatif）に直接依存せず、この trait 経由で
//! 進捗を報告する。ユニットテストでは `NoopProgress` に差し替え、
//! 実行バイナリ（`main.rs`）では indicatif ベースの実装を使う。

use std::sync::Arc;

/// パイプラインの進捗を報告する抽象。
pub trait ProgressReporter: Send + Sync {
    /// `delta` 件処理が完了したことを報告する。
    fn inc(&self, delta: u64);

    /// 全件処理完了を報告する（表示のクリーンアップ等に使う）。
    fn finish(&self);
}

/// 進捗表示を行わない実装。ユニットテスト・統合テストで使う既定値。
pub struct NoopProgress;

impl ProgressReporter for NoopProgress {
    fn inc(&self, _delta: u64) {}
    fn finish(&self) {}
}

// `ProgressReporter` はこのクレートで定義された trait なので、外部クレートの型
// （`indicatif::ProgressBar`）への実装はこのクレート内でのみ許可される（orphan rule）。
// そのため `main.rs`（別クレート）ではなくここで実装する。
impl ProgressReporter for indicatif::ProgressBar {
    fn inc(&self, delta: u64) {
        indicatif::ProgressBar::inc(self, delta);
    }

    fn finish(&self) {
        indicatif::ProgressBar::finish(self);
    }
}

/// テスト・デフォルト用に `Arc<dyn ProgressReporter>` を手軽に得るためのヘルパー。
#[must_use]
pub fn noop() -> Arc<dyn ProgressReporter> {
    Arc::new(NoopProgress)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_progress_does_not_panic() {
        let p = NoopProgress;
        p.inc(10);
        p.finish();
    }

    #[test]
    fn noop_helper_returns_usable_reporter() {
        let p = noop();
        p.inc(1);
        p.finish();
    }
}
