//! フィードの永続化層。
//!
//! `FeedRepository` trait の背後に `SQLite` 実装 (`SqliteRepository`) を隠蔽し、
//! テストでは外部依存（実ファイルI/O）無しの `InMemoryRepository` に差し替える。
//!
//! 設計方針: ワーカースレッドは並列にジャッジを呼び出すが、SQLiteへの書き込みは
//! 単一の「DBライタースレッド」に集約し、ここで順次 `save_ok` / `save_dlq` を呼ぶ。
//! これにより複数スレッドからの同時書き込み競合を避ける。

use chrono::Utc;
use rusqlite::{Connection, params};
use thiserror::Error;

use super::feed::Feed;
use crate::api::judge::TokenUsage;

#[derive(Debug, Error)]
pub enum RepoError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// フィードの永続化を行う抽象。OK判定とNG(DLQ)判定で保存先を分ける。
///
/// `expected_label` は生成時に埋め込んだ正解ラベル（`"OK"` または `"NG"`。
/// `core::generator::SeedLabel::expected_str` 参照）。実際の判定結果と比較して
/// Accuracy/Precision/Recallを計算・表示するために保存する。
///
/// `usage` はジャッジ呼び出しで取得できたトークン使用量（プロバイダ次第で `None` になる
/// こともある）。ビューアでの合計・平均トークン数表示のためにそのまま保存する。
///
/// `latency_ms` はジャッジ呼び出し（リトライ込み）に要した時間（ミリ秒）。
pub trait FeedRepository {
    /// # Errors
    /// 永続化先への書き込みに失敗した場合に返す。
    fn save_ok(
        &self,
        feed: &Feed,
        expected_label: &str,
        usage: Option<&TokenUsage>,
        latency_ms: u64,
    ) -> Result<(), RepoError>;

    /// # Errors
    /// 永続化先への書き込みに失敗した場合に返す。
    fn save_dlq(
        &self,
        feed: &Feed,
        reason: &str,
        expected_label: &str,
        usage: Option<&TokenUsage>,
        latency_ms: u64,
    ) -> Result<(), RepoError>;
}

/// `SQLite` ベースの実装。`feeds` テーブルと `dlq_feeds` テーブルを同一DBファイル内に持つ。
pub struct SqliteRepository {
    conn: Connection,
}

