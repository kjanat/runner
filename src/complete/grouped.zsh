#compdef {BIN}
#
# Dynamic completion for {BIN} with tag-based grouping.
#
# Placeholders replaced at registration time by the Rust binary:
#   {NAME}       — sanitised binary name (function identifier)
#   {BIN}        — binary name as seen by the shell
#   {COMPLETER}  — path to the binary that produces completions
#   {VAR}        — env-var that activates CompleteEnv (default: COMPLETE)
#
# The binary outputs one candidate per line:
#   TAG \x1f VALUE [\t DESCRIPTION]
# where \x1f separates the group tag from the entry, and \t separates
# the completion value from its optional description.
#
# Each unique TAG gets its own `compadd -V` group with a `-X` header,
# producing visible "-- TAG --" sections in the completion menu.
#
# All locals use the __runner_ prefix to avoid shadowing zsh builtins
# like _tags, _describe, etc.

function _clap_dynamic_completer_{NAME}() {
    # Reset to zsh defaults with local scope so caller-side settings
    # (XTRACE, shwordsplit, nullglob, aliases, …) don't leak into us
    # and our tracing doesn't bleed into their prompt. `-o NULL_GLOB`
    # silences "no matches found" errors that `_files` internals and
    # user zstyles (e.g. specs tagged `globbed-files`) would otherwise
    # raise into the prompt when an unquoted `*` fails to match — and,
    # unlike `NO_NOMATCH`, it does so without leaving the literal glob
    # (e.g. `*(/)` from `_files -/` in a dir with no subdirectories)
    # as a candidate that then gets inserted into the command line.
    emulate -L zsh -o NULL_GLOB

    local __runner_idx=$(( CURRENT - 1 ))
    local __runner_ifs=$'\n'

    # Call the binary in completion mode.
    local __runner_raw=("${(@f)$( \
        _CLAP_IFS="$__runner_ifs" \
        _CLAP_COMPLETE_INDEX="$__runner_idx" \
        {VAR}="zsh" \
        {COMPLETER} -- "${words[@]}" 2>/dev/null \
    )}")

    [[ -z "$__runner_raw" ]] && return

    # Path-hint delegation: when the Rust completer returns the path
    # sentinel, hand off to zsh's `_files` builtin so tilde (`~/foo`,
    # `~named-dir/`), globs, and `cdpath` all work natively.
    if [[ "${__runner_raw[1]}" == __CLAP_PATHFILES__* ]]; then
        local __runner_flags="${${__runner_raw[1]}#__CLAP_PATHFILES__}"
        __runner_flags="${__runner_flags#$'\t'}"
        if [[ -n "$__runner_flags" ]]; then
            # Disable globbing so patterns like `*(*)` reach `_files`
            # literally (it interprets them itself); `emulate -L zsh`
            # at the top of this function restores the option on return.
            setopt noglob
            _files ${=__runner_flags}
        else
            _files
        fi
        return
    fi

    # --- Pass 1: collect unique tags in insertion order ---------------
    local -a __runner_grps=()
    local __runner_ln
    for __runner_ln in "${__runner_raw[@]}"; do
        local __runner_g="${__runner_ln%%$'\x1f'*}"
        if (( ! ${__runner_grps[(Ie)$__runner_g]} )); then
            __runner_grps+=("$__runner_g")
        fi
    done

    # --- Pass 2: build per-group arrays and compadd each -------------
    local __runner_g
    for __runner_g in "${__runner_grps[@]}"; do
        local -a __runner_vals=()
        local -a __runner_dsps=()
        for __runner_ln in "${__runner_raw[@]}"; do
            if [[ "${__runner_ln%%$'\x1f'*}" == "$__runner_g" ]]; then
                local __runner_e="${__runner_ln#*$'\x1f'}"
                # Split value and optional description on \t
                if [[ "$__runner_e" == *$'\t'* ]]; then
                    __runner_vals+=("${__runner_e%%$'\t'*}")
                    __runner_dsps+=("${(r:30:)${__runner_e%%$'\t'*}} -- ${__runner_e#*$'\t'}")
                else
                    __runner_vals+=("$__runner_e")
                    __runner_dsps+=("$__runner_e")
                fi
            fi
        done
        if (( ${#__runner_vals} )); then
            compadd -V "$__runner_g" -X "-- $__runner_g --" \
                -d __runner_dsps -a -- __runner_vals
        fi
    done
}

compdef _clap_dynamic_completer_{NAME} {BIN}
