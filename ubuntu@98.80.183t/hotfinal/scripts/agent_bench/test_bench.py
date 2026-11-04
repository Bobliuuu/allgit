"""Offline tests for the agent benchmark: needs a vcs binary (env VCS_BIN) and git. No network, no LLM."""
import os
import shutil
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import fixture  # noqa: E402
import harness  # noqa: E402
import tasks as TK  # noqa: E402

VCS = os.environ.get("VCS_BIN")


@unittest.skipUnless(VCS and shutil.which("git"), "set VCS_BIN to a vcs binary")
class OfflineBench(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.dir = tempfile.mkdtemp(prefix="agentbench-test-")
        fixture.build(cls.dir, VCS)
        cls.ex = harness.Executor({"git": cls.dir + "/git", "vcs": cls.dir + "/vcs"}, VCS)
        cls.tasks = TK.make_tasks()

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.dir, ignore_errors=True)

    def test_task_count_and_ground_truth_is_independent(self):
        self.assertTrue(15 <= len(self.tasks) <= 25)
        self.assertEqual(len({t.id for t in self.tasks}), len(self.tasks))

    def test_every_oracle_is_correct_for_both_tools(self):
        bad = []
        for t in self.tasks:
            for tool in ("git", "vcs"):
                r = harness.run_oracle(self.ex, t, tool)
                if not r["correct"]:
                    bad.append((t.id, tool, r["answer"], t.answer, r["error"]))
        self.assertEqual(bad, [])

    def test_graders_reject_wrong_answers(self):
        t = next(x for x in self.tasks if x.id == "build_cli")
        self.assertFalse(TK.grade(t, ["cli"]))
        self.assertFalse(TK.grade(t, None))
        self.assertTrue(TK.grade(t, ["CLI", "api", "core", "net", "storage", "util"]))

    def test_both_repos_have_the_same_history(self):
        g = self.ex.run("git", ["git", "log", "--format=%s"])[0].splitlines()
        self.assertEqual(g[::-1], [c[1] for c in fixture.COMMITS])

    def test_executor_blocks_writes(self):
        self.assertEqual(self.ex.run("git", ["git", "commit", "-m", "x"])[1], 2)
        self.assertEqual(self.ex.run("git", ["git", "log", "--output=/tmp/x"])[1], 2)
        self.assertEqual(self.ex.run("vcs", ["vcs", "commit", "-m", "x"])[1], 2)
        self.assertEqual(self.ex.run("vcs", ["git", "log"])[1], 2)

    def test_episode_metrics_recorded(self):
        r = harness.run_oracle(self.ex, self.tasks[0], "vcs")
        self.assertEqual(r["tool_calls"], len(r["calls"]))
        self.assertEqual(r["out_bytes"], sum(c["out_bytes"] for c in r["calls"]))

    def test_parse_answer(self):
        self.assertEqual(harness.parse_answer("x\nANSWER: [\"a\"]"), ["a"])
        self.assertEqual(harness.parse_answer("ANSWER: 4"), 4)
        self.assertIsNone(harness.parse_answer("no answer"))


if __name__ == "__main__":
    unittest.main()