impl SqliteRepository {
    /// # Errors
    /// DBファイルのオープンまたはスキーマ初期化に失敗した場合に返す。
    pub fn open(path: &str) -> Result<Self, RepoError> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        let repo = Self { conn };
        repo.init_schema()?;
        Ok(repo)
    }

    /// テスト等でインメモリDBを使いたい場合。
    ///
    /// # Errors
    /// スキーマ初期化に失敗した場合に返す。
    pub fn open_in_memory() -> Result<Self, RepoError> {
        let conn = Connection::open_in_memory()?;
        let repo = Self { conn };
        repo.init_schema()?;
        Ok(repo)
    }

    fn init_schema(&self) -> Result<(), RepoError> {
        // input_tokens/output_tokens/expected_labelは、古いスキーマのDBや
        // ジャッジがusageを報告しなかった場合にNULLになり得る
        // （ビューアでの合計・平均集計、Accuracy等の計算はNULLを除外して行う）。
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS feeds (
                id INTEGER PRIMARY KEY,
                user_name TEXT NOT NULL,
                message TEXT NOT NULL,
                time_sent TEXT NOT NULL,
                judged_at TEXT NOT NULL,
                input_tokens INTEGER,
                output_tokens INTEGER,
                expected_label TEXT,
                latency_ms INTEGER
            );
            CREATE TABLE IF NOT EXISTS dlq_feeds (
                id INTEGER PRIMARY KEY,
                user_name TEXT NOT NULL,
                message TEXT NOT NULL,
                time_sent TEXT NOT NULL,
                reason TEXT NOT NULL,
                judged_at TEXT NOT NULL,
                input_tokens INTEGER,
                output_tokens INTEGER,
                expected_label TEXT,
                latency_ms INTEGER
            );",
        )?;
        // 既存の（列追加前の）DBファイルを開いた場合は CREATE TABLE IF NOT EXISTS が
        // 素通りしてしまうため、不足している列を明示的に追加する簡易マイグレーション。
        self.add_column_if_missing("feeds", "input_tokens", "INTEGER")?;
        self.add_column_if_missing("feeds", "output_tokens", "INTEGER")?;
        self.add_column_if_missing("feeds", "expected_label", "TEXT")?;
        self.add_column_if_missing("feeds", "latency_ms", "INTEGER")?;
        self.add_column_if_missing("dlq_feeds", "input_tokens", "INTEGER")?;
        self.add_column_if_missing("dlq_feeds", "output_tokens", "INTEGER")?;
        self.add_column_if_missing("dlq_feeds", "expected_label", "TEXT")?;
        self.add_column_if_missing("dlq_feeds", "latency_ms", "INTEGER")?;
        Ok(())
    }

    fn add_column_if_missing(
        &self,
        table: &str,
        column: &str,
        sql_type: &str,
    ) -> Result<(), RepoError> {
        match self.conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {sql_type}"),
            [],
        ) {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(_, Some(msg)))
                if msg.contains("duplicate column name") =>
            {
                // 列が既に存在する（新規作成されたDBではCREATE TABLEの時点で列があるため、
                // この分岐は既存DBへのマイグレーション時以外は通常ここに来る）。想定内。
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// `feeds` / `dlq_feeds` の既存データをすべて削除し、クリーンな状態から実行を開始する。
    ///
    /// `feed_risk_checker` は「1回の実行 = 1スナップショット」の運用を前提としており
    /// （小規模テスト→HITL確認→本番実行、のように同じDBファイルを使い回すことがある）、
    /// 生成されるフィードIDは毎回 `0` から振り直されるため、前回実行分のデータが
    /// 残っていると `id` の主キー重複で2回目以降の保存がすべて失敗する
    /// （実機検証で確認済みの不具合）。これを避けるため、新しい実行の開始時に必ず呼ぶ。
    ///
    /// # Errors
    /// テーブルのクリアに失敗した場合に返す。
    pub fn reset(&self) -> Result<(), RepoError> {
        self.conn
            .execute_batch("DELETE FROM feeds; DELETE FROM dlq_feeds;")?;
        Ok(())
    }

    /// # Errors
    /// クエリ実行に失敗した場合に返す。
    pub fn count_ok(&self) -> Result<u64, RepoError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM feeds", [], |row| row.get(0))?;
        // SQLiteのCOUNT(*)は常に0以上なので符号損失は発生しない。
        #[allow(clippy::cast_sign_loss)]
        Ok(count as u64)
    }

    /// # Errors
    /// クエリ実行に失敗した場合に返す。
    pub fn count_dlq(&self) -> Result<u64, RepoError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM dlq_feeds", [], |row| row.get(0))?;
        // SQLiteのCOUNT(*)は常に0以上なので符号損失は発生しない。
        #[allow(clippy::cast_sign_loss)]
        Ok(count as u64)
    }
}

