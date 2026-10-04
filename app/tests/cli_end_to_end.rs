//! CLIバイナリのエンドツーエンドテスト。
//!
//! 実際の`TypeSafe AI`には依存しない。到達不能なポートを `TYPESAFE_BASE_URL` に指定し、
//! 判定呼び出しがすべて `JudgeError` となってDLQへ振り分けられる経路を検証する
//! （`run_pipeline` はジャッジエラーをパニックさせずDLQ送りにする設計のため、
//! `TypeSafe AI`未接続でもバイナリ自体は正常終了する）。
//!
//! 重要: `API_KEY_JEV` は開発者のシェル環境に実際の値が設定されている場合があり、
//! `std::process::Command` はデフォルトで親プロセスの環境変数を子プロセスに継承する。
//! これらのテストでは `.env("API_KEY_JEV", "...")` で必ずダミー値に上書きし、かつ
//! `TYPESAFE_BASE_URL` も到達不能なアドレスに固定することで、実際の外部APIへの
//! 本物のAPIキーでのリクエストが絶対に発生しないようにしている。

use std::io::Write;
use std::process::{Command, Stdio};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_feed_risk_checker"))
}

#[test]
fn small_scale_run_completes_and_writes_sqlite_even_when_lm_studio_unreachable() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let db_path = dir.path().join("test_feed_risk.db");

    let output = bin()
        .args([
            "run",
            "--scale",
            "small",
            "--total-feeds",
            "20",
            "--user-count",
            "3",
        ])
        .env("SQLITE_PATH", db_path.to_str().expect("path is valid utf8"))
        // ポート9は通常未使用のため接続即拒否される想定。実APIへは絶対に到達しない。
        .env("TYPESAFE_BASE_URL", "http://127.0.0.1:9")
        // シェル環境に実際のAPI_KEY_JEVが設定されていても、ここで必ずダミー値に上書きする。
        .env("API_KEY_JEV", "dummy-key-for-e2e-test")
        .env("RETRY_COUNT", "0")
        .env("WORKER_COUNT", "2")
        .output()
        .expect("failed to execute binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "process did not exit successfully; stdout={stdout}, stderr={stderr}"
    );
    assert!(stdout.contains("Generation complete"));
    assert!(stdout.contains("Run Summary"));
    assert!(stdout.contains("ERROR (DLQ)    : 20"));
    assert!(db_path.exists(), "sqlite db file should have been created");
}

#[test]
fn full_scale_run_is_cancelled_when_confirmation_is_declined() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let db_path = dir.path().join("test_feed_risk_full.db");

    let mut child = bin()
        .args(["run", "--scale", "full"])
        .env("SQLITE_PATH", db_path.to_str().expect("path is valid utf8"))
        // このテストは確認プロンプトをキャンセルするのでジャッジには到達しないはずだが、
        // 将来コードが変わっても実APIに触れないよう念のためダミー値で固定しておく。
        .env("TYPESAFE_BASE_URL", "http://127.0.0.1:9")
        .env("API_KEY_JEV", "dummy-key-for-e2e-test")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn binary");

    child
        .stdin
        .take()
        .expect("stdin should be piped")
        .write_all(b"n\n")
        .expect("failed to write to child stdin");

    let output = child.wait_with_output().expect("failed to wait for child");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Cancelled"));
    assert!(
        !db_path.exists(),
        "sqlite db should not be created when the run is cancelled"
    );
}

#[test]
fn full_scale_run_with_yes_flag_skips_confirmation_prompt() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let db_path = dir.path().join("test_feed_risk_full_yes.db");

    let output = bin()
        .args([
            "run",
            "--scale",
            "full",
            "--total-feeds",
            "10",
            "--user-count",
            "2",
            "--yes",
        ])
        .env("SQLITE_PATH", db_path.to_str().expect("path is valid utf8"))
        .env("TYPESAFE_BASE_URL", "http://127.0.0.1:9")
        .env("API_KEY_JEV", "dummy-key-for-e2e-test")
        .env("RETRY_COUNT", "0")
        .env("WORKER_COUNT", "2")
        .output()
        .expect("failed to execute binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(
        !stdout.contains("Run at full volume"),
        "should not prompt when --yes is given"
    );
    assert!(stdout.contains("Run Summary"));
    assert!(db_path.exists());
}
