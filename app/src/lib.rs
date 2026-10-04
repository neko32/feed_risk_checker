//! `feed_risk_checker`: `LinkedIn` POC向け、大量メッセージフィードのコンプラ判定パイプライン。
//!
//! 100,000件規模のフィードを生成し、マルチスレッドワーカーがローカルLLM "Kev"
//! （LM Studio経由）に判定を依頼、NG判定はDLQへ、OK判定はSQLiteへ振り分ける。

pub mod api;
pub mod cli;
pub mod config;
pub mod core;
