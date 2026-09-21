#!/usr/bin/env python3
"""launch_worktree.py — Create or reuse a git worktree, then launch Claude Code.

Creates a worktree at .claude/worktrees/<name> with its own branch, branched
off the project's default dev branch (dev → develop → main → master, first
that exists), copies .env files, propagates enabledPlugins, then hands control
to the claude CLI inside it. If the worktree already exists, it is reused
without error.

Hooks and skills come from the mindcoachlabs Claude Code plugin (not copied
per worktree). Claude Code resolves plugin paths via ${CLAUDE_PLUGIN_ROOT}.

Cross-platform: runs on macOS, Linux, and Windows. Python 3.8+ required.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

USAGE = """\
Usage: launch_worktree.py <name> [--base <branch>] [--cli claude|grok|none] [-h|--help]

Arguments:
  <name>             Worktree slug (required). Creates or reuses
                     .claude/worktrees/<name> on branch feature/<name>.

Flags:
  --base <branch>    Override base branch (default: auto-detect from
                     dev → develop → main → master).
  --cli <name>       CLI to exec after create: claude (default), grok, or
                     none (create worktree only; print path).
  -h, --help         Show this help and exit.

Examples:
  python launch_worktree.py wt1                    # auto-detect base, launch Claude Code
  python launch_worktree.py auth-refactor --cli grok
  python launch_worktree.py hotfix --base main --cli none
