//! Runtime-generated shell completions with filtering for CLI relationships.
//!
//! The shell adapters pass the current command line to the hidden
//! `__complete` subcommand. Clap supplies option/value candidates; this module
//! removes candidates that would violate relationships enforced by the CLI.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use clap_complete::engine::{self, CompletionCandidate};

use crate::{Cli, profiles, registry, state_anchor};

pub(crate) fn script(shell: &str) -> Option<&'static str> {
    match shell {
        "bash" => Some(
            r#"_omp_topology_complete() {
    local candidate
    # COMP_WORDBREAKS splits values such as https://github.com/acme/repo
    # into fragments (https : / / github.com / acme / repo) that the
    # completion engine cannot recognize as one --source value. Rebuild
    # the words from COMP_LINE up to the cursor instead: only unquoted
    # whitespace and "=" separate words, quotes group and are removed,
    # a backslash quotes the next character (backslash-newline is a line
    # continuation), and every other character - including the
    # COMP_WORDBREAKS punctuation - stays inside its word.
    local line=${COMP_LINE:0:COMP_POINT}
    local -a words=()
    local i char quote='' token='' started=0
    for ((i = 0; i < ${#line}; i++)); do
        char=${line:i:1}
        if [[ -n $quote ]]; then
            if [[ $char == "$quote" ]]; then
                quote=''
            elif [[ $quote == '"' && $char == '\' ]]; then
                i=$((i + 1))
                if (( i < ${#line} )); then
                    char=${line:i:1}
                    case $char in
                        '"'|'\'|'$'|'`') token+=$char ;;
                        $'\n') ;;
                        *) token+="\\$char" ;;
                    esac
                else
                    token+='\'
                fi
            else
                token+=$char
            fi
        elif [[ $char == "'" || $char == '"' ]]; then
            quote=$char
            started=1
        elif [[ $char == '\' ]]; then
            i=$((i + 1))
            if (( i < ${#line} )); then
                char=${line:i:1}
                if [[ $char != $'\n' ]]; then
                    token+=$char
                fi
            else
                token+='\'
            fi
            started=1
        elif [[ $char == ' ' || $char == $'\t' || $char == $'\n' ]]; then
            if (( started )); then
                words+=("$token")
                token=''
                started=0
            elif (( ${#words[@]} > 1 )) && [[ ${words[$((${#words[@]} - 1))]} == "=" && ${words[$((${#words[@]} - 2))]} == -* ]]; then
                # An unquoted whitespace delimiter after a standalone "="
                # that follows an option completes an empty attached value
                # ("--option= <TAB>" or "--option= --next<TAB>"). Keep the
                # empty value as its own word so the normalization below
                # preserves it and the engine does not mistake the next
                # current word for the option's value.
                words+=("")
            fi
        elif [[ $char == '=' ]]; then
            if (( started )); then
                words+=("$token")
                token=''
                started=0
            fi
            words+=("=")
        else
            token+=$char
            started=1
        fi
    done
    if (( started )); then
        words+=("$token")
    elif [[ $line == *' ' || $line == *$'\t' || $line == *$'\n' ]]; then
        # The line ends with a separator, so the current word is empty. A
        # completed "--option=" right before that separator has an empty
        # value: add it as its own word so the engine receives the
        # completed value and the new current word as two empty arguments.
        if (( ${#words[@]} > 0 )) && [[ ${words[$((${#words[@]} - 1))]} == "=" ]]; then
            words+=("")
        fi
        words+=("")
    fi
    words=("${words[@]:1}")
    local -a normalized_words=()
    local i last_index
    for ((i = 0; i < ${#words[@]}; i++)); do
        if [[ ${words[i]} == "=" ]] && (( ${#normalized_words[@]} > 0 )); then
            last_index=$((${#normalized_words[@]} - 1))
            if [[ ${normalized_words[last_index]} == -* ]]; then
                # COMP_WORDBREAKS splits "--option=value-prefix" into
                # "--option", "=", and the value prefix; drop the standalone
                # "=" so the engine completes the value prefix as the current
                # word instead of returning "--option=value". When the "=" is
                # the last word the value prefix is empty, so add an empty
                # current word for the engine to complete the value against.
                if (( i + 1 >= ${#words[@]} )); then
                    normalized_words+=("")
                fi
            else
                # A later "=" belongs to the value itself (e.g. "a=b"); rejoin
                # it with the surrounding words.
                normalized_words[last_index]+="="
                if (( i + 1 < ${#words[@]} )); then
                    normalized_words[last_index]+="${words[i + 1]}"
                    i=$((i + 1))
                fi
            fi
        else
            normalized_words+=("${words[i]}")
        fi
    done
    words=("${normalized_words[@]}")
    COMPREPLY=()
    while IFS= read -r -d '' candidate; do
        COMPREPLY+=("$candidate")
    done < <(command omp-topology __complete -- "${words[@]}")
}
complete -o nosort -o filenames -F _omp_topology_complete omp-topology
"#,
        ),
        "fish" => Some(
            r#"function __omp_topology_complete
    # `commandline -opc` prints one raw token per line, so command
    # substitution cannot frame a completed token that contains an embedded
    # newline: the token splits into several words and a line such as
    # "--config-merge" inside a quoted path would turn into a real option.
    # In an interactive shell, walk the buffer backwards from the current
    # token instead, asking fish's tokenizer for the token at each cursor
    # position (commandline -tC reports the cursor offset inside the token,
    # so position minus offset is the token's start), and capture each token
    # value with the same string escape round-trip used below. -pC keeps the
    # walk inside the current process, matching the old -opc scoping.
    set -l words
    set -l cursor (commandline -C 2>/dev/null)
    # Reading the cursor can succeed under `complete -C` on newer fish even
    # though moving it is rejected there, so probe with a no-op move before
    # choosing the cursor walk.
    if test (count $cursor) -gt 0; and commandline -C $cursor 2>/dev/null
        set -l process_start (math $cursor - (commandline -pC))
        set -l position (math $cursor - (commandline -tC) - 1)
        while test $position -ge $process_start
            commandline -C $position
            set -l token (commandline -ot | string collect -N)
            set token (string escape -- "$token")
            set token (string replace -r -- '\\\\n$' '' "$token")
            eval "set token $token"
            if test (count $token) -gt 0
                set words $token $words
            end
            set position (math $position - (commandline -tC) - 1)
        end
        commandline -C $cursor
    else
        # Outside an interactive reader (e.g. `complete -C`) fish rejects
        # cursor moves, so the walk cannot run, and `commandline -opc`
        # prints one raw token per line, which cannot frame a completed
        # token containing an embedded newline. Tokenize the raw process
        # text instead: `commandline -pc` reports it as one string (string
        # collect keeps it a single element), and the loop below applies
        # fish's quoting rules - whitespace ends a token unless it sits
        # inside a command substitution, single quotes are literal, double
        # quotes and backslashes escape the next character, a
        # backslash-newline continuation removes both characters (inside
        # double quotes too), and `#` at the start of a token comments out
        # the rest of the line - inside a command substitution the comment
        # ends at the newline without ending the substitution, so its
        # quotes and parentheses never change nesting or tokenization -
        # so every completed token keeps its exact framing.
        # Variables, command substitutions, and wildcards stay unexpanded;
        # option filtering only needs the literal tokens. A trailing
        # unterminated token is the one being completed and is appended
        # below via commandline -ot.
        set -l raw (commandline -pc | string collect)
        set -l index 1
        set -l raw_length (string length -- "$raw")
        set -l quote ''
        set -l depth 0
        set -l token ''
        set -l started 0
        set -l comment 0
        set -l boundary 1
        while test $index -le $raw_length
            # Command substitution frames on newlines, so a newline character
            # comes back as two empty elements; detect and restore it.
            set -l char (string sub -s $index -l 1 -- "$raw")
            if test (count $char) -gt 1
                set char \n
            end
            if test $comment -eq 1
                # A comment inside a command substitution runs to the end
                # of the line; the newline ends the comment but not the
                # substitution, and the comment's quotes and parentheses
                # never change nesting or tokenization.
                if test "$char" = \n
                    set comment 0
                end
            else if test -n "$quote"
                set boundary 0
                if test "$char" = "$quote"
                    set quote ''
                else if test "$char" = '\\'
                    set index (math $index + 1)
                    if test $index -le $raw_length
                        set -l next (string sub -s $index -l 1 -- "$raw")
                        if test (count $next) -gt 1
                            set next \n
                        end
                        if test "$quote" = "'"
                            if test "$next" = "'" -o "$next" = '\\'
                                set token "$token$next"
                            else
                                set token "$token\\$next"
                            end
                        else
                            if test "$next" = \n
                                # A backslash-newline continuation is
                                # removed inside double quotes as well.
                            else if test "$next" = '"' -o "$next" = '\\' -o "$next" = '$'
                                set token "$token$next"
                            else
                                set token "$token\\$next"
                            end
                        end
                    else
                        set token "$token\\"
                    end
                else
                    set token "$token$char"
                end
            else if test "$char" = "'" -o "$char" = '"'
                set quote "$char"
                set started 1
                set boundary 0
            else if test "$char" = '\\'
                set boundary 0
                set index (math $index + 1)
                if test $index -le $raw_length
                    set -l next (string sub -s $index -l 1 -- "$raw")
                    if test (count $next) -gt 1
                        set next \n
                    end
                    if test "$next" = \n
                        # A backslash-newline continuation removes both
                        # characters, joining the text into one token.
                    else
                        set token "$token$next"
                        set started 1
                    end
                else
                    set token "$token\\"
                    set started 1
                end
            else if test "$char" = '('
                set depth (math $depth + 1)
                set token "$token$char"
                set started 1
                set boundary 1
            else if test "$char" = ')'
                if test $depth -gt 0
                    set depth (math $depth - 1)
                end
                set token "$token$char"
                set started 1
                set boundary 1
            else if test "$char" = ' ' -o "$char" = \t -o "$char" = \n
                set boundary 1
                if test $depth -gt 0
                    # Whitespace inside a command substitution stays in the
                    # token: the substitution is one word to the shell.
                    set token "$token$char"
                else if test $started -eq 1
                    set words $words "$token"
                    set token ''
                    set started 0
                end
            else if test "$char" = '#' -a $started -eq 0 -a $depth -eq 0
                break
            else if test "$char" = '#' -a $boundary -eq 1
                # Inside a command substitution `#` at a token boundary
                # starts a comment that runs to the end of the line.
                set comment 1
            else
                set token "$token$char"
                set started 1
                set boundary 0
            end
            set index (math $index + 1)
        end
    end
    # `commandline` appends one newline after the current token and command
    # substitution splits on newlines, which would break a token containing an
    # embedded newline into several words. Capture the token verbatim, escape
    # it onto a single line, drop the separator newline's escape sequence, and
    # let eval restore the exact token (string escape's documented purpose).
    set -l current (commandline -ot | string collect -N)
    set current (string escape -- "$current")
    set current (string replace -r -- '\\\\n$' '' "$current")
    eval "set current $current"
    if test (count $current) -eq 0
        set current ''
    end
    command omp-topology __complete -- $words "$current" | string split0
end
complete -c omp-topology -f -a '(__omp_topology_complete)'
"#,
        ),
        "zsh" => Some(
            r#"#compdef omp-topology

local candidate
local -a candidates completion_words
completion_words=("${(@)words[2,CURRENT]}")
candidates=()
while IFS= read -r -d "" candidate; do
    candidates+=("$candidate")
done < <(command omp-topology __complete -- "${completion_words[@]}")
compadd -- "${candidates[@]}"
"#,
        ),
        _ => None,
    }
}

pub(crate) fn complete_install_ids(current: &OsStr) -> Vec<CompletionCandidate> {
    let Ok(anchor) = state_anchor() else {
        return Vec::new();
    };
    let Ok(registry) = registry::load_registry(&anchor) else {
        return Vec::new();
    };
    let prefix = current.to_string_lossy();
    registry
        .aliases
        .keys()
        .filter(|id| id.starts_with(prefix.as_ref()))
        .map(|id| CompletionCandidate::new(id.clone()))
        .collect()
}

pub(crate) fn complete_profiles(current: &OsStr) -> Vec<CompletionCandidate> {
    let Ok(anchor) = state_anchor() else {
        return Vec::new();
    };
    let Ok(profiles) = profiles::discover_profiles(&anchor) else {
        return Vec::new();
    };
    let prefix = current.to_string_lossy();
    profiles
        .into_iter()
        .filter_map(|profile| profile.name)
        .filter(|name| name.starts_with(prefix.as_ref()))
        .map(CompletionCandidate::new)
        .collect()
}

pub(crate) fn complete(words: Vec<OsString>) {
    let mut words = normalize_words(words);
    if words.len() == 1 {
        words.push(OsString::new());
    }
    let current_index = words.len() - 1;
    let mut command = <Cli as clap::CommandFactory>::command();
    let current_dir = std::env::current_dir().ok();
    let candidates = engine::complete(
        &mut command,
        words.clone(),
        current_index,
        current_dir.as_deref(),
    )
    .unwrap_or_default();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for candidate in candidates {
        let value = candidate.get_value();
        if !valid_candidate(value, &words, current_index) {
            continue;
        }
        let _ = out.write_all(value.as_bytes());
        let _ = out.write_all(b"\0");
    }
}

fn normalize_words(mut words: Vec<OsString>) -> Vec<OsString> {
    let command_is_present = words
        .first()
        .is_some_and(|word| Path::new(word).file_name() == Some(OsStr::new("omp-topology")));
    if !command_is_present {
        words.insert(0, OsString::from("omp-topology"));
    }
    words
}

fn valid_candidate(candidate: &OsStr, words: &[OsString], current_index: usize) -> bool {
    let prior = words.get(1..current_index).unwrap_or_default();
    let command = prior
        .iter()
        .filter_map(|word| word.to_str())
        .find(|word| !word.starts_with('-'))
        .unwrap_or_default();
    let has = |option: &str| option_present(prior, option);
    let is = |option: &str| candidate == OsStr::new(option);

    if candidate.to_str().is_some_and(&has) {
        return false;
    }

    match command {
        "doctor" | "status" | "remove" => {
            if has("--install-id") && (is("--profile") || is("--agent-dir"))
                || has("--profile") && (is("--install-id") || is("--agent-dir"))
                || has("--agent-dir") && (is("--install-id") || is("--profile"))
            {
                return false;
            }
        }
        "install" => {
            if has("--agent-dir") && (is("--profile") || is("--alias"))
                || has("--profile") && is("--agent-dir")
                || has("--alias") && is("--agent-dir")
                || is("--alias") && !has("--profile")
                || is("--ref") && !has("--source")
                || has("--config-merge") && is("--config-replace")
                || has("--config-replace") && (is("--config-merge") || is("--config-precedence"))
                || is("--config-precedence") && !has("--config-merge")
            {
                return false;
            }
        }
        "list" => {
            if is("--ref") && !has("--source") {
                return false;
            }
        }
        "update"
            if is("--ref") && !update_ref_is_github_source(prior)
                || has("--config-merge") && is("--config-replace")
                || has("--config-replace")
                    && (is("--config-merge") || is("--config-precedence"))
                || is("--config-precedence") && !has("--config-merge") =>
        {
            return false;
        }
        _ => {}
    }
    true
}

fn update_ref_is_github_source(words: &[OsString]) -> bool {
    if option_present(words, "--source") {
        return option_value(words, "--source").is_some_and(|spec| {
            matches!(
                crate::source::TopologySource::parse(spec),
                Ok(crate::source::TopologySource::GitHub { .. })
            )
        });
    }

    let Some(install_id) = option_value(words, "--install-id") else {
        return false;
    };
    let Ok(anchor) = state_anchor() else {
        return false;
    };
    let Ok(registry) = registry::load_registry(&anchor) else {
        return false;
    };
    matches!(
        registry.aliases.get(install_id).map(|entry| &entry.source),
        Some(registry::SourceSpec::GitHub { .. })
    )
}

fn option_value<'a>(words: &'a [OsString], option: &str) -> Option<&'a str> {
    for (index, word) in words.iter().enumerate() {
        let Some(word) = word.to_str() else {
            continue;
        };
        if word == option {
            return words.get(index + 1)?.to_str();
        }
        if let Some(value) = word
            .strip_prefix(option)
            .and_then(|suffix| suffix.strip_prefix('='))
        {
            return Some(value);
        }
    }
    None
}

fn option_present(words: &[OsString], option: &str) -> bool {
    words.iter().any(|word| {
        let word = word.to_string_lossy();
        word == option
            || word
                .strip_prefix(option)
                .is_some_and(|suffix| suffix.starts_with('='))
    })
}
