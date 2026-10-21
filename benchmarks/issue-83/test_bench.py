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

    def test_embedder_wrapper_preserves_whitespace_only_units(self):
        import json
        import subprocess
        import tempfile

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fake = root / "fake-embedder"
            fake.write_text(
                "#!/usr/bin/env python3\n"
                "import json, sys\n"
                "print(json.dumps(sys.argv[1:]))\n",
                encoding="utf-8",
            )
            fake.chmod(0o755)
            log = root / "calls.jsonl"
            wrapper = root / "wrapper.py"
            bench._make_embedder_wrapper(wrapper, fake, log, root / "home")
            completed = subprocess.run(
                [str(wrapper), "--json", "-"], input=b" " * 30,
                stdout=subprocess.PIPE, check=True,
            )
            self.assertEqual(json.loads(completed.stdout), ["--json", "--", " " * 30])
            record = json.loads(log.read_text())
            self.assertTrue(record["preserved_whitespace_stdin"])
            self.assertEqual(record["input_bytes"], 30)
            self.assertEqual(record["returncode"], 0)

    def test_embedder_wrapper_keeps_non_whitespace_stdin_mode(self):
        import json
        import subprocess
        import tempfile

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fake = root / "fake-embedder"
            fake.write_text(
                "#!/usr/bin/env python3\n"
                "import sys\n"
                "sys.stdout.write(sys.stdin.read())\n",
                encoding="utf-8",
            )
            fake.chmod(0o755)
            log = root / "calls.jsonl"
            wrapper = root / "wrapper.py"
            bench._make_embedder_wrapper(wrapper, fake, log, root / "home")
            completed = subprocess.run(
                [str(wrapper), "--json", "-"], input=b"ordinary text",
                stdout=subprocess.PIPE, check=True,
            )
            self.assertEqual(completed.stdout, b"ordinary text")
            self.assertFalse(json.loads(log.read_text())["preserved_whitespace_stdin"])

    def test_common_budget_keeps_only_complete_results(self):
        first = {"text": "a" * 2700}
        second = {"text": "b" * 2700}
        selected = bench._apply_common_budget([first, second], 5)
        self.assertEqual(selected, [first])
        self.assertLessEqual(len(__import__("json").dumps(selected, separators=(",", ":")).encode()), 5000)

    def test_all_task_references_exist_at_the_immutable_corpus_pins(self):
        import json
        from pathlib import Path

        data = json.loads(Path(__file__).with_name("tasks.json").read_text())
        roots = {
            "repodb": Path(__file__).resolve().parents[2],
            "protonsync_ts": Path("/home/emfrom/src/projects/protonsync-ts"),
        }
        self.assertGreaterEqual(len(data["tasks"]), 12)
        for corpus, repo in roots.items():
            self.assertEqual(bench._git_head(repo), bench.PIN_BY_CORPUS[corpus])
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