"""


def die(msg: str, code: int = 1) -> None:
    print(msg, file=sys.stderr)
    sys.exit(code)


def run_git(*args: str, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args],
        cwd=str(cwd) if cwd else None,
        capture_output=True,
        text=True,
        check=False,
    )


def parse_args(argv: list[str]) -> tuple[str, str, str]:
    name = ""
    base_override = ""
    cli = "claude"
    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg in ("-h", "--help"):
            print(USAGE)
            sys.exit(0)
        elif arg == "--base":
            if i + 1 >= len(argv):
                die("Error: --base requires a branch name")
            base_override = argv[i + 1]
            i += 2
            continue
        elif arg == "--cli":
            if i + 1 >= len(argv):
                die("Error: --cli requires claude|grok|none")
            cli = argv[i + 1].strip().lower()
            if cli not in ("claude", "grok", "none"):
                die("Error: --cli must be claude, grok, or none")
            i += 2
            continue
        elif arg.startswith("-"):
            die(f"Unknown flag: {arg}\n{USAGE}")
        else:
            if name:
                die(f"Error: multiple positional arguments (got '{name}' and '{arg}')")
            name = arg
        i += 1
    if not name:
        die(USAGE)
    return name, base_override, cli


def detect_base_branch(base_override: str, current_branch: str) -> tuple[str, bool]:
    """Return (base_branch, is_override_or_fallback)."""
    if base_override:
        return base_override, False
    for candidate in ("dev", "develop", "main", "master"):
        result = run_git("show-ref", "--verify", "--quiet", f"refs/heads/{candidate}")
        if result.returncode == 0:
            return candidate, False
    for candidate in ("dev", "develop", "main", "master"):
        result = run_git("show-ref", "--verify", "--quiet", f"refs/remotes/origin/{candidate}")
        if result.returncode == 0:
            return candidate, False
    print(
        f"⚠️  No dev/develop/main/master branch found — "
        f"falling back to current branch '{current_branch}'",
        file=sys.stderr,
    )
    return current_branch, True


def worktree_registered(worktree_dir: Path) -> bool:
    result = run_git("worktree", "list", "--porcelain")
    if result.returncode != 0:
        return False
    target = f"worktree {worktree_dir}"
    return any(line.strip() == target for line in result.stdout.splitlines())


def find_existing_branch_worktree(branch_ref: str) -> str | None:
    result = run_git("worktree", "list", "--porcelain")
    if result.returncode != 0:
        return None
    current_wt: str | None = None
    for line in result.stdout.splitlines():
        if line.startswith("worktree "):
            current_wt = line[len("worktree "):].strip()
        elif line.startswith("branch ") and current_wt:
            if line[len("branch "):].strip() == branch_ref:
                return current_wt
    return None


def copy_env_files(repo_root: Path, worktree_dir: Path) -> None:
    env_files = (".env", ".env.local", ".env.development")
    search_dirs = (repo_root, repo_root / "web", repo_root / "src")
    for env_file in env_files:
        for search_dir in search_dirs:
            src = search_dir / env_file
            if src.is_file():
                rel = (
                    search_dir.relative_to(repo_root)
                    if search_dir != repo_root
                    else Path(".")
                )
                dst_dir = worktree_dir / rel
                dst_dir.mkdir(parents=True, exist_ok=True)
                try:
                    shutil.copy2(src, dst_dir / env_file)
                    print(f"   📋 Copied {env_file}")
                except OSError:
                    pass


def propagate_enabled_plugins(repo_root: Path, worktree_dir: Path) -> None:
    """Copy enabledPlugins from main settings into the worktree settings."""
    main_settings = repo_root / ".claude" / "settings.json"
    wt_claude_dir = worktree_dir / ".claude"
    wt_settings = wt_claude_dir / "settings.json"

    if not main_settings.is_file():
        return

    try:
        main_data = json.loads(main_settings.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return

    enabled = main_data.get("enabledPlugins")
    if not enabled:
        return

    wt_claude_dir.mkdir(parents=True, exist_ok=True)

    if wt_settings.is_file():
        try:
            wt_data = json.loads(wt_settings.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            wt_data = {}
    else:
        wt_data = {"workspace": {"relatedRepos": []}}

    wt_data["enabledPlugins"] = enabled
    wt_settings.write_text(
        json.dumps(wt_data, indent=2) + "\n", encoding="utf-8"
    )
    print("   🔌 Propagated enabledPlugins to worktree settings")


def main(argv: list[str]) -> int:
    name, base_override, cli = parse_args(argv)

    top = run_git("rev-parse", "--show-toplevel")
    if top.returncode != 0:
        die("Not a git repository (or unable to run git).")
    repo_root = Path(top.stdout.strip())

    cur = run_git("branch", "--show-current")
    current_branch = cur.stdout.strip() if cur.returncode == 0 else ""

    worktree_dir = repo_root / ".claude" / "worktrees" / name
    branch_name = f"feature/{name}"

    base_branch, _ = detect_base_branch(base_override, current_branch)

    # Best-effort fetch so the worktree starts from latest upstream state.
    has_remote_base = False
    ls = run_git("ls-remote", "--exit-code", "--heads", "origin", base_branch)
    if ls.returncode == 0:
        fetch = run_git("fetch", "origin", base_branch, "--quiet")
        if fetch.returncode == 0:
            has_remote_base = True

    reusing = False
    if worktree_dir.is_dir():
        if worktree_registered(worktree_dir):
            print(f"♻️  Worktree '{name}' already exists — reusing at {worktree_dir}")
            reusing = True
        else:
            print(
                f"❌ Directory {worktree_dir} exists but is not a registered worktree (stale).",
                file=sys.stderr,
            )
            print(f"   To clean up: remove {worktree_dir}", file=sys.stderr)
            return 1

    if not reusing:
        start_point = f"origin/{base_branch}" if has_remote_base else base_branch

        print(f"🌳 Creating worktree: {name}")
        print(f"   Branch: {branch_name} (from {start_point})")
        print(f"   Path: {worktree_dir}")
        print()

        ref_check = run_git("show-ref", "--verify", "--quiet", f"refs/heads/{branch_name}")
        if ref_check.returncode == 0:
            existing_wt = find_existing_branch_worktree(f"refs/heads/{branch_name}")
            if existing_wt:
                print(f"   ♻️  Branch {branch_name} is already checked out at: {existing_wt}")
                print("   Redirecting to that worktree...")
                worktree_dir = Path(existing_wt)
            else:
                print(f"   ♻️  Branch {branch_name} already exists — reusing it")
                add = run_git("worktree", "add", str(worktree_dir), branch_name)
                if add.returncode != 0:
                    die(add.stderr or "git worktree add failed")
        else:
            add = run_git(
                "worktree", "add", "-b", branch_name, str(worktree_dir), start_point
            )
            if add.returncode != 0:
                die(add.stderr or "git worktree add failed")

        copy_env_files(repo_root, worktree_dir)
        propagate_enabled_plugins(repo_root, worktree_dir)

        print()
        print("✅ Worktree ready!")

    print()
    print("💡 After Claude starts, run /reload-plugins to activate the MindCoachLabs plugin.")
    print()
    print("When done:")
    print(f"   /mindcoachlabs:ship       # Merge back to {base_branch}")
    print(f"   git worktree remove {worktree_dir} && git branch -d {branch_name}")
    print()

    if cli == "none":
        print(f"📂 Worktree ready (no CLI launch): {worktree_dir}")
        print(f"   cd {worktree_dir} && grok   # or claude")
        return 0

    cli_path = shutil.which(cli)
    if not cli_path:
        print(
            f"❌ '{cli}' is not on PATH. Install it or adjust PATH, then re-run.",
            file=sys.stderr,
        )
        print(f"   Worktree is at: {worktree_dir}", file=sys.stderr)
        return 1

    print(f"🚀 Launching {cli} in {worktree_dir}...")
    os.chdir(worktree_dir)
    if cli == "grok":
        # Pass --cwd so session keys correctly even if shell cwd is ignored
        launch_argv = [cli_path, "--cwd", str(worktree_dir)]
    else:
        launch_argv = [cli_path]
    if os.name == "nt":
        return subprocess.run(launch_argv, cwd=str(worktree_dir)).returncode
    os.execvp(cli_path, launch_argv)
    return 0  # unreachable on Unix


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
