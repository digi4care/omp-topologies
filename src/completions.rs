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
    local -a words=("${COMP_WORDS[@]:1:COMP_CWORD}")
    if (( COMP_CWORD >= ${#COMP_WORDS[@]} )); then
        words+=("")
    fi
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
    if test (count $cursor) -gt 0
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
        # Cursor access is unavailable outside an interactive reader (e.g.
        # `complete -C`), and fish exposes no boundary-safe token listing in
        # that mode, so keep the legacy line-oriented token dump there.
        set words (commandline -opc)
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
