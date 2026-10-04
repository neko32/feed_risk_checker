"""feed_risk_checker 簡易ビューワー（ふっかちゃん担当）。

Rustバックエンドが書き込んだSQLiteファイル（feeds / dlq_feeds テーブル）を
読み取り専用で表示するだけの、軽量なTKInterデスクトップアプリ。

標準ライブラリのみを使用（sqlite3 / tkinter）。外部依存なしで動く。

使い方:
    python viewer.py [path/to/feed_risk.db]

    db パスを省略した場合は、このファイルから見て ../app/feed_risk.db を既定値とする。
    起動後も「DBを開く...」ボタンでいつでも切り替えられる。
"""

from __future__ import annotations

import sqlite3
import sys
import tkinter as tk
from pathlib import Path
from tkinter import filedialog, messagebox, ttk

DEFAULT_DB_PATH = Path(__file__).resolve().parent.parent / "app" / "feed_risk.db"

OK_COLUMNS = (
    "id",
    "user_name",
    "message",
    "expected_label",
    "input_tokens",
    "output_tokens",
    "latency_ms",
)
DLQ_COLUMNS = (
    "id",
    "user_name",
    "message",
    "reason",
    "expected_label",
    "input_tokens",
    "output_tokens",
    "latency_ms",
)

COLUMN_LABELS = {
    "id": "ID",
    "user_name": "User",
    "message": "Message",
    "reason": "Reason",
    "expected_label": "Expected",
    "input_tokens": "In Tokens",
    "output_tokens": "Out Tokens",
    "latency_ms": "Latency (ms)",
}

COLUMN_WIDTHS = {
    "id": 60,
    "user_name": 110,
    "message": 420,
    "reason": 220,
    "expected_label": 80,
    "input_tokens": 90,
    "output_tokens": 90,
    "latency_ms": 100,
}

# dlq_feeds.reasonがこのプレフィックスで始まる行は「判定自体が行えなかった」エラーであり、
# Accuracy/Precision/Recallの計算対象（混同行列）には含めない。Rust側（core::worker）の
# 扱いと揃える。
JUDGE_ERROR_REASON_PREFIX = "judge_error:"


