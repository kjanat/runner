#!/usr/bin/env bash
# Re-verifies every claim made about the branch's state vs origin/master.
# Each check is (claim, command, expected). If the command's actual output
# doesn't match expected, the check fails and the claim is false.
#
# Run from repo root. Read-only. Read it before running so you trust what
# it does.

set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.." 2>/dev/null || true
[ -d .git ] || cd /home/kjanat/projects/runner || exit 1

pass=0
fail=0
warn=0

check() {
	local label="$1"
	local actual="$2"
	local expected="$3"
	if [ "$actual" = "$expected" ]; then
		printf '  \e[32m✓\e[0m %s\n' "$label"
		pass=$((pass + 1))
	else
		printf '  \e[31m✗\e[0m %s\n' "$label"
		printf '      expected: %s\n' "$expected"
		printf '      actual:   %s\n' "$actual"
		fail=$((fail + 1))
	fi
}

note() {
	printf '  \e[33m!\e[0m %s\n' "$1"
	warn=$((warn + 1))
}

section() {
	printf '\n\e[1m── %s ──\e[0m\n' "$1"
}

section "branch position"
check "HEAD on claude/setup-crates-publishing-eYir9" \
	"$(git rev-parse --abbrev-ref HEAD)" \
	"claude/setup-crates-publishing-eYir9"
check "HEAD = 7a6abcd (date drop)" \
	"$(git rev-parse HEAD | cut -c1-7)" \
	"7a6abcd"

section "merge with master is byte-clean for files NOT touched by branch"
# npm-release.yml is included here because F1 (branch's fix) converges with
# master's 6c42cc9 — same fix string, so the merged file matches master.
for f in action.yml install.sh build.rs npm/scripts/build-packages.ts \
	npm/facade/README.md src/cli.rs src/cmd/list.rs src/cmd/run.rs \
	src/detect.rs src/lib.rs src/tool/node.rs src/tool/turbo.rs \
	src/types.rs site/package.json site/wrangler.jsonc \
	.github/workflows/npm-release.yml; do
	if [ -z "$(git diff origin/master HEAD -- "$f")" ]; then
		printf '  \e[32m✓\e[0m %s matches origin/master byte-for-byte\n' "$f"
		pass=$((pass + 1))
	else
		printf '  \e[31m✗\e[0m %s differs from origin/master (unexpected)\n' "$f"
		fail=$((fail + 1))
	fi
done

section "intentional branch divergence (these *should* differ from master)"
for f in .dprint.json .github/workflows/crates-release.yml \
	CHANGELOG.md Cargo.lock \
	Cargo.toml README.md bin/runner justfile site/README.md \
	site/biome.json site/build.ts site/dev.ts site/public/_headers \
	site/src/404.html site/src/index.html site/src/styles/index.css \
	src/tool/cargo_aliases.rs; do
	if [ -n "$(git diff origin/master HEAD -- "$f")" ]; then
		printf '  \e[32m✓\e[0m %s differs (expected)\n' "$f"
		pass=$((pass + 1))
	else
		printf '  \e[33m!\e[0m %s identical to master (no branch divergence — verify intentional)\n' "$f"
		warn=$((warn + 1))
	fi
done

section "CHANGELOG section headings"
expected_headings='## [Unreleased]
## [0.7.0]
## [0.6.1] - 2026-05-08
## [0.6.0] - 2026-05-05
## [0.5.0] - 2026-04-21
## [0.4.1] - 2026-04-21
## [0.4.0] - 2026-04-17
## [0.3.1] - 2026-04-15
## [0.3.0] - 2026-04-15
## [0.2.1] - 2026-04-15
## [0.2.0] - 2026-03-29
## [0.1.0] - 2026-03-27'
actual_headings=$(grep '^## \[' CHANGELOG.md)
check "CHANGELOG section headings match expected order + dates" \
	"$actual_headings" "$expected_headings"

section "CHANGELOG [0.6.1] section is byte-identical to origin/master's"
m=$(git show origin/master:CHANGELOG.md | sed -n '/^## \[0\.6\.1\]/,/^## \[0\.6\.0\]/p' | sed '$d')
h=$(sed -n '/^## \[0\.6\.1\]/,/^## \[0\.6\.0\]/p' CHANGELOG.md | sed '$d')
if [ "$m" = "$h" ]; then
	printf '  \e[32m✓\e[0m matches\n'
	pass=$((pass + 1))
else
	printf '  \e[31m✗\e[0m differs\n'
	diff <(echo "$m") <(echo "$h") | head -20
	fail=$((fail + 1))
fi

