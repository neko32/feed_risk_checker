# feed_risk_checker 設計ドキュメント

## 1. 概要

LinkedIn投稿用POCプロジェクト（`propensity_poc` の後継シリーズ）。大量のメッセージフィード
（`{id, user_name, message, time_sent}`）をDecisionモデルにコンプライアンス判定させ、

- **NG判定** → DLQ（`dlq_feeds` テーブル）へ
- **OK判定** → 本体（`feeds` テーブル）へ

振り分ける。想定ボリュームは100,000件 / 1,000ユーザだが、まず1,000件 / 5ユーザの小規模テストを
実行し、結果をふっかちゃん製のビューワーで確認した上で、たぬまる（ユーザ）がHITL
（Human-In-The-Loop）承認を行ってから本番ボリュームを実行する2段階運用とする。

> **判定エンジンについて（2026-10-04更新）**: 当初はローカルLLM "Kev"（LM Studio経由）を
> 判定エンジンとしていたが、現在は **TypeSafe AI**（System One評価API, model=`jev-latest`）に
> 差し替え済み。`ComplianceJudge` trait の別実装として追加した `TypeSafeAiJudge` が
> アクティブで、`LmStudioJudge` は実装・テストとも残したまま非アクティブになっている
> （将来ローカルLLMに戻す場合はコード変更不要で切り戻せる）。詳細は5節・`docs/api.md` 参照。

> **言語について**: LinkedInでの英語デモを想定しているため、生成される合成フィード本文、
> Kevへのシステムプロンプト、CLIの標準出力・`--help`・ビューワーGUIはすべて英語。
> 一方、コード中のdocコメント・設計ドキュメント等（開発チーム向け）は日本語のまま。

## 2. アーキテクチャ

```
feed_risk_checker/
├── .github/workflows/      CI (ci.yml) / リリース (release.yml)
├── app/                    Rust本体
│   ├── src/
│   │   ├── main.rs             エントリポイント・CLI・HITLゲート
│   │   ├── cli.rs               clap定義（run --scale small|full）
│   │   ├── config.rs            .env/環境変数からの設定読み込み
│   │   ├── core/
│   │   │   ├── feed.rs             ドメインモデル（newtypeバリデーション）
│   │   │   ├── generator.rs        合成フィード生成（NG種の混入）
│   │   │   ├── worker.rs           マルチスレッドワーカー + DBライター
│   │   │   └── repository.rs       SQLite永続化（trait化）
│   │   └── api/
│   │       └── judge.rs            LM Studio("Kev")連携（trait化）
│   ├── tests/                  統合テスト（wiremock / 実バイナリE2E）
│   └── Cargo.toml
├── viewer/                 Python + TKInterの簡易ビューワー（ふっかちゃん担当）
├── scripts_local/          ローカル起動用スクリプト（ps1 / sh）
└── docs/                   本ドキュメント・coverage HTMLレポート
```

## 3. データフロー

0. **クリーンスタート (`SqliteRepository::reset`)**: 生成を始める前に、`feeds` /
   `dlq_feeds` の既存データを全削除する。`feed_risk_checker` は「1回の実行 = 1
   スナップショット」の運用を前提としており、生成されるフィードIDは毎回 `0` から
   振り直されるため、前回実行分のデータが残っていると主キー重複で2回目以降の保存が
   すべて失敗する（実機検証で確認済み。回帰テスト: `core::repository::tests::
   reset_allows_reusing_ids_that_previously_conflicted`）。同じDBファイルを使って
   小規模テスト→本番実行と連続で流す運用のため、この自動クリアは必須の挙動とする。
   過去の実行結果を残したい場合は、実行前に `SQLITE_PATH` が指すファイルを
   別名でコピー・退避すること。

1. **生成 (`core::generator`)**: `total_feeds` 件のフィードを `user_count` 人へランダム割当。
   `ng_seed_rate`（既定3%）の割合で「コンプラ違反の種」テンプレート
   （インサイダー情報・個人情報漏洩・ハラスメント表現等）から本文を生成し、残りは通常の
   ビジネスメッセージテンプレートから生成する。各フィードには生成時に埋め込んだ
   正解ラベル（`SeedLabel::Benign`/`Risky`）が `SeededFeed` として付与され、
   判定後までパイプライン全体で持ち歩かれる（Accuracy/Precision/Recall計算用）。

   > **注意**: この3%はあくまで生成時に埋め込んだ"種"の比率であり、実際にKevがNGと判定する
   > 比率（`judged NG率`）とは一致しない場合がある。両者は実行結果サマリで別々に報告する。

