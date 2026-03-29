#compdef BIN
#
# Dynamic completion for BIN with tag-based grouping.
#
# Placeholders replaced at build time by the Rust binary:
#   NAME       — sanitised binary name (used for the function name)
#   BIN        — binary name as seen by the shell (e.g. "runner", "run")
#   COMPLETER  — path to the binary that produces completions
#   VAR        — env-var name that activates CompleteEnv (default: COMPLETE)
#
# The binary outputs one candidate per line in the format:
#   TAG\x1fVALUE:DESCRIPTION
# where \x1f (ASCII Unit Separator) delimits the group tag from the
# standard zsh "value:description" pair.  Each unique TAG becomes a
# separate _describe call, which zsh renders as a "-- TAG --" header.
#
# NOTE: All local variables use the __runner_ prefix to avoid collisions
# with zsh's completion system internals (_tags, _describe, etc.).

function _clap_dynamic_completer_NAME() {
    # CURRENT is 1-based in zsh; the completion engine expects 0-based.
    local __runner_idx=$(expr $CURRENT - 1)
    local __runner_ifs=$'\n'

    # Invoke the binary in completion mode.  The VAR env-var tells
    # CompleteEnv to emit completions instead of running normally.
    # Words after "--" are the command line tokens typed so far.
    local __runner_raw=("${(@f)$( \
        _CLAP_IFS="$__runner_ifs" \
        _CLAP_COMPLETE_INDEX="$__runner_idx" \
        VAR="zsh" \
        COMPLETER -- "${words[@]}" 2>/dev/null \
    )}")

    [[ -z "$__runner_raw" ]] && return

    # --- Pass 1: collect unique tags in insertion order ---------------
    local -a __runner_grps=()
    local __runner_ln
    for __runner_ln in "${__runner_raw[@]}"; do
        local __runner_g="${__runner_ln%%$'\x1f'*}"          # everything before \x1f
        if (( ! ${__runner_grps[(Ie)$__runner_g]} )); then   # (Ie) = exact-match index
            __runner_grps+=("$__runner_g")
        fi
    done

    # --- Pass 2: group entries by tag and _describe each group -------
    local __runner_g
    for __runner_g in "${__runner_grps[@]}"; do
        local -a __runner_ent=()
        for __runner_ln in "${__runner_raw[@]}"; do
            if [[ "${__runner_ln%%$'\x1f'*}" == "$__runner_g" ]]; then
                __runner_ent+=("${__runner_ln#*$'\x1f'}")    # everything after \x1f
            fi
        done
        # _describe renders the group header ("-- TAG --") and lists
        # entries as "value -- description".
        (( ${#__runner_ent} )) && _describe "$__runner_g" __runner_ent
    done
}

compdef _clap_dynamic_completer_NAME BIN
