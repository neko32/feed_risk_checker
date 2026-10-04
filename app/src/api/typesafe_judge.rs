//! `TypeSafe AI`（<https://docs.typesafe.ai/api>, "System One" evaluation endpoint）との
//! コンプライアンス判定連携。ローカルLM Studio("Kev")の代替実装として追加した。
//!
//! # API仕様（2026-10-04に `docs.typesafe.ai/api.md` の生Markdownを直接取得して確認済み）
//! - `POST {base_url}/v1/systemone`、`Authorization: Bearer <API_KEY>`
//! - リクエスト: `{"state": <評価対象>, "model": <モデル名>, "questions": {<id>: Question}}`
//! - 本実装では `choice` 型の質問を1つだけ使う。選択肢は「コンプラ違反なし(ok)」+
//!   本プロジェクトの20カテゴリ（`core::generator` のRiskyテンプレートと同じ分類）。
//!   `choice` の値がそのまま `JudgeVerdict::Ng` の `reason` になる。
//! - エラー: `401`（認証）, `422`（バリデーション）, `429`（レート制限）, `529`（過負荷）。
//!   ドキュメントが明示的に指数バックオフでのリトライを推奨しているため、
//!   `429`/`529` のときだけ `backoff_delay` 分スリープしてリトライする。
//!
//! APIキー（環境変数 `API_KEY_JEV`）は一切ログ・エラーメッセージに含めない。

use serde_json::json;

use crate::core::feed::Feed;

use super::judge::{ComplianceJudge, JudgeError, JudgeOutcome, JudgeVerdict, TokenUsage};

/// `TypeSafe AI`のデフォルトモデル（ドキュメント記載のフラグシップモデル）。
pub const DEFAULT_MODEL: &str = "jev-latest";

/// `TypeSafe AI`のデフォルトベースURL。
pub const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";

/// `compliance_check` 質問の選択肢。先頭の `ok` 以外は `core::generator::RISKY_TEMPLATES`
/// で使っている20カテゴリと対応させている。
const CATEGORY_CRITERIA: &[(&str, &str)] = &[
    (
        "ok",
        "No compliance violation. Normal business communication.",
    ),
    (
        "insider_trading",
        "Leaking insider or non-public financial information.",
    ),
    (
        "pii_leak",
        "Sharing personal identifiable information without authorization.",
    ),
    (
        "credential_leak",
        "Leaking passwords, API keys, or other credentials.",
    ),
    (
        "harassment",
        "Harassment or bullying directed at a colleague.",
    ),
    (
        "discrimination",
        "Discriminatory language or discriminatory hiring practices.",
    ),
    (
        "confidential_info_leak",
        "Leaking confidential contract or client information.",
    ),
    (
        "illegal_competitor_info",
        "Sharing illegally obtained competitor information.",
    ),
    (
        "defamation",
        "Publicly naming, shaming, or defaming a colleague.",
    ),
    ("safety_coverup", "Covering up a workplace safety incident."),
    ("bribery", "Bribery or kickbacks."),
    (
        "trade_secret_leak",
        "Leaking a trade secret or proprietary process.",
    ),
    (
        "regulatory_violation",
        "Admission of a regulatory compliance violation.",
    ),
    (
        "fraud",
        "Admission of fraud, such as falsified billing or inflated numbers.",
    ),
    (
        "customer_data_misuse",
        "Misuse of customer data beyond its authorized purpose.",
    ),
    (
        "price_collusion",
        "Price collusion or coordination with a competitor.",
    ),
    ("tax_evasion", "A tax evasion scheme."),
    (
        "retaliation_threat",
        "Threatening retaliation against a colleague.",
    ),
    (
        "record_falsification",
        "Falsifying records, logs, or approvals.",
    ),
    ("ip_theft", "Theft or misuse of intellectual property."),
    (
        "unauthorized_access",
        "Unauthorized system access or doxxing of personal information.",
    ),
];

const INSTRUCTIONS: &str = "Does this posted internal message violate corporate compliance \
policy? If it violates policy, choose the single category that best fits. If it does not \
violate policy, choose \"ok\".";

/// `TypeSafe AI`（System One）経由で判定を依頼する実装。
pub struct TypeSafeAiJudge {
    client: reqwest::blocking::Client,
    base_url: String,
    model: String,
    api_key: String,
    retry_count: u32,
}

