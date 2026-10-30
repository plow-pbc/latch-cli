"""`docs_site.py publish`'s own tests, offline: plow is a local repo, and gh a
stub that records the PR it was asked to open.

    python3 scripts/test_docs_site.py LATCH
"""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
LATCH = None


def git(where, *args):
    return subprocess.run(["git", "-C", str(where), *args], check=True, capture_output=True, text=True).stdout.strip()


def commit_all(where, message):
    git(where, "add", "--all")
    git(where, "-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "--quiet", "-m", message)


class Publish(unittest.TestCase):
    def setUp(self):
        self.temp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.temp)
        # latch-cli as its own committed repo, so the script's checks see a clean tree.
        self.latch_cli = self.temp / "latch-cli"
        shutil.copytree(ROOT / "docs/site", self.latch_cli / "docs/site")
        (self.latch_cli / "scripts").mkdir()
        shutil.copy(ROOT / "scripts/docs_site.py", self.latch_cli / "scripts")
        git(self.temp, "init", "--quiet", "-b", "main", str(self.latch_cli))
        commit_all(self.latch_cli, "latch")
        # plow's origin, whose main has an old page and something else, and a clone of it.
        seed = self.temp / "seed"
        (seed / "plow.co/latch/docs").mkdir(parents=True)
        (seed / "plow.co/latch/docs/index.html").write_text("old\n")
        (seed / "plow.co/latch/docs/gone.html").write_text("a page latch dropped\n")
        (seed / "README.md").write_text("plow\n")
        git(self.temp, "init", "--quiet", "-b", "main", str(seed))
        commit_all(seed, "plow")
        self.origin = self.temp / "origin.git"
        git(self.temp, "clone", "--quiet", "--bare", str(seed), str(self.origin))
        self.plow = self.temp / "plow"
        git(self.temp, "clone", "--quiet", str(self.origin), str(self.plow))
        # gh: records its arguments and answers with a PR URL.
        stub = self.temp / "bin"
        stub.mkdir()
        (stub / "gh").write_text(f'#!/bin/sh\nprintf "%s\\n" "$@" > "{self.temp}/gh-args"\n'
                                 'echo https://github.com/plow-pbc/plow/pull/1\n')
        (stub / "gh").chmod(0o755)
        self.env = {**os.environ, "PATH": f"{stub}:{os.environ['PATH']}",
                    "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.com",
                    "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.com"}

    def publish(self, *args):
        return subprocess.run([sys.executable, str(self.latch_cli / "scripts/docs_site.py"), "publish",
                               "--latch", LATCH, str(self.plow), *args],
                              capture_output=True, text=True, env=self.env)

    def branches(self):
        return git(self.origin, "branch", "--format=%(refname:short)").split()

    def assert_checkout_untouched(self):
        self.assertEqual(git(self.plow, "status", "--porcelain"), "")
        self.assertEqual(git(self.plow, "rev-parse", "HEAD"), git(self.origin, "rev-parse", "main"))
        self.assertEqual(len(git(self.plow, "worktree", "list").splitlines()), 1)

    def test_dry_run_prints_the_change_and_leaves_everything_as_it_was(self):
        answer = self.publish("--dry-run")
        self.assertEqual(answer.returncode, 0, answer.stderr)
        self.assertIn("plow.co/latch/docs/gone.html", answer.stdout)
        self.assertIn("plow.co/latch/docs/commands.html", answer.stdout)
        self.assertIn("dry run", answer.stdout)
        self.assertEqual(self.branches(), ["main"])
        self.assertFalse((self.temp / "gh-args").exists())
        self.assert_checkout_untouched()

    def test_publish_pushes_only_the_docs_and_opens_a_pr(self):
        answer = self.publish()
        self.assertEqual(answer.returncode, 0, answer.stderr)
        self.assertTrue(answer.stdout.endswith("https://github.com/plow-pbc/plow/pull/1\n"))
        sha = git(self.latch_cli, "rev-parse", "--short", "HEAD")
        branch = f"latch-docs-{sha}"
        self.assertEqual(sorted(self.branches()), sorted(["main", branch]))
        changed = git(self.origin, "diff", "--name-only", "main", branch).split()
        self.assertTrue(changed and all(p.startswith("plow.co/latch/docs/") for p in changed), changed)
        self.assertIn("plow.co/latch/docs/gone.html", changed)
        gh = (self.temp / "gh-args").read_text().splitlines()
        self.assertEqual(gh[:2], ["pr", "create"])
        self.assertEqual(gh[gh.index("--head") + 1], branch)
        self.assertEqual(gh[gh.index("--title") + 1], f"plow.co/latch/docs: update from latch {sha}")
        self.assertNotIn("merge", gh)
        self.assert_checkout_untouched()

    def test_nothing_changed_opens_no_pr(self):
        # The first PR merged: plow's main is its branch.
        self.assertEqual(self.publish().returncode, 0)
        branch = "latch-docs-" + git(self.latch_cli, "rev-parse", "--short", "HEAD")
        git(self.origin, "update-ref", "refs/heads/main", git(self.origin, "rev-parse", branch))
        git(self.plow, "pull", "--quiet")
        (self.temp / "gh-args").unlink()
        answer = self.publish()
        self.assertEqual(answer.returncode, 0, answer.stderr)
        self.assertIn("no PR", answer.stdout)
        self.assertFalse((self.temp / "gh-args").exists())

    def test_a_dirty_plow_checkout_is_refused(self):
        (self.plow / "README.md").write_text("edited\n")
        answer = self.publish("--dry-run")
        self.assertNotEqual(answer.returncode, 0)
        self.assertIn("uncommitted changes", answer.stderr)

    def test_uncommitted_pages_are_not_published(self):
        with open(self.latch_cli / "docs/site/index.md", "a") as page:
            page.write("\nNot committed.\n")
        answer = self.publish()
        self.assertNotEqual(answer.returncode, 0)
        self.assertIn("uncommitted changes", answer.stderr)
        self.assertEqual(self.branches(), ["main"])


if __name__ == "__main__":
    LATCH = str(Path(sys.argv.pop(1)).resolve())
    unittest.main()
