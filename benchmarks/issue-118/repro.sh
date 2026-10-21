#!/usr/bin/env bash
set -euo pipefail

old_bin=${REPO_022_BIN:-/tmp/repodb-target-022/release/repodb}
new_bin=${REPO_030_BIN:-/tmp/repodb-target-030/release/repodb}
: "${XDG_CONFIG_HOME:?Set XDG_CONFIG_HOME to the RepoDB config used for both binaries}"
for binary in "$old_bin" "$new_bin"; do
    test -x "$binary" || { printf 'missing executable: %s\n' "$binary" >&2; exit 2; }
done

run_root=$(mktemp -d /tmp/issue118-repro.XXXXXX)
fixture="$run_root/fixture"
outputs="$run_root/outputs"
mkdir -p "$fixture" "$outputs"
git -C "$fixture" init -q
git -C "$fixture" config user.name 'Issue 118 Fixture'
git -C "$fixture" config user.email 'issue118@example.invalid'
printf '%s\n' 'amber-needle alpha' 'violet-lantern beta' 'amber-needle gamma' 'cedar-window delta' > "$fixture/notes.txt"
git -C "$fixture" add notes.txt
GIT_AUTHOR_DATE='2024-01-01T00:00:00Z' GIT_COMMITTER_DATE='2024-01-01T00:00:00Z' \
    git -C "$fixture" commit -qm 'fixture initial amber-needle'
printf '%s\n' 'amber-needle epsilon' 'violet-lantern zeta' 'cobalt-river eta' > "$fixture/notes.txt"
git -C "$fixture" add notes.txt
GIT_AUTHOR_DATE='2024-01-02T00:00:00Z' GIT_COMMITTER_DATE='2024-01-02T00:00:00Z' \
    git -C "$fixture" commit -qm 'fixture update cobalt-river'

ISSUE118_FIXTURE="$fixture" ISSUE118_OUTPUTS="$outputs" \
ISSUE118_022_BIN="$old_bin" ISSUE118_030_BIN="$new_bin" \
python3 - <<'PY'
import json
import os
import pathlib
import sqlite3
import subprocess

fixture = pathlib.Path(os.environ["ISSUE118_FIXTURE"])
outputs = pathlib.Path(os.environ["ISSUE118_OUTPUTS"])
binaries = {
    "022": os.environ["ISSUE118_022_BIN"],
    "030": os.environ["ISSUE118_030_BIN"],
}
commands = [
    ("version", ["--version"]),
    ("init", ["init"]),
    ("default-json", ["search", "amber-needle", "--json"]),
    ("no-match-json", ["search", "absent-quartz", "--json"]),
    ("exact-json", ["search", "amber-needle", "--exact", "--json"]),
    ("history-json", ["search", "amber-needle", "--exact", "--history=all", "--json"]),
    ("pagination-json", ["search", "amber-needle", "--limit", "2", "--json"]),
    ("bad-option", ["search", "amber-needle", "--not-an-option"]),
]
manifest = {
    "fixture_head": subprocess.check_output(["git", "-C", str(fixture), "rev-parse", "HEAD"], text=True).strip(),
    "home": os.environ.get("HOME"),
    "xdg_config_home": os.environ.get("XDG_CONFIG_HOME"),
    "commands": [],
}
for version, binary in binaries.items():
    database = outputs / f"{version}.sqlite"
    for name, args in commands:
        command = [binary, *args] if name == "version" else [binary, "--database", str(database), *args]
        try:
            result = subprocess.run(command, cwd=fixture, text=True, errors="replace", capture_output=True, timeout=180)
            returncode, stdout, stderr = result.returncode, result.stdout, result.stderr
        except subprocess.TimeoutExpired as error:
            returncode = 124
            stdout = error.stdout.decode(errors="replace") if isinstance(error.stdout, bytes) else (error.stdout or "")
            stderr = error.stderr.decode(errors="replace") if isinstance(error.stderr, bytes) else (error.stderr or "")
            stderr += "\nTIMEOUT after 180 seconds\n"
        stdout_path = outputs / f"{version}-{name}.stdout"
        stderr_path = outputs / f"{version}-{name}.stderr"
        stdout_path.write_text(stdout)
        stderr_path.write_text(stderr)
        manifest["commands"].append({
            "version": version,
            "command": command,
            "cwd": str(fixture),
            "exit": returncode,
            "stdout": stdout_path.name,
            "stderr": stderr_path.name,
        })
    try:
        with sqlite3.connect(database) as connection:
            config = dict(connection.execute("SELECT key, value FROM embedding_config"))
    except (sqlite3.Error, OSError):
        config = None
    manifest.setdefault("embedding_config", {})[version] = config
(outputs / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
for command in manifest["commands"]:
    assert (outputs / command["stdout"]).is_file()
    assert (outputs / command["stderr"]).is_file()
print(f"Captured {len(manifest['commands'])} commands; fixture and outputs are under {outputs}")
PY
