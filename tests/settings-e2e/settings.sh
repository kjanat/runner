#!/usr/bin/env bash
# Settings behaviour end to end, through the `bin/runner` and `bin/run` shims.
set -u -o pipefail
HERE=$(cd "${BASH_SOURCE[0]%/*}" && pwd)
REPO=$(cd "${HERE}/../.." && pwd)
PATH="${REPO}/bin:${PATH}"
runner --version
run --version
T=$(mktemp -d)
CARGO=$(command -v cargo)
SYS=${REPO}/bin:/usr/bin:/bin:${CARGO%/*}

stub() { # dir name body
	mkdir -p "${1}"
	printf '#!/bin/sh\n%s\n' "${3}" >"${1}/${2}"
	chmod +x "${1}/${2}"
}
logstub() { # dir name
	stub "${1}" "${2}" "if [ \"\$1\" = --version ]; then echo 11.0.0; exit 0; fi; echo \"${2} \$*\" >> \"\$LOG\""
}
say() { printf -- '--- %s\n' "$*"; }
ran() { if [[ -e ${1} ]]; then echo yes; else echo no; fi; }

printf '=== a command-line choice displaces the opposite one from the environment\n'
P=${T}/one
mkdir -p "${P}"
printf '#!/bin/sh\nsleep 0.5\ntouch "%s/done"\n' "${P}" >"${P}/first.sh"
printf '#!/bin/sh\nif [ -e "%s/done" ]; then echo ORDER-OK; else echo ORDER-BAD; fi\n' "${P}" >"${P}/second.sh"
chmod +x "${P}/first.sh" "${P}/second.sh"
for bin in runner run; do
	rm -f "${P}/done"
	sub=()
	if [[ ${bin} == runner ]]; then sub=(run); fi
	say "RUNNER_RUN_PARALLEL=1 ${bin} ${sub[*]+${sub[*]}} -s ./first.sh ./second.sh"
	(cd "${P}" && env RUNNER_RUN_PARALLEL=1 "${bin}" ${sub[@]+"${sub[@]}"} -s ./first.sh ./second.sh 2>&1 | grep ORDER)
done
jq -n '{name:"one",private:true,scripts:{a:"true"}}' >"${P}/package.json"
say "RUNNER_LIST_JSON=1 runner list --raw"
(cd "${P}" && env RUNNER_LIST_JSON=1 runner list --raw 2>&1)

printf '\n=== the later of --flag and --no-flag wins on either side of a subcommand\n'
P=${T}/two
mkdir -p "${P}/bin"
jq -n '{name:"two",private:true,packageManager:"npm@11.0.0"}' >"${P}/package.json"
echo '{}' >"${P}/package-lock.json"
logstub "${P}/bin" npm
stub "${P}/bin" node 'echo v22.0.0'
for line in "--runtime node --download install --no-download --no-tools" \
	"--runtime node --download --no-download install --no-tools" \
	"--runtime node --no-download install --download --no-tools"; do
	read -ra args <<<"${line}"
	rm -f "${P}/log"
	say "runner ${line}"
	(
		cd "${P}" && env PATH="${P}/bin:${SYS}" LOG="${P}/log" runner "${args[@]}" 2>&1 | grep -iE 'refus|error'
		printf 'exit=%s installer-ran=' "${PIPESTATUS[0]}"
		ran "${P}/log"
	)
done

printf '\n=== a provider only a task table chooses serves that task alone\n'
P=${T}/three
mkdir -p "${P}/bin"
jq -n '{name:"three",private:true,packageManager:"npm@11.0.0",scripts:{build:"true",lint:"true"}}' >"${P}/package.json"
echo 'console.log(1)' >"${P}/check.js"
for t in npm node bun deno; do logstub "${P}/bin" "${t}"; done
for cfg in '' '[tasks.build.runtime]\njavascript = "bun"\n' '[tasks.build]\npm = "bun"\n'; do
	printf '%b' "${cfg}" >"${P}/runner.toml"
	for target in ./check.js build lint; do
		say "config: ${cfg//\\n/ } ; run ${target}"
		(
			cd "${P}" && rm -f log && env PATH="${P}/bin:${SYS}" LOG="${P}/log" runner run "${target}" >/dev/null 2>&1
			cat "${P}/log"
		)
	done
done
printf '[tasks.build.runtime]\njavascript = "bun"\n' >"${P}/runner.toml"
say "run binary: run ./check.js with task-only bun"
(
	cd "${P}" && rm -f log && env PATH="${P}/bin:${SYS}" LOG="${P}/log" run ./check.js >/dev/null 2>&1
	cat "${P}/log"
)
printf '[tasks.build]\npm = "go"\n' >"${P}/runner.toml"
for target in build lint; do
	say "config: [tasks.build] pm = \"go\" ; run ${target}"
	(
		cd "${P}" && rm -f log && env PATH="${P}/bin:${SYS}" LOG="${P}/log" runner run "${target}" 2>&1 | grep -E 'cannot|Error'
		printf 'exit=%s ran=' "${PIPESTATUS[0]}"
		if [[ -e ${P}/log ]]; then cat "${P}/log"; else echo nothing; fi
	)
done
printf '[tasks.build.runtime]\njavascript = "bun"\n' >"${P}/runner.toml"
say "doctor installers with task-only bun"
(cd "${P}" && env PATH="${P}/bin:${SYS}" runner doctor 2>&1 | grep -A3 '^Decisions')
Q=${T}/three-deno
mkdir -p "${Q}/bin"
jq -n '{name:"td",private:true,scripts:{build:"true"}}' >"${Q}/package.json"
for t in npm node deno; do logstub "${Q}/bin" "${t}"; done
say "no PM declaration, install default without task config"
(
	cd "${Q}" && rm -f log runner.toml && env PATH="${Q}/bin:${SYS}" LOG="${Q}/log" runner install --no-tools >/dev/null 2>&1
	cat "${Q}/log"
)
printf '[tasks.build.runtime]\njavascript = "deno"\n' >"${Q}/runner.toml"
say "no PM declaration, install default with task-only deno"
(
	cd "${Q}" && rm -f log && env PATH="${Q}/bin:${SYS}" LOG="${Q}/log" runner install --no-tools >/dev/null 2>&1
	cat "${Q}/log"
)

printf '\n=== a source choice refuses names that source does not define\n'
P=${T}/four
mkdir -p "${P}/bin"
printf 'hello:\n\techo hi\n' >"${P}/justfile"
logstub "${P}/bin" absent_tool
say "runner --source just run absent_tool"
(
	cd "${P}" && rm -f log && env PATH="${P}/bin:${SYS}" LOG="${P}/log" runner --source just run absent_tool 2>&1
	printf 'exit=%s ran=' "$?"
	ran "${P}/log"
)
say "RUNNER_SOURCE=just runner run absent_tool"
(
	cd "${P}" && rm -f log && env PATH="${P}/bin:${SYS}" LOG="${P}/log" RUNNER_SOURCE=just runner run absent_tool 2>&1
	printf 'exit=%s ran=' "$?"
	ran "${P}/log"
)
say "RUNNER_SOURCE=just run absent_tool"
(
	cd "${P}" && rm -f log && env PATH="${P}/bin:${SYS}" LOG="${P}/log" RUNNER_SOURCE=just run absent_tool 2>&1
	printf 'exit=%s ran=' "$?"
	ran "${P}/log"
)
printf '[tasks.absent_tool]\nsource = "just"\n' >"${P}/runner.toml"
say "[tasks.absent_tool] source = just"
(
	cd "${P}" && rm -f log && env PATH="${P}/bin:${SYS}" LOG="${P}/log" runner run absent_tool 2>&1
	printf 'exit=%s ran=' "$?"
	ran "${P}/log"
)
rm -f "${P}/runner.toml"
say "absent from PATH too, dry-run"
(cd "${P}" && env PATH="${P}/bin:${SYS}" runner --source just --dry-run run no_such_tool_anywhere 2>&1)
say "the chosen runner's own entry point still runs"
JUST=$(command -v just)
(cd "${P}" && env PATH="${P}/bin:${SYS}:${JUST%/*}" runner --source just --dry-run run just 2>&1 | grep -E 'argv|Error')

printf '\n=== RUNNER_QUIET and -q each expand at their own layer\n'
P=${T}/five
mkdir -p "${P}/bin"
jq -n '{name:"five",private:true,packageManager:"npm@11.0.0",scripts:{build:"true"}}' >"${P}/package.json"
echo '{}' >"${P}/pnpm-lock.yaml"
logstub "${P}/bin" npm
stub "${P}/bin" node 'echo v22.0.0'
logstub "${P}/bin" pnpm
for bin in runner run; do
	sub=()
	if [[ ${bin} == runner ]]; then sub=(run); fi
	for q in "" "-q" "--warnings"; do
		flag=()
		if [[ -n ${q} ]]; then flag=("${q}"); fi
		say "RUNNER_QUIET=2 ${bin} ${q} ${sub[*]+${sub[*]}} build"
		(
			cd "${P}" && rm -f log && env PATH="${P}/bin:${SYS}" LOG="${P}/log" RUNNER_QUIET=2 "${bin}" ${flag[@]+"${flag[@]}"} ${sub[@]+"${sub[@]}"} build 2>&1 | grep -c 'warn'
			cat "${P}/log"
		)
	done
done

printf '\n=== list, doctor and why report what run executes\n'
P=${T}/six
mkdir -p "${P}/bin"
jq -n '{name:"six",private:true,packageManager:"npm@11.0.0",scripts:{build:"true"}}' >"${P}/package.json"
echo '{}' >"${P}/package-lock.json"
printf 'build:\n\techo just-build\n' >"${P}/justfile"
for t in npm node bun just; do logstub "${P}/bin" "${t}"; done
printf '[tasks.build]\nsource = "just"\n' >"${P}/runner.toml"
say "list"
(cd "${P}" && env PATH="${P}/bin:${SYS}" runner list 2>&1 | grep 'build:')
say "doctor --json selected"
(cd "${P}" && env PATH="${P}/bin:${SYS}" runner doctor --json 2>/dev/null | jq -c '[.conflicts[] | .selected]')
say "why --json selected"
(cd "${P}" && env PATH="${P}/bin:${SYS}" runner why build --json 2>/dev/null | jq -c '.selected.task.fqn')
say "dry-run argv"
(cd "${P}" && env PATH="${P}/bin:${SYS}" runner --dry-run run build 2>&1 | grep 'argv')
rm -f "${P}/justfile"
for cfg in '[runtime]\njavascript = "node"\n\n[tasks.build.runtime]\njavascript = "bun"\n' '[tasks.build.runtime]\njavascript = "bun"\n'; do
	printf '%b' "${cfg}" >"${P}/runner.toml"
	say "config: ${cfg//\\n/ }"
	(cd "${P}" && env PATH="${P}/bin:${SYS}" runner why build --json 2>/dev/null | jq -c '.runtime')
	(
		cd "${P}" && rm -f log && env PATH="${P}/bin:${SYS}" LOG="${P}/log" runner run build >/dev/null 2>&1
		cat "${P}/log"
	)
done

printf '\n=== runner lsp resolves quoted task keys\n'
python3 "${HERE}/lsp_quoted_keys.py" runner
printf '\nfixtures: %s\n' "${T}"
