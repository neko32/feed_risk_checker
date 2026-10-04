//! 実行時設定。 `.env` / 環境変数から読み込み、CLI引数で上書きされる前のデフォルトを提供する。

use std::env;

use crate::api::typesafe_judge;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// ローカルLM Studio("Kev")連携用。現在は `TypeSafeAiJudge` に差し替え済みのため
    /// アクティブには使われていないが、`LmStudioJudge` 自体は trait実装として残しており、
    /// 将来切り戻す場合にこの設定をそのまま使える。
    pub lm_studio_base_url: String,
    pub lm_studio_model: String,
    /// `TypeSafe AI`（System One）のベースURL。
    pub typesafe_base_url: String,
    /// `TypeSafe AI`のモデル名（既定: `jev-latest`）。
    pub typesafe_model: String,
    /// `TypeSafe AI`のAPIキー。環境変数名は `API_KEY_JEV`（ユーザから共有された名称）。
    /// 未設定の場合は `None`（実際に `TypeSafeAiJudge` を構築する側で必須チェックする）。
    pub typesafe_api_key: Option<String>,
    pub worker_count: usize,
    pub retry_count: u32,
    pub sqlite_path: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            lm_studio_base_url: env::var("LM_STUDIO_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:1234".to_string()),
            lm_studio_model: env::var("LM_STUDIO_MODEL").unwrap_or_else(|_| "Kev".to_string()),
            typesafe_base_url: env::var("TYPESAFE_BASE_URL")
                .unwrap_or_else(|_| typesafe_judge::DEFAULT_BASE_URL.to_string()),
            typesafe_model: env::var("TYPESAFE_MODEL")
                .unwrap_or_else(|_| typesafe_judge::DEFAULT_MODEL.to_string()),
            // NOTE: 環境変数名は `TYPESAFE_API_KEY` ではなく `API_KEY_JEV`。
            typesafe_api_key: env::var("API_KEY_JEV").ok(),
            worker_count: env::var("WORKER_COUNT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(default_worker_count),
            retry_count: env::var("RETRY_COUNT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3),
            sqlite_path: env::var("SQLITE_PATH").unwrap_or_else(|_| "feed_risk.db".to_string()),
        }
    }
}

fn default_worker_count() -> usize {
    std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_worker_count_is_at_least_one() {
        assert!(default_worker_count() >= 1);
    }

    #[test]
    fn config_falls_back_to_defaults_when_env_missing() {
        // NOTE: テスト実行環境の環境変数に依存しないよう、既知のキーをクリアしてから検証する。
        for key in [
            "LM_STUDIO_BASE_URL",
            "LM_STUDIO_MODEL",
            "TYPESAFE_BASE_URL",
            "TYPESAFE_MODEL",
            "API_KEY_JEV",
            "WORKER_COUNT",
            "RETRY_COUNT",
            "SQLITE_PATH",
        ] {
            unsafe { env::remove_var(key) };
        }
        let config = Config::from_env();
        assert_eq!(config.lm_studio_base_url, "http://localhost:1234");
        assert_eq!(config.lm_studio_model, "Kev");
        assert_eq!(config.typesafe_base_url, typesafe_judge::DEFAULT_BASE_URL);
        assert_eq!(config.typesafe_model, typesafe_judge::DEFAULT_MODEL);
        assert_eq!(config.typesafe_api_key, None);
        assert_eq!(config.retry_count, 3);
        assert_eq!(config.sqlite_path, "feed_risk.db");

        // 同じテスト内で続けて検証する（別テストに分けると、cargo testのデフォルト並列実行で
        // 他のテストと同じ環境変数 `API_KEY_JEV` を同時に書き換えてしまい、競合（flaky）の
        // 原因になるため）。
        unsafe { env::set_var("API_KEY_JEV", "test-secret-value") };
        let config_with_key = Config::from_env();
        assert_eq!(
            config_with_key.typesafe_api_key,
            Some("test-secret-value".to_string())
        );
        unsafe { env::remove_var("API_KEY_JEV") };
    }
}
