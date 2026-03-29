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

function _clap_dynamic_completer_NAME() {
    # CURRENT is 1-based in zsh; the completion engine expects 0-based.
    local _CLAP_COMPLETE_INDEX=$(expr $CURRENT - 1)
    local _CLAP_IFS=$'\n'

    # Invoke the binary in completion mode.  The VAR env-var tells
    # CompleteEnv to emit completions instead of running normally.
    # Words after "--" are the command line tokens typed so far.
    local raw=("${(@f)$( \
        _CLAP_IFS="$_CLAP_IFS" \
        _CLAP_COMPLETE_INDEX="$_CLAP_COMPLETE_INDEX" \
        VAR="zsh" \
        COMPLETER -- "${words[@]}" 2>/dev/null \
    )}")

    [[ -z "$raw" ]] && return

    # --- Pass 1: collect unique tags in insertion order ---------------
    local -a _tags=()
    local _line
    for _line in "${raw[@]}"; do
        local _tag="${_line%%$'\x1f'*}"          # everything before \x1f
        if (( ! ${_tags[(Ie)$_tag]} )); then     # (Ie) = exact-match index
            _tags+=("$_tag")
        fi
    done

    # --- Pass 2: group entries by tag and _describe each group -------
    local _tag
    for _tag in "${_tags[@]}"; do
        local -a _entries=()
        for _line in "${raw[@]}"; do
            if [[ "${_line%%$'\x1f'*}" == "$_tag" ]]; then
                _entries+=("${_line#*$'\x1f'}")   # everything after \x1f
            fi
        done
        # _describe renders the group header ("-- TAG --") and lists
        # entries as "value -- description".
        (( ${#_entries} )) && _describe "$_tag" _entries
    done
}

compdef _clap_dynamic_completer_NAME BIN