/// 1回のHTTP試行の結果。`429`/`529` のときだけバックオフしてリトライする区別のため、
/// 通常の `Result` ではなく3値で表現する。
enum AttemptOutcome {
    Success(JudgeOutcome),
    /// レート制限/過負荷。バックオフ後にリトライする。
    RetryWithBackoff(JudgeError),
    /// それ以外の失敗。即座にリトライ（バックオフ無し）するか、リトライ尽きたら返す。
    Fail(JudgeError),
}

impl TypeSafeAiJudge {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: impl Into<String>,
        retry_count: u32,
    ) -> Self {
        Self {
            client: reqwest::blocking::Client::new(),
            base_url: base_url.into(),
            model: model.into(),
            api_key: api_key.into(),
            retry_count,
        }
    }

    fn endpoint_url(&self) -> String {
        format!("{}/v1/systemone", self.base_url.trim_end_matches('/'))
    }

    fn request_body(&self, feed: &Feed) -> serde_json::Value {
        let criteria: serde_json::Map<String, serde_json::Value> = CATEGORY_CRITERIA
            .iter()
            .map(|(key, desc)| ((*key).to_string(), json!(desc)))
            .collect();

        json!({
            "state": feed.message.as_str(),
            "model": self.model,
            "questions": {
                "compliance_check": {
                    "type": "choice",
                    "instructions": INSTRUCTIONS,
                    "criteria": criteria,
                }
            }
        })
    }

    fn call_once(&self, feed: &Feed) -> AttemptOutcome {
        let response = match self
            .client
            .post(self.endpoint_url())
            .bearer_auth(&self.api_key)
            .json(&self.request_body(feed))
            .send()
        {
            Ok(r) => r,
            Err(e) => return AttemptOutcome::Fail(JudgeError::Http(e.to_string())),
        };

        let status = status_code(&response);

        if status == 429 || status == 529 {
            let detail = response.text().unwrap_or_default();
            return AttemptOutcome::RetryWithBackoff(JudgeError::Http(format!(
                "HTTP {status}: {detail}"
            )));
        }

        if !(200..300).contains(&status) {
            let detail = response.text().unwrap_or_default();
            return AttemptOutcome::Fail(JudgeError::Http(format!("HTTP {status}: {detail}")));
        }

        let body: serde_json::Value = match response.json() {
            Ok(b) => b,
            Err(e) => {
                return AttemptOutcome::Fail(JudgeError::InvalidResponse(format!(
                    "response body is not JSON: {e}"
                )));
            }
        };

        match parse_response(&body) {
            Ok(verdict) => AttemptOutcome::Success(JudgeOutcome {
                verdict,
                usage: parse_usage(&body),
            }),
            Err(e) => AttemptOutcome::Fail(e),
        }
    }
}

fn status_code(response: &reqwest::blocking::Response) -> u16 {
    response.status().as_u16()
}

