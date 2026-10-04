//! LM Studio（OpenAI互換API）連携の統合テスト。
//!
//! 実際のLM Studio／Kevモデルには依存せず、`wiremock` でHTTPサーバーを模擬し、
//! `LmStudioJudge` が実際にHTTP経由でリクエストを送信し、レスポンスを正しく解釈できるかを検証する。
//!
//! 注意: `LmStudioJudge` は内部で `reqwest::blocking::Client`（独自のtokioランタイムを
//! 内包する）を使うため、呼び出しは必ず素の `std::thread::spawn` 上で行う。
//! `tokio::task::spawn_blocking` で呼ぶと、ブロッキングクライアント内部のランタイムを
//! 既存のtokioランタイムのコンテキスト内でdropすることになりpanicする
//! ("Cannot drop a runtime in a context where blocking is not allowed")。

use feed_risk_checker::api::judge::{
    ComplianceJudge, JudgeError, JudgeOutcome, JudgeVerdict, LmStudioJudge,
};
use feed_risk_checker::core::feed::{Feed, FeedId, MessageBody, TimeSentUtc, UserName};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sample_feed() -> Feed {
    Feed::new(
        FeedId::new(1),
        UserName::new("tanumaru").unwrap(),
        MessageBody::new("本日の進捗を共有します").unwrap(),
        TimeSentUtc::new(chrono::Utc::now()),
    )
}

/// `reqwest::blocking::Client` を使う `LmStudioJudge` の構築と呼び出しを、
/// tokioランタイムの外側の素のOSスレッドで完結させるヘルパー。
///
/// `LmStudioJudge::new` の呼び出しも含めてスレッド内に閉じ込める必要がある。
/// 構築だけを呼び出し元（tokioランタイム上のテスト本体スレッド）で行うと、
/// 内部の `reqwest::blocking::Client` が暗黙に生成するランタイムが
/// 周囲の非同期コンテキストと衝突し、後のdrop時にpanicする。
fn judge_on_blocking_thread(
    base_url: String,
    model: &'static str,
    retry_count: u32,
    feed: Feed,
) -> Result<JudgeOutcome, JudgeError> {
    std::thread::spawn(move || {
        let judge = LmStudioJudge::new(base_url, model, retry_count);
        judge.judge(&feed)
    })
    .join()
    .expect("blocking judge thread panicked")
}

#[tokio::test]
async fn judge_parses_ok_verdict_from_mocked_lm_studio() {
    let server = MockServer::start().await;
    let body = json!({
        "choices": [{"message": {"content": "{\"verdict\": \"OK\", \"reason\": \"no issue\"}"}}]
    });
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let outcome = judge_on_blocking_thread(server.uri(), "Kev", 0, sample_feed())
        .expect("judge call should succeed");

    assert_eq!(outcome.verdict, JudgeVerdict::Ok);
}

#[tokio::test]
async fn judge_parses_ng_verdict_with_reason_from_mocked_lm_studio() {
    let server = MockServer::start().await;
    let body = json!({
        "choices": [{"message": {"content": "{\"verdict\": \"NG\", \"reason\": \"insider trading tip\"}"}}]
    });
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let outcome = judge_on_blocking_thread(server.uri(), "Kev", 0, sample_feed())
        .expect("judge call should succeed");

    assert_eq!(
        outcome.verdict,
        JudgeVerdict::Ng {
            reason: "insider trading tip".to_string()
        }
    );
}

#[tokio::test]
async fn judge_parses_token_usage_when_present_in_response() {
    let server = MockServer::start().await;
    let body = json!({
        "choices": [{"message": {"content": "{\"verdict\": \"OK\"}"}}],
        "usage": {"prompt_tokens": 123, "completion_tokens": 45}
    });
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let outcome = judge_on_blocking_thread(server.uri(), "Kev", 0, sample_feed())
        .expect("judge call should succeed");

    let usage = outcome.usage.expect("usage should be parsed");
    assert_eq!(usage.input_tokens, 123);
    assert_eq!(usage.output_tokens, 45);
}

#[tokio::test]
async fn judge_returns_error_when_lm_studio_keeps_failing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    // retry_count=2 -> 計3回試行してすべて失敗することを期待する。
    let result = judge_on_blocking_thread(server.uri(), "Kev", 2, sample_feed());

    assert!(matches!(result, Err(JudgeError::Http(_))));
}

#[tokio::test]
async fn judge_returns_error_when_lm_studio_returns_malformed_content() {
    let server = MockServer::start().await;
    let body = json!({
        "choices": [{"message": {"content": "this is not json"}}]
    });
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let result = judge_on_blocking_thread(server.uri(), "Kev", 0, sample_feed());

    assert!(matches!(result, Err(JudgeError::InvalidResponse(_))));
}