impl FeedRepository for SqliteRepository {
    fn save_ok(
        &self,
        feed: &Feed,
        expected_label: &str,
        usage: Option<&TokenUsage>,
        latency_ms: u64,
    ) -> Result<(), RepoError> {
        // feed.idは生成時にtotal_feeds（想定上限は数十万件）でしか振られないため、
        // i64::MAXを超えてラップすることは実運用上発生しない。
        #[allow(clippy::cast_possible_wrap)]
        let id = feed.id.value() as i64;
        let (input_tokens, output_tokens) = usage_as_i64(usage);
        // 1件あたりのレイテンシがi64::MAXを超えることは実運用上発生しない。
        #[allow(clippy::cast_possible_wrap)]
        let latency_ms = latency_ms as i64;
        self.conn.execute(
            "INSERT INTO feeds (id, user_name, message, time_sent, judged_at, input_tokens, output_tokens, expected_label, latency_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id,
                feed.user_name.as_str(),
                feed.message.as_str(),
                feed.time_sent.to_rfc3339(),
                Utc::now().to_rfc3339(),
                input_tokens,
                output_tokens,
                expected_label,
                latency_ms,
            ],
        )?;
        Ok(())
    }

    fn save_dlq(
        &self,
        feed: &Feed,
        reason: &str,
        expected_label: &str,
        usage: Option<&TokenUsage>,
        latency_ms: u64,
    ) -> Result<(), RepoError> {
        #[allow(clippy::cast_possible_wrap)]
        let id = feed.id.value() as i64;
        let (input_tokens, output_tokens) = usage_as_i64(usage);
        #[allow(clippy::cast_possible_wrap)]
        let latency_ms = latency_ms as i64;
        self.conn.execute(
            "INSERT INTO dlq_feeds (id, user_name, message, time_sent, reason, judged_at, input_tokens, output_tokens, expected_label, latency_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                feed.user_name.as_str(),
                feed.message.as_str(),
                feed.time_sent.to_rfc3339(),
                reason,
                Utc::now().to_rfc3339(),
                input_tokens,
                output_tokens,
                expected_label,
                latency_ms,
            ],
        )?;
        Ok(())
    }
}

/// `TokenUsage` を `SQLite` の `INTEGER` カラムに入れられる形（`Option<i64>`のペア）に変換する。
/// `u64`→`i64`はトークン数が現実的な範囲（数百〜数千程度）に収まるため安全。
#[allow(clippy::cast_possible_wrap)]
fn usage_as_i64(usage: Option<&TokenUsage>) -> (Option<i64>, Option<i64>) {
    match usage {
        Some(u) => (Some(u.input_tokens as i64), Some(u.output_tokens as i64)),
        None => (None, None),
    }
}

/// `InMemoryRepository::ok_feeds` の要素型（フィード・トークン使用量・正解ラベル・レイテンシの組）。
#[cfg(test)]
type OkRecord = (Feed, Option<TokenUsage>, String, u64);
/// `InMemoryRepository::dlq_feeds` の要素型
/// （フィード・NG理由・トークン使用量・正解ラベル・レイテンシの組）。
#[cfg(test)]
type DlqRecord = (Feed, String, Option<TokenUsage>, String, u64);

/// テスト用のインメモリリポジトリ。ファイルI/O無しでロジックを検証する。
///
/// `Arc<Mutex<_>>` で内部状態を保持するため `Clone` でき、
/// 所有権を `run_pipeline` に渡した後も別のクローンから結果を検査できる。
#[cfg(test)]
#[derive(Clone)]
pub struct InMemoryRepository {
    pub ok_feeds: std::sync::Arc<std::sync::Mutex<Vec<OkRecord>>>,
    pub dlq_feeds: std::sync::Arc<std::sync::Mutex<Vec<DlqRecord>>>,
}