class FeedRiskViewer(tk.Tk):
    """feed_risk_checker の実行結果を閲覧するためのメインウィンドウ。"""

    def __init__(self, db_path: Path) -> None:
        super().__init__()
        self.title("feed_risk_checker Viewer")
        self.geometry("1100x650")

        self.db_path = db_path

        self._build_toolbar()
        self._build_summary()
        self._build_tables()

        self.refresh()

    # ------------------------------------------------------------------
    # UI構築
    # ------------------------------------------------------------------
    def _build_toolbar(self) -> None:
        bar = ttk.Frame(self, padding=8)
        bar.pack(side=tk.TOP, fill=tk.X)

        self.db_path_var = tk.StringVar(value=str(self.db_path))
        ttk.Label(bar, text="DB:").pack(side=tk.LEFT)
        ttk.Entry(bar, textvariable=self.db_path_var, width=70, state="readonly").pack(
            side=tk.LEFT, padx=(4, 8)
        )
        ttk.Button(bar, text="Open DB...", command=self._choose_db).pack(side=tk.LEFT)
        ttk.Button(bar, text="Refresh", command=self.refresh).pack(side=tk.LEFT, padx=(8, 0))

    def _build_summary(self) -> None:
        frame = ttk.LabelFrame(self, text="Summary", padding=8)
        frame.pack(side=tk.TOP, fill=tk.X, padx=8, pady=(0, 8))

        self.summary_labels: dict[str, ttk.Label] = {}
        fields = [
            ("total", "Total"),
            ("ok", "OK Count"),
            ("dlq", "NG (DLQ) Count"),
            ("ng_rate", "Judged NG Rate"),
        ]
        metric_fields = [
            ("accuracy", "Accuracy"),
            ("precision", "Precision"),
            ("recall", "Recall"),
        ]
        token_fields = [
            ("total_tokens", "Total Tokens"),
            ("avg_tokens", "Avg Tokens/Request"),
        ]
        for i, (key, label) in enumerate(fields):
            ttk.Label(frame, text=f"{label}:").grid(row=0, column=i * 2, sticky="w", padx=(0, 4))
            value_label = ttk.Label(frame, text="-", font=("TkDefaultFont", 10, "bold"))
            value_label.grid(row=0, column=i * 2 + 1, sticky="w", padx=(0, 20))
            self.summary_labels[key] = value_label
        for i, (key, label) in enumerate(metric_fields):
            ttk.Label(frame, text=f"{label}:").grid(
                row=1, column=i * 2, sticky="w", padx=(0, 4), pady=(4, 0)
            )
            value_label = ttk.Label(frame, text="-", font=("TkDefaultFont", 10, "bold"))
            value_label.grid(row=1, column=i * 2 + 1, sticky="w", padx=(0, 20), pady=(4, 0))
            self.summary_labels[key] = value_label
        for i, (key, label) in enumerate(token_fields):
            ttk.Label(frame, text=f"{label}:").grid(
                row=2, column=i * 2, sticky="w", padx=(0, 4), pady=(4, 0)
            )
            value_label = ttk.Label(frame, text="-", font=("TkDefaultFont", 10, "bold"))
            value_label.grid(row=2, column=i * 2 + 1, sticky="w", padx=(0, 20), pady=(4, 0))
            self.summary_labels[key] = value_label

    def _build_tables(self) -> None:
        notebook = ttk.Notebook(self)
        notebook.pack(side=tk.TOP, fill=tk.BOTH, expand=True, padx=8, pady=(0, 8))

        ok_frame = ttk.Frame(notebook)
        dlq_frame = ttk.Frame(notebook)
        notebook.add(ok_frame, text="OK Feeds")
        notebook.add(dlq_frame, text="NG (DLQ) Feeds")

        self.ok_tree = self._make_tree(ok_frame, OK_COLUMNS)
        self.dlq_tree = self._make_tree(dlq_frame, DLQ_COLUMNS)

    @staticmethod
    def _make_tree(parent: ttk.Frame, columns: tuple[str, ...]) -> ttk.Treeview:
        tree = ttk.Treeview(parent, columns=columns, show="headings")
        for col in columns:
            tree.heading(col, text=COLUMN_LABELS.get(col, col))
            tree.column(col, width=COLUMN_WIDTHS.get(col, 120), anchor="w")

        vsb = ttk.Scrollbar(parent, orient="vertical", command=tree.yview)
        hsb = ttk.Scrollbar(parent, orient="horizontal", command=tree.xview)
        tree.configure(yscrollcommand=vsb.set, xscrollcommand=hsb.set)

        tree.grid(row=0, column=0, sticky="nsew")
        vsb.grid(row=0, column=1, sticky="ns")
        hsb.grid(row=1, column=0, sticky="ew")
        parent.rowconfigure(0, weight=1)
        parent.columnconfigure(0, weight=1)
        return tree

    # ------------------------------------------------------------------
    # データ読み込み
    # ------------------------------------------------------------------
    def _choose_db(self) -> None:
        chosen = filedialog.askopenfilename(
            title="Select SQLite Database",
            filetypes=[("SQLite DB", "*.db"), ("All Files", "*.*")],
            initialdir=str(self.db_path.parent) if self.db_path.parent.exists() else None,
        )
        if chosen:
            self.db_path = Path(chosen)
            self.db_path_var.set(str(self.db_path))
            self.refresh()

    def refresh(self) -> None:
        if not self.db_path.exists():
            messagebox.showwarning(
                "DB not found",
                f"Could not find the specified DB file:\n{self.db_path}\n\n"
                "Try running feed_risk_checker first.",
            )
            self._clear_tables()
            self._set_summary(0, 0, 0.0, 0.0, 0.0, 0.0, 0, 0.0)
            return

        try:
            ok_rows, dlq_rows = self._load_rows()
        except sqlite3.Error as exc:
            messagebox.showerror("Read Error", f"Failed to read the DB:\n{exc}")
            return

        self._populate_tree(self.ok_tree, OK_COLUMNS, ok_rows)
        self._populate_tree(self.dlq_tree, DLQ_COLUMNS, dlq_rows)

        total = len(ok_rows) + len(dlq_rows)
        ng_rate = (len(dlq_rows) / total * 100.0) if total else 0.0
        total_tokens, avg_tokens = self._compute_token_stats(ok_rows, dlq_rows)
        accuracy, precision, recall = self._compute_confusion_metrics(ok_rows, dlq_rows)
        self._set_summary(
            len(ok_rows),
            len(dlq_rows),
            ng_rate,
            accuracy,
            precision,
            recall,
            total_tokens,
            avg_tokens,
        )

    @staticmethod
    def _compute_confusion_metrics(
        ok_rows: list[tuple], dlq_rows: list[tuple]
    ) -> tuple[float, float, float]:
        """Accuracy / Precision / Recall を計算する（%ではなく0.0〜1.0の比率で返す）。

        正解ラベル（`expected_label`）と実際の判定結果を比較する。NGが「陽性」クラス
        （検出したい違反）という前提の標準的な定義：
          - TP: 正解NG かつ 判定NG（正しく検出）
          - FP: 正解OK かつ 判定NG（誤検知）
          - TN: 正解OK かつ 判定OK（正しく見逃さなかった）
          - FN: 正解NG かつ 判定OK（見逃し）
        `judge_error:`で始まる行は判定自体が行えていないため混同行列には含めない
        （Rust側の `core::worker::RunStats` と同じ扱い）。
        """
        ok_expected_idx = OK_COLUMNS.index("expected_label")
        dlq_expected_idx = DLQ_COLUMNS.index("expected_label")
        dlq_reason_idx = DLQ_COLUMNS.index("reason")

        true_positive = false_positive = true_negative = false_negative = 0

        for row in ok_rows:
            if row[ok_expected_idx] == "NG":
                false_negative += 1
            elif row[ok_expected_idx] == "OK":
                true_negative += 1
            # expected_labelがNULL（マイグレーションされた古い行等）は集計対象外。

        for row in dlq_rows:
            if row[dlq_reason_idx].startswith(JUDGE_ERROR_REASON_PREFIX):
                continue
            if row[dlq_expected_idx] == "NG":
                true_positive += 1
            elif row[dlq_expected_idx] == "OK":
                false_positive += 1

        classified = true_positive + false_positive + true_negative + false_negative
        accuracy = ((true_positive + true_negative) / classified) if classified else 0.0
        precision = (
            (true_positive / (true_positive + false_positive))
            if (true_positive + false_positive)
            else 0.0
        )
        recall = (
            (true_positive / (true_positive + false_negative))
            if (true_positive + false_negative)
            else 0.0
        )
        return accuracy, precision, recall

    @staticmethod
    def _compute_token_stats(ok_rows: list[tuple], dlq_rows: list[tuple]) -> tuple[int, float]:
        """合計トークン数と、使用量が取れたリクエスト1件あたりの平均トークン数を計算する。

        `input_tokens`/`output_tokens` はRust側でジャッジがトークン使用量を報告した
        場合のみ埋まり、報告されなかった行ではNULL（Python側ではNone）になる。
        平均の分母はNULLでない行の件数のみを使う（全件で割ると過小評価になるため）。
        """
        in_idx = OK_COLUMNS.index("input_tokens")
        out_idx = OK_COLUMNS.index("output_tokens")
        dlq_in_idx = DLQ_COLUMNS.index("input_tokens")
        dlq_out_idx = DLQ_COLUMNS.index("output_tokens")

        total_tokens = 0
        usage_sample_count = 0

        for row in ok_rows:
            if row[in_idx] is not None and row[out_idx] is not None:
                total_tokens += row[in_idx] + row[out_idx]
                usage_sample_count += 1
        for row in dlq_rows:
            if row[dlq_in_idx] is not None and row[dlq_out_idx] is not None:
                total_tokens += row[dlq_in_idx] + row[dlq_out_idx]
                usage_sample_count += 1

        avg_tokens = (total_tokens / usage_sample_count) if usage_sample_count else 0.0
        return total_tokens, avg_tokens

    def _load_rows(self) -> tuple[list[tuple], list[tuple]]:
        with sqlite3.connect(str(self.db_path)) as conn:
            conn.row_factory = None
            self._migrate_token_columns_if_missing(conn)
            ok_rows = conn.execute(
                f"SELECT {', '.join(OK_COLUMNS)} FROM feeds ORDER BY id"
            ).fetchall()
            dlq_rows = conn.execute(
                f"SELECT {', '.join(DLQ_COLUMNS)} FROM dlq_feeds ORDER BY id"
            ).fetchall()
        return ok_rows, dlq_rows

    @staticmethod
    def _migrate_token_columns_if_missing(conn: sqlite3.Connection) -> None:
        """`input_tokens`/`output_tokens`/`expected_label`/`latency_ms` 列が無い
        古いスキーマのDBでも読めるようにする。

        Rust側（`SqliteRepository::init_schema`）にも同じ趣旨のマイグレーションがあるが、
        あれはRustバイナリがそのDBファイルを開いたときにしか走らない。ビューワーを
        「Rustバイナリを再実行せずに、古いDBファイルだけ単独で開く」という使い方をすると
        `no such column: input_tokens` でエラーになる不具合が実際に発生したため、
        ビューワー側にも同じマイグレーションを持たせて自己完結させる。
        """
        for table in ("feeds", "dlq_feeds"):
            existing_columns = {
                row[1] for row in conn.execute(f"PRAGMA table_info({table})")
            }
            for column, sql_type in (
                ("input_tokens", "INTEGER"),
                ("output_tokens", "INTEGER"),
                ("expected_label", "TEXT"),
                ("latency_ms", "INTEGER"),
            ):
                if column not in existing_columns:
                    conn.execute(f"ALTER TABLE {table} ADD COLUMN {column} {sql_type}")
        conn.commit()

    def _clear_tables(self) -> None:
        for tree in (self.ok_tree, self.dlq_tree):
            for item in tree.get_children():
                tree.delete(item)

    @staticmethod
    def _populate_tree(tree: ttk.Treeview, columns: tuple[str, ...], rows: list[tuple]) -> None:
        for item in tree.get_children():
            tree.delete(item)
        for row in rows:
            tree.insert("", tk.END, values=row)

    def _set_summary(
        self,
        ok_count: int,
        dlq_count: int,
        ng_rate: float,
        accuracy: float,
        precision: float,
        recall: float,
        total_tokens: int,
        avg_tokens: float,
    ) -> None:
        total = ok_count + dlq_count
        self.summary_labels["total"].configure(text=str(total))
        self.summary_labels["ok"].configure(text=str(ok_count))
        self.summary_labels["dlq"].configure(text=str(dlq_count))
        self.summary_labels["ng_rate"].configure(text=f"{ng_rate:.2f}%")
        self.summary_labels["accuracy"].configure(text=f"{accuracy * 100.0:.2f}%")
        self.summary_labels["precision"].configure(text=f"{precision * 100.0:.2f}%")
        self.summary_labels["recall"].configure(text=f"{recall * 100.0:.2f}%")
        self.summary_labels["total_tokens"].configure(text=f"{total_tokens:,}")
        self.summary_labels["avg_tokens"].configure(text=f"{avg_tokens:.1f}")


def main() -> None:
    db_path = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_DB_PATH
    app = FeedRiskViewer(db_path)
    app.mainloop()


if __name__ == "__main__":
    main()
