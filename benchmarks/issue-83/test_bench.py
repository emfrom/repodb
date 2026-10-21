import importlib.util
import unittest
from pathlib import Path

MODULE_PATH = Path(__file__).with_name("bench.py")
spec = importlib.util.spec_from_file_location("issue83_bench", MODULE_PATH)
bench = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(bench)


class ScorerTests(unittest.TestCase):
    def test_scores_answer_and_pinned_source_line(self):
        task = {
            "id": "example",
            "category": "source",
            "answer_terms": ["stale", "refresh"],
            "expected_references": [{"path": "src/worktree.rs", "start_line": 142, "end_line": 149}],
        }
        result = {"path": "src/worktree.rs", "line_start": 145, "line_end": 147,
                  "text": "indexed HEAD is stale; refresh the repository"}
        score = bench.score_task(task, [result], 5)
        self.assertEqual(score["recall_at_k"], 1.0)
        self.assertTrue(score["source_reference_correct"])
        self.assertTrue(score["answer_correct"])

    def test_does_not_accept_same_path_with_nonoverlapping_lines(self):
        task = {
            "id": "example",
            "category": "source",
            "answer_terms": ["stale"],
            "expected_references": [{"path": "src/worktree.rs", "start_line": 142, "end_line": 149}],
        }
        result = {"path": "src/worktree.rs", "line": 80, "text": "stale"}
        score = bench.score_task(task, [result], 5)
        self.assertEqual(score["recall_at_k"], 0.0)
        self.assertFalse(score["source_reference_correct"])

    def test_runner_configures_the_real_embedder_without_changing_home(self):
        import os
        import tempfile
        from unittest.mock import patch

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            embedder = root / "generate_embeddings"
            embedder.write_text("#!/bin/sh\nexit 0\n")
            embedder.chmod(0o755)
            with patch.dict(os.environ, {"HOME": str(root)}):
                env = bench.configure_embedding_environment(root / "run", embedder, None)
            config = (root / "run" / ".config" / "repodb" / "config.toml").read_text()
            self.assertEqual(env["HOME"], str(root))
            self.assertEqual(env["XDG_CONFIG_HOME"], str(root / "run" / ".config"))
            self.assertIn(f'embedding_command = "{embedder}"', config)
            self.assertNotIn("wrapper", config)
            self.assertEqual(env["HF_HUB_OFFLINE"], "1")
            self.assertEqual(env["TRANSFORMERS_OFFLINE"], "1")

    def test_common_budget_keeps_only_complete_results(self):
        first = {"text": "a" * 2700}
        second = {"text": "b" * 2700}
        selected = bench._apply_common_budget([first, second], 5)
        self.assertEqual(selected, [first])
        self.assertLessEqual(len(__import__("json").dumps(selected, separators=(",", ":")).encode()), 5000)

    def test_all_task_references_exist_at_the_immutable_corpus_pins(self):
        import json
        import os
        import subprocess

        data = json.loads(Path(__file__).with_name("tasks.json").read_text())
        repo_root = Path(__file__).resolve().parents[2]
        protonsync = Path(os.environ.get("ISSUE83_PROTONSYNC_REPO", repo_root.parent / "protonsync-ts"))
        roots = {"repodb": repo_root, "protonsync_ts": protonsync}
        self.assertGreaterEqual(len(data["tasks"]), 12)
        for corpus, repo in roots.items():
            with self.subTest(corpus=corpus):
                if corpus == "protonsync_ts" and not repo.is_dir() and "ISSUE83_PROTONSYNC_REPO" not in os.environ:
                    continue  # Optional external fixture; the benchmark runner requires it.
                pin = bench.PIN_BY_CORPUS[corpus]
                subprocess.run(["git", "cat-file", "-e", f"{pin}^{{commit}}"], cwd=repo, check=True)
                bench.verify_task_source_spans(repo, corpus)

    def test_result_limit_applies_before_reference_matching(self):
        task = {
            "id": "example",
            "category": "source",
            "answer_terms": ["right"],
            "expected_references": [{"path": "src/right.rs", "start_line": 1, "end_line": 2}],
        }
        wrong = {"path": "src/wrong.rs", "line": 1, "text": "right"}
        right = {"path": "src/right.rs", "line": 1, "text": "right"}
        self.assertFalse(bench.score_task(task, [wrong, right], 1)["source_reference_correct"])
        self.assertTrue(bench.score_task(task, [wrong, right], 2)["source_reference_correct"])


if __name__ == "__main__":
    unittest.main()