section "CHANGELOG [Unreleased] is byte-identical to origin/master's"
m=$(git show origin/master:CHANGELOG.md | sed -n '/^## \[Unreleased\]/,/^## \[/p' | sed '$d')
h=$(sed -n '/^## \[Unreleased\]/,/^## \[/p' CHANGELOG.md | sed '$d')
if [ "$m" = "$h" ]; then
	printf '  \e[32m✓\e[0m matches\n'
	pass=$((pass + 1))
else
	printf '  \e[31m✗\e[0m differs\n'
	diff <(echo "$m") <(echo "$h") | head -20
	fail=$((fail + 1))
fi

section "no conflict markers anywhere in tracked files"
markers=$(git ls-files | xargs grep -lE '^<<<<<<<|^>>>>>>>|^======= *$' 2>/dev/null | grep -v plans/ || true)
if [ -z "$markers" ]; then
	printf '  \e[32m✓\e[0m clean\n'
	pass=$((pass + 1))
else
	printf '  \e[31m✗\e[0m markers found in:\n%s\n' "$markers"
	fail=$((fail + 1))
fi

section "build + tests"
if cargo check --workspace --all-targets --quiet 2>&1 | tail -5; then
	printf '  \e[32m✓\e[0m cargo check passes\n'
	pass=$((pass + 1))
else
	printf '  \e[31m✗\e[0m cargo check failed\n'
	fail=$((fail + 1))
fi

if out=$(cargo test --workspace --all-targets --quiet 2>&1); then
	n=$(echo "$out" | grep -oE '[0-9]+ passed' | head -1 | cut -d' ' -f1)
	printf '  \e[32m✓\e[0m cargo test: %s passed\n' "${n:-0}"
	pass=$((pass + 1))
else
	printf '  \e[31m✗\e[0m cargo test failed\n'
	echo "$out" | tail -10
	fail=$((fail + 1))
fi

if cargo clippy --workspace --all-targets --all-features --quiet 2>&1 | tail -5 >/dev/null; then
	printf '  \e[32m✓\e[0m cargo clippy clean (deny-level config)\n'
	pass=$((pass + 1))
else
	printf '  \e[31m✗\e[0m cargo clippy warnings/errors\n'
	fail=$((fail + 1))
fi

if dprint check >/dev/null 2>&1; then
	printf '  \e[32m✓\e[0m dprint check passes\n'
	pass=$((pass + 1))
else
	printf '  \e[31m✗\e[0m dprint check failed\n'
	fail=$((fail + 1))
fi

section "site"
if (cd site && bun run typecheck >/dev/null 2>&1); then
	printf '  \e[32m✓\e[0m bun typecheck passes\n'
	pass=$((pass + 1))
else
	printf '  \e[31m✗\e[0m bun typecheck failed\n'
	fail=$((fail + 1))
fi
if (cd site && bun run build >/dev/null 2>&1); then
	printf '  \e[32m✓\e[0m bun build passes\n'
	pass=$((pass + 1))
else
	printf '  \e[31m✗\e[0m bun build failed\n'
	fail=$((fail + 1))
fi

section "Cargo.lock dep drift vs master (non-failure, surfaced for awareness)"
for pkg in clap_complete libc; do
	mv=$(git show origin/master:Cargo.lock | awk -v p="$pkg" '$0 == "name = \"" p "\""{getline; print}')
	hv=$(awk -v p="$pkg" '$0 == "name = \"" p "\""{getline; print}' Cargo.lock)
	if [ "$mv" = "$hv" ]; then
		printf '  \e[32m✓\e[0m %s: master=%s HEAD=%s (no drift)\n' "$pkg" "$mv" "$hv"
		pass=$((pass + 1))
	else
		note "$pkg: master=$mv HEAD=$hv (drift)"
	fi
done

section "merge-tree dry-run vs origin/master (any conflicts?)"
mt_out=$(git merge-tree --write-tree --name-only HEAD origin/master 2>&1)
# first line is tree SHA; remaining lines (if any) are conflicting paths
conflict_lines=$(echo "$mt_out" | tail -n +2 | grep -v '^$' || true)
if [ -z "$conflict_lines" ]; then
	printf '  \e[32m✓\e[0m no further conflicts vs origin/master\n'
	pass=$((pass + 1))
else
	printf '  \e[31m✗\e[0m would conflict on:\n%s\n' "$conflict_lines"
	fail=$((fail + 1))
fi

section "summary"
printf '  pass: \e[32m%d\e[0m  fail: \e[31m%d\e[0m  warn: \e[33m%d\e[0m\n' "$pass" "$fail" "$warn"
exit "$fail"