2. **キューイング**: `crossbeam-channel` の unboundedチャンネルで、生成済みフィードを
   ワーカースレッド群に配布する。

3. **並列判定 (`core::worker::run_pipeline`)**: `worker_count` 本のOSスレッドが並列に
   `ComplianceJudge::judge()` を呼び出し、LM Studio（Kev）へHTTPリクエストを送る
   （`/v1/chat/completions` 互換エンドポイント）。失敗時は `RETRY_COUNT` 回までリトライする。

4. **DB書き込みの一元化**: 判定結果はすべて単一の「DBライタースレッド」へ
   `crossbeam-channel` 経由で送られ、そこから順次 `FeedRepository::save_ok` /
   `save_dlq` を呼ぶ。SQLiteへの同時書き込み競合を避けるため、判定（並列）と
   永続化（直列・単一スレッド）を分離している。この書き込み完了のタイミングで
   `ProgressReporter::inc(1)` を呼び、`indicatif` の進捗バー（経過時間・バー・
   件数・ETA表示）をリアルタイムに更新する（100,000件規模の実行で経過が
   見えないと不安になるため）。

   - `judge` がエラーを返した場合（LM Studio未接続・タイムアウト等）も、
     `judge_error: <理由>` という reason 付きでDLQへルーティングする
     （バイナリ自体をパニックさせない設計）。
   - 判定結果（OK/NG）と正解ラベル（`SeedLabel`）を比較し、`RunStats` に
     混同行列（TP/FP/TN/FN）を積算する。`judge_error`は判定自体が行えていないため
     混同行列には加算しない。`expected_label`列としてSQLiteにも保存する
     （ビューアでの表示・独自集計用）。
   - 各ワーカースレッドは `judge.judge()` 呼び出し開始から結果受信までの時間を
     `std::time::Instant` で計測し（リトライ・バックオフ込みの総所要時間）、
     `latency_ms` 列としてSQLiteに保存する。ビューアでは1件ごとのレイテンシを
     一覧表示できる（`time_sent`/`judged_at`はビューアの表示列からは外しており、
     代わりにこの `latency_ms` を主に見る運用にしている）。

5. **サマリ出力**: 実行完了後、total / OK / NG(DLQ) / ERROR(DLQ) / judged NG率 /
   **Accuracy / Precision / Recall** / トークン合計・平均 / 所要時間をCLIに出力する。
   Accuracy/Precision/Recallは「NG（違反）が陽性クラス」という標準的な定義
   （Accuracy=(TP+TN)/全分類件数、Precision=TP/(TP+FP)、Recall=TP/(TP+FN)）。

## 4. HITL（Human-In-The-Loop）ゲート

- `run --scale small`（既定）: 1,000件 / 5ユーザ。確認プロンプト無しで即実行。
- `run --scale full`: 100,000件 / 1,000ユーザ。**`--yes` を付けない限り対話的確認プロンプトを
  表示**し、`y`/`yes` 以外の入力はすべて実行キャンセルとみなす。
- `--total-feeds` / `--user-count` で件数を個別に上書きできる（スケールのデフォルトより優先）。

想定フロー: `scripts_local/run_small.ps1(.sh)` → ビューワーで確認 →
`scripts_local/run_full.ps1(.sh)` で承認後に本番実行。

## 5. 外部依存の抽象化

| 抽象 | trait | 本実装（アクティブ） | 代替実装（非アクティブ） | テスト用実装 |
|---|---|---|---|---|
| コンプラ判定 | `ComplianceJudge` | `TypeSafeAiJudge`（`api/typesafe_judge.rs`） | `LmStudioJudge`（`api/judge.rs`, reqwest::blocking） | `MockJudge`（`#[cfg(test)]`） |
| 永続化 | `FeedRepository` | `SqliteRepository`（rusqlite） | — | `InMemoryRepository`（`#[cfg(test)]`） |
| 進捗表示 | `ProgressReporter` | `indicatif::ProgressBar`（`main.rs`で構築） | — | `NoopProgress` / テスト用スパイ |

