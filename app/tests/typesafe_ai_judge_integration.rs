//! `TypeSafe AI`（System One）連携の統合テスト。
//!
//! 実際の `api.typesafe.ai` には依存せず、`wiremock` でドキュメント記載の
//! レスポンス形式（`docs.typesafe.ai/api.md` を直接取得して確認済み）を模擬する。

use feed_risk_checker::api::judge::{ComplianceJudge, JudgeError, JudgeOutcome, JudgeVerdict};
use feed_risk_checker::api::typesafe_judge::TypeSafeAiJudge;
use feed_risk_checker::core::feed::{Feed, FeedId, MessageBody, TimeSentUtc, UserName};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sample_feed() -> Feed {
    Feed::new(
        FeedId::new(1),
        UserName::new("tanumaru").unwrap(),
        MessageBody::new("Sharing today's project progress update.").unwrap(),
        TimeSentUtc::new(chrono::Utc::now()),
    )
}

/// `reqwest::blocking` を使うため、tokioランタイムの外側の素のOSスレッドで実行する
/// （`lm_studio_judge_integration.rs` と同じ理由）。
fn judge_on_blocking_thread(
    base_url: String,
    model: &'static str,
    api_key: &'static str,
    retry_count: u32,
    feed: Feed,
) -> Result<JudgeOutcome, JudgeError> {
    std::thread::spawn(move || {
        let judge = TypeSafeAiJudge::new(base_url, model, api_key, retry_count);
        judge.judge(&feed)
    })
    .join()
    .expect("blocking judge thread panicked")
}

#[tokio::test]
async fn judge_parses_ok_choice_from_mocked_typesafe_ai() {
    let server = MockServer::start().await;
    let body = json!({
        "model": "jev-1.13.0",
        "answers": {
            "compliance_check": {"type": "choice", "choice": "ok", "probabilities": {"ok": 0.98}, "confidence": 0.98}
        },
        "usage": {"input_tokens": 50, "output_tokens": 10}
    });
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(header("Authorization", "Bearer test-key-123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let outcome =
        judge_on_blocking_thread(server.uri(), "jev-latest", "test-key-123", 0, sample_feed())
            .expect("judge call should succeed");

    assert_eq!(outcome.verdict, JudgeVerdict::Ok);
    let usage = outcome.usage.expect("usage should be parsed");
    assert_eq!(usage.input_tokens, 50);
    assert_eq!(usage.output_tokens, 10);
}

#[tokio::test]
async fn judge_parses_violation_choice_with_reason_from_mocked_typesafe_ai() {
    let server = MockServer::start().await;
    let body = json!({
        "model": "jev-1.13.0",
        "answers": {
            "compliance_check": {
                "type": "choice",
                "choice": "insider_trading",
                "probabilities": {"insider_trading": 0.91},
                "confidence": 0.91
            }
        },
        "usage": {"input_tokens": 60, "output_tokens": 12}
    });
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let outcome = judge_on_blocking_thread(server.uri(), "jev-latest", "k", 0, sample_feed())
        .expect("judge call should succeed");

    assert_eq!(
        outcome.verdict,
        JudgeVerdict::Ng {
            reason: "insider_trading (confidence=0.91)".to_string()
        }
    );
}

#[tokio::test]
async fn judge_returns_error_on_401_unauthorized() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": "Missing or invalid API key"
        })))
        .mount(&server)
        .await;

    let result = judge_on_blocking_thread(server.uri(), "jev-latest", "bad-key", 0, sample_feed());

    assert!(matches!(result, Err(JudgeError::Http(_))));
}

#[tokio::test]
async fn judge_returns_error_on_422_validation_failure() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({
            "error": "questions.compliance_check.criteria is required"
        })))
        .mount(&server)
        .await;

    let result = judge_on_blocking_thread(server.uri(), "jev-latest", "k", 0, sample_feed());

    assert!(matches!(result, Err(JudgeError::Http(_))));
}

#[tokio::test]
async fn judge_retries_with_backoff_on_429_and_eventually_fails_after_exhausting_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": "rate limit exceeded"
        })))
        // retry_count=1 -> 計2回呼ばれるはず。
        .expect(2)
        .mount(&server)
        .await;

    // retry_count=1: バックオフ(200ms)を1回挟んで2回試行し、最終的に失敗する。
    let result = judge_on_blocking_thread(server.uri(), "jev-latest", "k", 1, sample_feed());

    assert!(matches!(result, Err(JudgeError::Http(_))));
    // `.expect(2)` がモックサーバーのドロップ時に検証される（呼び出し回数が2回であること）。
}

#[tokio::test]
async fn judge_returns_error_when_response_body_is_malformed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;

    let result = judge_on_blocking_thread(server.uri(), "jev-latest", "k", 0, sample_feed());

    assert!(matches!(result, Err(JudgeError::InvalidResponse(_))));
}