impl ComplianceJudge for TypeSafeAiJudge {
    fn judge(&self, feed: &Feed) -> Result<JudgeOutcome, JudgeError> {
        let mut last_err = None;
        for attempt in 0..=self.retry_count {
            match self.call_once(feed) {
                AttemptOutcome::Success(outcome) => return Ok(outcome),
                AttemptOutcome::RetryWithBackoff(e) => {
                    last_err = Some(e);
                    if attempt < self.retry_count {
                        std::thread::sleep(backoff_delay(attempt));
                    }
                }
                AttemptOutcome::Fail(e) => {
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.expect("retry loop always runs at least once"))
    }
}

/// `usage.input_tokens` / `usage.output_tokens` を読む。欠落・型不正の場合は `None`
/// （トークン集計はベストエフォート。判定自体のエラーには影響させない）。
fn parse_usage(body: &serde_json::Value) -> Option<TokenUsage> {
    let input_tokens = body["usage"]["input_tokens"].as_u64()?;
    let output_tokens = body["usage"]["output_tokens"].as_u64()?;
    Some(TokenUsage {
        input_tokens,
        output_tokens,
    })
}

/// `429`/`529` 時の指数バックオフ待機時間。`200ms * 2^attempt`、6回目以降は頭打ち。
fn backoff_delay(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_millis(200 * 2u64.pow(attempt.min(5)))
}

/// `{"model": ..., "answers": {"compliance_check": {"type": "choice", "choice": ..., "confidence": ...}}, "usage": {...}}`
/// を `JudgeVerdict` に変換する。
fn parse_response(body: &serde_json::Value) -> Result<JudgeVerdict, JudgeError> {
    let answer = &body["answers"]["compliance_check"];
    let choice = answer["choice"].as_str().ok_or_else(|| {
        JudgeError::InvalidResponse(format!("missing answers.compliance_check.choice: {body}"))
    })?;

    if choice.eq_ignore_ascii_case("ok") {
        return Ok(JudgeVerdict::Ok);
    }

    let reason = match answer["confidence"].as_f64() {
        Some(confidence) => format!("{choice} (confidence={confidence:.2})"),
        None => choice.to_string(),
    };
    Ok(JudgeVerdict::Ng { reason })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::feed::{FeedId, MessageBody, TimeSentUtc, UserName};

    fn sample_feed() -> Feed {
        Feed::new(
            FeedId::new(1),
            UserName::new("tanumaru").unwrap(),
            MessageBody::new("hello").unwrap(),
            TimeSentUtc::new(chrono::Utc::now()),
        )
    }

    #[test]
    fn request_body_has_expected_shape() {
        let judge = TypeSafeAiJudge::new(DEFAULT_BASE_URL, DEFAULT_MODEL, "secret-key", 0);
        let body = judge.request_body(&sample_feed());

        assert_eq!(body["state"], "hello");
        assert_eq!(body["model"], DEFAULT_MODEL);
        assert_eq!(body["questions"]["compliance_check"]["type"], "choice");
        // APIキーがリクエストボディ自体に紛れ込んでいないことも確認する。
        assert!(!body.to_string().contains("secret-key"));
    }

    #[test]
    fn request_body_contains_all_21_categories() {
        let judge = TypeSafeAiJudge::new(DEFAULT_BASE_URL, DEFAULT_MODEL, "k", 0);
        let body = judge.request_body(&sample_feed());
        let criteria = body["questions"]["compliance_check"]["criteria"]
            .as_object()
            .unwrap();
        assert_eq!(criteria.len(), 21);
        assert!(criteria.contains_key("ok"));
        assert!(criteria.contains_key("insider_trading"));
        assert!(criteria.contains_key("unauthorized_access"));
    }

    #[test]
    fn parse_response_ok_choice_yields_ok_verdict() {
        let body = json!({
            "model": "jev-1.13.0",
            "answers": {"compliance_check": {"type": "choice", "choice": "ok", "probabilities": {}, "confidence": 0.99}},
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });
        assert_eq!(parse_response(&body).unwrap(), JudgeVerdict::Ok);
    }

    #[test]
    fn parse_response_ok_choice_is_case_insensitive() {
        let body = json!({
            "answers": {"compliance_check": {"choice": "OK", "confidence": 0.5}}
        });
        assert_eq!(parse_response(&body).unwrap(), JudgeVerdict::Ok);
    }

    #[test]
    fn parse_response_violation_choice_yields_ng_with_reason_and_confidence() {
        let body = json!({
            "answers": {"compliance_check": {"type": "choice", "choice": "insider_trading", "confidence": 0.87}}
        });
        let verdict = parse_response(&body).unwrap();
        assert_eq!(
            verdict,
            JudgeVerdict::Ng {
                reason: "insider_trading (confidence=0.87)".to_string()
            }
        );
    }

    #[test]
    fn parse_response_missing_choice_field_is_invalid_response() {
        let body = json!({ "answers": { "compliance_check": { "confidence": 0.5 } } });
        let err = parse_response(&body).unwrap_err();
        assert!(matches!(err, JudgeError::InvalidResponse(_)));
    }

    #[test]
    fn backoff_delay_doubles_each_attempt_and_caps_out() {
        assert_eq!(backoff_delay(0), std::time::Duration::from_millis(200));
        assert_eq!(backoff_delay(1), std::time::Duration::from_millis(400));
        assert_eq!(backoff_delay(2), std::time::Duration::from_millis(800));
        // attempt>=5 で頭打ち（200ms * 2^5 = 6400ms）になることを確認する。
        assert_eq!(backoff_delay(5), backoff_delay(10));
    }

    #[test]
    fn parse_usage_reads_input_and_output_tokens() {
        let body = json!({
            "answers": {"compliance_check": {"choice": "ok", "confidence": 1.0}},
            "usage": {"input_tokens": 60, "output_tokens": 12}
        });
        let usage = parse_usage(&body).unwrap();
        assert_eq!(usage.input_tokens, 60);
        assert_eq!(usage.output_tokens, 12);
        assert_eq!(usage.total(), 72);
    }

    #[test]
    fn parse_usage_returns_none_when_missing() {
        let body = json!({"answers": {}});
        assert_eq!(parse_usage(&body), None);
    }
}