#[cfg(test)]
impl Default for InMemoryRepository {
    fn default() -> Self {
        Self {
            ok_feeds: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            dlq_feeds: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
}

#[cfg(test)]
impl FeedRepository for InMemoryRepository {
    fn save_ok(
        &self,
        feed: &Feed,
        expected_label: &str,
        usage: Option<&TokenUsage>,
        latency_ms: u64,
    ) -> Result<(), RepoError> {
        self.ok_feeds.lock().expect("lock poisoned").push((
            feed.clone(),
            usage.copied(),
            expected_label.to_string(),
            latency_ms,
        ));
        Ok(())
    }

    fn save_dlq(
        &self,
        feed: &Feed,
        reason: &str,
        expected_label: &str,
        usage: Option<&TokenUsage>,
        latency_ms: u64,
    ) -> Result<(), RepoError> {
        self.dlq_feeds.lock().expect("lock poisoned").push((
            feed.clone(),
            reason.to_string(),
            usage.copied(),
            expected_label.to_string(),
            latency_ms,
        ));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::feed::{FeedId, MessageBody, TimeSentUtc, UserName};

    fn sample_feed(id: u64) -> Feed {
        Feed::new(
            FeedId::new(id),
            UserName::new("tester").unwrap(),
            MessageBody::new("hello").unwrap(),
            TimeSentUtc::new(Utc::now()),
        )
    }

    #[test]
    fn sqlite_repo_saves_and_counts_ok_feeds() {
        let repo = SqliteRepository::open_in_memory().unwrap();
        repo.save_ok(&sample_feed(1), "OK", None, 10).unwrap();
        repo.save_ok(&sample_feed(2), "OK", None, 10).unwrap();
        assert_eq!(repo.count_ok().unwrap(), 2);
        assert_eq!(repo.count_dlq().unwrap(), 0);
    }

    #[test]
    fn sqlite_repo_saves_and_counts_dlq_feeds() {
        let repo = SqliteRepository::open_in_memory().unwrap();
        repo.save_dlq(&sample_feed(1), "insider info", "NG", None, 15)
            .unwrap();
        assert_eq!(repo.count_dlq().unwrap(), 1);
        assert_eq!(repo.count_ok().unwrap(), 0);
    }

    #[test]
    fn sqlite_repo_persists_expected_label_for_ok_and_dlq_feeds() {
        let repo = SqliteRepository::open_in_memory().unwrap();
        repo.save_ok(&sample_feed(1), "OK", None, 10).unwrap();
        repo.save_dlq(&sample_feed(2), "bad", "NG", None, 15)
            .unwrap();

        let conn = &repo.conn;
        let expected: String = conn
            .query_row("SELECT expected_label FROM feeds WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(expected, "OK");

        let expected: String = conn
            .query_row(
                "SELECT expected_label FROM dlq_feeds WHERE id = 2",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(expected, "NG");
    }

    #[test]
    fn sqlite_repo_persists_token_usage_for_ok_and_dlq_feeds() {
        let repo = SqliteRepository::open_in_memory().unwrap();
        let usage_ok = TokenUsage {
            input_tokens: 50,
            output_tokens: 10,
        };
        let usage_ng = TokenUsage {
            input_tokens: 60,
            output_tokens: 12,
        };
        repo.save_ok(&sample_feed(1), "OK", Some(&usage_ok), 10)
            .unwrap();
        repo.save_dlq(&sample_feed(2), "bad", "NG", Some(&usage_ng), 15)
            .unwrap();

        let conn = &repo.conn;
        let (input, output): (i64, i64) = conn
            .query_row(
                "SELECT input_tokens, output_tokens FROM feeds WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(input, 50);
        assert_eq!(output, 10);

        let (input, output): (i64, i64) = conn
            .query_row(
                "SELECT input_tokens, output_tokens FROM dlq_feeds WHERE id = 2",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(input, 60);
        assert_eq!(output, 12);
    }

    #[test]
    fn sqlite_repo_stores_null_tokens_when_usage_is_none() {
        let repo = SqliteRepository::open_in_memory().unwrap();
        repo.save_ok(&sample_feed(1), "OK", None, 10).unwrap();

        let input: Option<i64> = repo
            .conn
            .query_row("SELECT input_tokens FROM feeds WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(input, None);
    }

    #[test]
    fn sqlite_repo_persists_latency_ms_for_ok_and_dlq_feeds() {
        let repo = SqliteRepository::open_in_memory().unwrap();
        repo.save_ok(&sample_feed(1), "OK", None, 123).unwrap();
        repo.save_dlq(&sample_feed(2), "bad", "NG", None, 456)
            .unwrap();

        let latency: i64 = repo
            .conn
            .query_row("SELECT latency_ms FROM feeds WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(latency, 123);

        let latency: i64 = repo
            .conn
            .query_row("SELECT latency_ms FROM dlq_feeds WHERE id = 2", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(latency, 456);
    }

    #[test]
    fn migration_adds_latency_column_to_a_pre_existing_db_without_it() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE feeds (
                id INTEGER PRIMARY KEY,
                user_name TEXT NOT NULL,
                message TEXT NOT NULL,
                time_sent TEXT NOT NULL,
                judged_at TEXT NOT NULL,
                input_tokens INTEGER,
                output_tokens INTEGER,
                expected_label TEXT
            );
            CREATE TABLE dlq_feeds (
                id INTEGER PRIMARY KEY,
                user_name TEXT NOT NULL,
                message TEXT NOT NULL,
                time_sent TEXT NOT NULL,
                reason TEXT NOT NULL,
                judged_at TEXT NOT NULL,
                input_tokens INTEGER,
                output_tokens INTEGER,
                expected_label TEXT
            );",
        )
        .unwrap();

        let repo = SqliteRepository { conn };
        repo.init_schema().unwrap();

        // マイグレーション後はlatency_msを使ったINSERTが成功するはず。
        repo.save_ok(&sample_feed(1), "OK", None, 42).unwrap();
        assert_eq!(repo.count_ok().unwrap(), 1);
    }

    #[test]
    fn reset_clears_both_tables() {
        let repo = SqliteRepository::open_in_memory().unwrap();
        repo.save_ok(&sample_feed(1), "OK", None, 10).unwrap();
        repo.save_dlq(&sample_feed(2), "bad", "NG", None, 15)
            .unwrap();
        assert_eq!(repo.count_ok().unwrap(), 1);
        assert_eq!(repo.count_dlq().unwrap(), 1);

        repo.reset().unwrap();

        assert_eq!(repo.count_ok().unwrap(), 0);
        assert_eq!(repo.count_dlq().unwrap(), 0);
    }

    #[test]
    fn reset_allows_reusing_ids_that_previously_conflicted() {
        // 実機検証で確認した不具合の回帰テスト: reset() 無しで同じidを再度
        // save_ok/save_dlqすると UNIQUE constraint エラーになっていた。
        let repo = SqliteRepository::open_in_memory().unwrap();
        repo.save_ok(&sample_feed(1), "OK", None, 10).unwrap();
        repo.reset().unwrap();

        // reset後なら同じid=1を再利用しても衝突しない。
        repo.save_ok(&sample_feed(1), "OK", None, 10).unwrap();
        assert_eq!(repo.count_ok().unwrap(), 1);
    }

    #[test]
    fn migration_adds_token_columns_to_a_pre_existing_db_without_them() {
        // トークン列追加前のスキーマで作られたDBを模擬し、initSchemaが
        // 既存DBに対しても列を追加できる（マイグレーションが効く）ことを確認する。
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE feeds (
                id INTEGER PRIMARY KEY,
                user_name TEXT NOT NULL,
                message TEXT NOT NULL,
                time_sent TEXT NOT NULL,
                judged_at TEXT NOT NULL
            );
            CREATE TABLE dlq_feeds (
                id INTEGER PRIMARY KEY,
                user_name TEXT NOT NULL,
                message TEXT NOT NULL,
                time_sent TEXT NOT NULL,
                reason TEXT NOT NULL,
                judged_at TEXT NOT NULL
            );",
        )
        .unwrap();

        let repo = SqliteRepository { conn };
        repo.init_schema().unwrap();

        // マイグレーション後は新しい列を使ったINSERTが成功するはず。
        repo.save_ok(&sample_feed(1), "OK", None, 10).unwrap();
        assert_eq!(repo.count_ok().unwrap(), 1);
    }

    #[test]
    fn in_memory_repo_tracks_separately() {
        let repo = InMemoryRepository::default();
        repo.save_ok(&sample_feed(1), "OK", None, 10).unwrap();
        repo.save_dlq(&sample_feed(2), "bad", "NG", None, 15)
            .unwrap();
        assert_eq!(repo.ok_feeds.lock().unwrap().len(), 1);
        assert_eq!(repo.dlq_feeds.lock().unwrap().len(), 1);
        assert_eq!(repo.dlq_feeds.lock().unwrap()[0].1, "bad");
        assert_eq!(repo.dlq_feeds.lock().unwrap()[0].3, "NG");
    }

    #[test]
    fn in_memory_repo_tracks_token_usage() {
        let repo = InMemoryRepository::default();
        let usage = TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
        };
        repo.save_ok(&sample_feed(1), "OK", Some(&usage), 10)
            .unwrap();
        assert_eq!(repo.ok_feeds.lock().unwrap()[0].1, Some(usage));
    }
}