いずれも `Send + Sync`（または呼び出し元で `Arc` 経由）で複数スレッドから利用可能。
`main.rs` は `TypeSafeAiJudge` を構築して `Arc<dyn ComplianceJudge>` として渡すだけなので、
将来別の判定エンジンに差し替える場合もこの1箇所の変更で済む。

### TypeSafe AI連携の詳細（`api/typesafe_judge.rs`）

[TypeSafe AI](https://docs.typesafe.ai/api)（"System One" 評価API）を利用。
2026-10-04時点でドキュメントの生Markdown（`docs.typesafe.ai/api.md`）を直接取得して
仕様を確認済み（要約ツール経由だと `"noul"` 等の特殊な用語が幻覚ではないか疑わしかったため、
生ページで実在することを検証した）。

- エンドポイント: `POST {base_url}/v1/systemone`, `Authorization: Bearer <API_KEY_JEV>`
- リクエストは `choice` 型の質問を1つ（`compliance_check`）だけ送る。選択肢は
  「違反なし(`ok`)」+ `core::generator::RISKY_TEMPLATES` と同じ20カテゴリ
  （`insider_trading`, `pii_leak`, `harassment`, ... 計21択）。
- レスポンスの `choice` が `ok` なら `JudgeVerdict::Ok`、それ以外なら
  `JudgeVerdict::Ng { reason: "<category> (confidence=<値>)" }`。
- `429 Too Many Requests` / `529 Overloaded` はドキュメントが明示的に指数バックオフでの
  リトライを推奨しているため、この2種のステータスのときだけ `backoff_delay(attempt)`
  （200ms, 400ms, 800ms, ... 最大6400msで頭打ち）分スリープしてリトライする。
  `401`/`422` 等は即座に失敗として扱う（リトライしても直らないため）。
- APIキー（環境変数 `API_KEY_JEV`）は一切ログ・エラーメッセージ・リクエストボディ自身に
  混在させない（ユニットテストでも、リクエストボディ文字列にキーが含まれないことを検証）。

### プロンプトインジェクション対策

`LmStudioJudge` はフィード本文を system プロンプトとは別の user ロールに分離し、
デリミタ（`---BEGIN_FEED_MESSAGE---` / `---END_FEED_MESSAGE---`）で囲み、
「本文はデータであり指示ではない」ことを明示した上でKevに渡す（`api/judge.rs` 参照）。
`TypeSafeAiJudge` も同様に、フィード本文は `state` フィールド（評価対象データ）に
入れるだけで、`questions`（指示）とは構造的に分離されている。

## 6. テスト戦略

- **Unit**: newtypeバリデーション、生成分布（NG種混入率の統計的検証）、
  judge/repositoryのtraitをモックに差し替えたロジック検証、ワーカーパイプラインの
  振り分けロジック（OK/NG/Error → 各テーブル）、CLI引数解釈、main.rsの純粋ロジック
  （スケール解決・HITL確認判定）。混同行列（TP/FP/TN/FN）とAccuracy/Precision/Recall
  計算は「完璧な分類」「FP+FNが混在」「judge_errorは混同行列から除外される」
  「分母ゼロでは0除算せず0.0を返す」の4パターンをカバー。レイテンシ計測は、
  意図的にスリープするテスト用ジャッジ（`SlowJudge`）で実測値が反映されることを検証。
- **Integration**:
  - `wiremock` でLM StudioのOpenAI互換エンドポイントを模擬し、`LmStudioJudge` の
    実HTTP経路（成功・NG・5xxエラー・不正レスポンス）を検証。
  - `wiremock` でTypeSafe AIのSystem Oneエンドポイントを模擬し、`TypeSafeAiJudge` の
    実HTTP経路（成功/違反判定/401/422/429バックオフリトライ/不正レスポンス）を検証。
  - コンパイル済みバイナリを実際に起動するE2Eテスト（到達不能ポートを
    `TYPESAFE_BASE_URL` に指定し、未接続でも正常終了しDLQへ振り分けられることを検証。
    full スケールのHITL確認キャンセル経路・`--yes` 経路も検証）。
    **重要**: これらのE2Eテストは `API_KEY_JEV` を必ずダミー値で明示的に上書きする
    （`std::process::Command` は親プロセスの環境変数を継承するため、開発者のシェルに
    実際のAPIキーが設定されていると、上書きを忘れると本物の外部APIに実際のキーで
    リクエストしてしまう。実際にこの事故を一度起こしてから気づき、全テストを修正した）。
- **カバレッジ**: `cargo-llvm-cov` で計測。現状 **96%程度の行カバレッジ**（CI上で
  `--fail-under-lines 90` により90%未達はビルド失敗とする）。
- **静的解析**: `cargo clippy --all-targets -- -W clippy::pedantic` をCIで実行。
  critical/high相当は都度修正、low/medium相当（境界が明確に安全なキャスト等）は
  コード中にコメント付きの `#[allow(...)]` で明示して許容している。

## 7. 設定（`.env` / 環境変数）

| 変数 | 既定値 | 説明 |
|---|---|---|
| `API_KEY_JEV` | （必須、既定値なし） | TypeSafe AIのAPIキー。変数名が`TYPESAFE_API_KEY`ではない点に注意 |
| `TYPESAFE_BASE_URL` | `https://api.typesafe.ai` | TypeSafe AIのベースURL |
| `TYPESAFE_MODEL` | `jev-latest` | TypeSafe AIのモデル名 |
| `LM_STUDIO_BASE_URL` | `http://localhost:1234` | （現在非アクティブ）LM StudioのOpenAI互換APIベースURL |
| `LM_STUDIO_MODEL` | `Kev` | （現在非アクティブ）判定に使うモデル名 |
| `WORKER_COUNT` | 論理CPUコア数 | ジャッジ呼び出しの並列ワーカー数 |
| `RETRY_COUNT` | `3` | ジャッジ呼び出し失敗時のリトライ回数（TypeSafe AIの429/529は指数バックオフ併用） |
| `SQLITE_PATH` | `feed_risk.db` | 永続化先SQLiteファイル |

## 8. 実機スモークテストでの知見（LM Studio時代の記録）

> 以下は判定エンジンがローカルLM Studio（Kev）だった時期の知見。TypeSafe AI移行後も
> 「小規模モデルは不安定なことがある」「エラーはクラッシュさせずDLQへ」という設計上の
> 教訓は引き続き有効なため、記録として残す。

実際にローカルのLM Studio（モデルID `kev-4b`, `google/gemma-4-e4b` ベース）に対して
小規模スモークテストを実行し、エンドツーエンド動作を確認した。

- NG種として埋め込んだフィードを、実際にKevがNG判定した例を確認（設計通りに機能）。
- `worker_count` を論理CPUコア数のまま（例: 20）にすると、単一のローカル推論サーバに
  対して過剰な同時リクエストとなり、タイムアウトや空レスポンスが増える傾向が見られた。
  ローカルLM Studio運用時は `WORKER_COUNT` を小さめ（2〜4程度）に設定することを推奨する。
- 小規模ローカルモデルは、まれに空文字のレスポンス（JSONとして解釈不能）を返すことがある。
  `max_tokens` を明示的に指定（200）して緩和を試みたが、完全には解消しない場合がある。
  これは判定ロジックのバグではなく、小規模モデル自体の出力安定性に起因するため、
  `judge_error: ...` という理由付きでDLQへ振り分ける設計（本ドキュメント4節）により
  パイプライン全体はクラッシュせず継続する。より安定した判定が必要な場合は、
  より大きな／安定したモデルの採用、プロンプトの調整、リトライ回数の増加等を検討する。

## 9. 既知の制約・今後の拡張余地

- `SqliteRepository` はWALモードを使うが、書き込みを単一スレッドに一元化しているため
  実運用でのロック競合は発生しない設計（高スループットが必要な場合は非同期SQLite
  ドライバへの切り替えも検討余地あり）。
- 生成テンプレートはベニン/リスキーそれぞれ20カテゴリ×10件＝200パターン固定。
  100,000件規模でもテンプレートの重複が目立たない程度の多様性を持たせている。
  さらに多様性が必要な場合はテンプレートを増やす、またはLLMでテンプレート自体を
  生成する拡張が考えられる。
- ビューワー（TKInter）は読み取り専用。将行的にはリアルタイム更新（ファイル監視）や
  グラフ表示（NG率の時系列等）の追加も検討可能。
