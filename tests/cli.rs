use std::ffi::OsString;
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn run_cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_omp-topology"))
        .env_remove("OMP_HOME")
        .env_remove("OMP_PROFILE")
        .env_remove("OMP_TEST_AGENT_DIR")
        .env_remove("OMP_TEST_CREATE_DIR")
        .env_remove("OMP_TEST_OMP_LOG")
        .env_remove("OMP_TEST_EXPECTED_PROFILE")
        .env_remove("OMP_TEST_EXTERNAL_LOG")
        .args(args)
        .output()
        .expect("failed to execute omp-topology")
}

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before UNIX epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("omp-topology-cli-{}-{unique}", std::process::id()));
        fs::create_dir_all(&path).expect("failed to create isolated test directory");
        Self(path)
    }

    fn cli_command(&self, cwd: &Path) -> Command {
        let home = self.0.join("home");
        let xdg_config = self.0.join("xdg-config");
        fs::create_dir_all(&home).expect("failed to create isolated HOME");
        fs::create_dir_all(&xdg_config).expect("failed to create isolated XDG_CONFIG_HOME");

        let mut command = Command::new(env!("CARGO_BIN_EXE_omp-topology"));
        command
            .current_dir(cwd)
            .env_remove("OMP_HOME")
            .env_remove("OMP_PROFILE")
            .env_remove("OMP_TEST_AGENT_DIR")
            .env_remove("OMP_TEST_CREATE_DIR")
            .env_remove("OMP_TEST_OMP_LOG")
            .env_remove("OMP_TEST_EXPECTED_PROFILE")
            .env_remove("OMP_TEST_EXTERNAL_LOG")
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", xdg_config)
            .env("XDG_STATE_HOME", self.0.join("state"));
        command
    }

    fn configure_fake_omp(&self, command: &mut Command, agent_dir: &Path, create_dir: bool) {
        let bin_dir = self.0.join("bin");
        fs::create_dir_all(&bin_dir).expect("failed to create fake PATH");
        let fake_omp = bin_dir.join("omp");
        let invocation_log = self.0.join("omp-invocations.log");
        fs::write(
            &fake_omp,
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> \"$OMP_TEST_OMP_LOG\"\n\
             if [ \"$1\" = \"--profile\" ]; then shift 2; fi\n\
             if [ \"$1\" != \"config\" ] || [ \"$2\" != \"path\" ]; then exit 64; fi\n\
             if [ \"$OMP_TEST_CREATE_DIR\" = 1 ]; then mkdir -p \"$OMP_TEST_AGENT_DIR\" || exit 1; fi\n\
             printf '%s\\n' \"$OMP_TEST_AGENT_DIR\"\n",
        )
        .expect("failed to write fake omp");
        fs::set_permissions(&fake_omp, fs::Permissions::from_mode(0o755))
            .expect("failed to make fake omp executable");

        let inherited_path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(bin_dir).chain(std::env::split_paths(&inherited_path)),
        )
        .expect("failed to build isolated PATH");
        command
            .env("PATH", path)
            .env("OMP_TEST_AGENT_DIR", agent_dir)
            .env("OMP_TEST_CREATE_DIR", if create_dir { "1" } else { "0" })
            .env("OMP_TEST_OMP_LOG", invocation_log);
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn help_shows_description_and_version_option() {
    let output = run_cli(&["--help"]);
    assert!(output.status.success(), "--help failed: {output:?}");

    let stdout = String::from_utf8(output.stdout).expect("--help output was not UTF-8");
    assert!(
        stdout.contains("Install and manage OMP topology profiles"),
        "--help omitted the command description: {stdout}"
    );
    assert!(
        stdout.contains("--version"),
        "--help omitted the --version option: {stdout}"
    );
}
#[test]
fn cli_rejects_legacy_update_alias_before_state_or_target_access() {
    let temp = TestDir::new();
    let default_agent_dir = temp.0.join("default-agent");
    let blocked_state_home = temp.0.join("blocked-state-home");
    let state_sentinel = b"state access must not happen\n";
    fs::write(&blocked_state_home, state_sentinel).expect("failed to block state home");

    let mut command = temp.cli_command(Path::new(env!("CARGO_MANIFEST_DIR")));
    command.args(["update", "--alias", "omp-dps"]);
    command.env("XDG_STATE_HOME", &blocked_state_home);
    temp.configure_fake_omp(&mut command, &default_agent_dir, true);

    let output = command.output().expect("failed to execute omp-topology");
    assert_eq!(
        output.status.code(),
        Some(2),
        "legacy update option did not return clap exit 2: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Usage:")
            && stderr.contains("omp-topology update")
            && stderr.contains("unexpected argument")
            && stderr.contains("--alias"),
        "removed update --alias was not rejected as an unknown clap argument:\n{stderr}"
    );
    assert_eq!(
        fs::read(&blocked_state_home).expect("blocked state sentinel disappeared"),
        state_sentinel,
        "invalid update accessed or mutated the blocked state path"
    );
    assert!(
        !default_agent_dir.exists(),
        "invalid update resolved or created the target before clap rejected the option"
    );
}

#[test]
fn cli_guidance_install_help_documents_shell_alias_profile_requirement() {
    let output = run_cli(&["install", "--help"]);
    assert!(output.status.success(), "install --help failed: {output:?}");
    let stdout = String::from_utf8(output.stdout).expect("help output was not UTF-8");
    let lower = stdout.to_ascii_lowercase();
    assert!(
        lower.contains("requires --profile")
            && lower.contains("shell alias is independent of the registry install id")
            && lower.contains("never influences install selection"),
        "install help did not document the shell-alias requirements and independent install ID:\n{stdout}"
    );
    assert!(
        stdout.contains("--profile") && stdout.contains("--agent-dir"),
        "install help omitted --profile or --agent-dir target guidance:\n{stdout}"
    );
}

#[test]
fn version_prints_exact_package_version() {
    let output = run_cli(&["--version"]);
    assert!(output.status.success(), "--version failed: {output:?}");

    let stdout = String::from_utf8(output.stdout).expect("--version output was not UTF-8");
    assert!(
        stdout.trim_end() == concat!("omp-topology ", env!("CARGO_PKG_VERSION")),
        "unexpected --version output: {stdout:?}"
    );
}

#[test]
fn unknown_argument_returns_clap_usage_error() {
    let output = run_cli(&["--unknown-argument"]);
    assert!(
        !output.status.success(),
        "unknown argument unexpectedly succeeded"
    );

    let stderr = String::from_utf8(output.stderr).expect("error output was not UTF-8");
    assert!(
        stderr.contains("error:") && stderr.contains("Usage:"),
        "expected a clap usage error, got: {stderr}"
    );
}

#[test]
fn install_dry_run_reports_shell_alias_without_creating_it() {
    let temp = TestDir::new();
    let profile_agent = temp.0.join("home/.omp/profiles/demo/agent");
    let home = temp.0.join("home");
    let xdg_config = temp.0.join("xdg-config");
    fs::create_dir_all(&home).expect("failed to create isolated HOME");
    fs::create_dir_all(&xdg_config).expect("failed to create isolated XDG_CONFIG_HOME");
    fs::write(home.join(".bashrc"), b"# existing shell configuration\n")
        .expect("failed to seed shell configuration");
    let home_before = cli_file_snapshot(&home);
    let xdg_config_before = cli_file_snapshot(&xdg_config);
    let mut command = temp.cli_command(Path::new(env!("CARGO_MANIFEST_DIR")));
    command.args([
        "install",
        "--topology",
        "digital-product-studio",
        "--profile",
        "demo",
        "--alias",
        "demo-alias",
        "--dry-run",
    ]);
    temp.configure_fake_omp(&mut command, &profile_agent, false);

    let output = command.output().expect("failed to execute omp-topology");
    assert!(
        output.status.success(),
        "install --dry-run failed: {output:?}"
    );
    let stdout = String::from_utf8(output.stdout).expect("CLI output was not UTF-8");
    let invocation_log = fs::read_to_string(temp.0.join("omp-invocations.log"))
        .expect("fake OMP invocation log is missing");
    assert_eq!(
        invocation_log, "--profile demo config path\n",
        "dry-run invoked OMP shell-alias creation"
    );
    assert_eq!(
        cli_file_snapshot(&home),
        home_before,
        "install --dry-run mutated HOME shell configuration"
    );
    assert_eq!(
        cli_file_snapshot(&xdg_config),
        xdg_config_before,
        "install --dry-run mutated XDG shell configuration"
    );
    assert!(
        stdout.contains(&profile_agent.display().to_string()),
        "dry-run did not resolve the requested profile target: {stdout}"
    );
    assert!(
        !profile_agent.exists(),
        "install --dry-run created the missing profile directory"
    );
    assert!(
        !temp.0.join("state").exists(),
        "install --dry-run created installer state"
    );
    assert!(
        stdout.contains("Would create OMP shell alias 'demo-alias' for profile 'demo'"),
        "dry-run omitted the planned OMP shell alias: {stdout}"
    );
}

type CliTreeSnapshotEntry = (PathBuf, u8, u32, Vec<u8>);
type CliTreeSnapshot = Option<Vec<CliTreeSnapshotEntry>>;

fn cli_file_snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn visit(root: &Path, directory: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) {
        for entry in fs::read_dir(directory).expect("failed to read snapshot directory") {
            let entry = entry.expect("failed to read snapshot entry");
            let path = entry.path();
            if entry
                .file_type()
                .expect("failed to inspect snapshot entry")
                .is_dir()
            {
                visit(root, &path, files);
            } else {
                files.push((
                    path.strip_prefix(root)
                        .expect("snapshot path escaped its root")
                        .to_path_buf(),
                    fs::read(path).expect("failed to read snapshot file"),
                ));
            }
        }
    }

    let mut files = Vec::new();
    visit(root, root, &mut files);
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}

#[test]
fn status_does_not_create_missing_profile_directory() {
    let temp = TestDir::new();
    let profile_agent = temp.0.join("home/.omp/profiles/regression/agent");
    let mut command = temp.cli_command(Path::new(env!("CARGO_MANIFEST_DIR")));
    command.args(["status", "--profile", "regression", "--json"]);
    temp.configure_fake_omp(&mut command, &profile_agent, true);

    let output = command.output().expect("failed to execute omp-topology");
    assert!(output.status.success(), "status failed: {output:?}");
    assert!(
        !profile_agent.exists(),
        "read-only status created the missing profile directory"
    );
}

#[test]
fn remove_dry_run_does_not_create_missing_profile_directory() {
    let temp = TestDir::new();
    let profile_agent = temp.0.join("home/.omp/profiles/regression/agent");
    let mut command = temp.cli_command(Path::new(env!("CARGO_MANIFEST_DIR")));
    command.args([
        "remove",
        "--topology",
        "digital-product-studio",
        "--profile",
        "regression",
        "--dry-run",
    ]);
    temp.configure_fake_omp(&mut command, &profile_agent, true);

    let output = command.output().expect("failed to execute omp-topology");
    assert!(
        output.status.success(),
        "remove --dry-run failed: {output:?}"
    );
    assert!(
        !profile_agent.exists(),
        "remove --dry-run created the missing profile directory"
    );
}
#[test]
fn doctor_command_is_registered() {
    let temp = TestDir::new();
    let profile_agent = temp.0.join("home/.omp/profiles/demo/agent");
    let mut command = temp.cli_command(Path::new(env!("CARGO_MANIFEST_DIR")));
    command.args(["doctor", "--json"]);
    configure_doctor_omp(&temp, &mut command, &profile_agent, None, false);

    let output = command.output().expect("failed to execute omp-topology");
    assert!(output.status.success(), "doctor --json failed: {output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("doctor --json did not emit one JSON document");
    assert!(
        report.is_object(),
        "doctor report was not a JSON object: {report}"
    );
}

#[test]
fn completion_command_is_registered() {
    let temp = TestDir::new();
    let output = temp
        .cli_command(Path::new(env!("CARGO_MANIFEST_DIR")))
        .args(["completions", "bash"])
        .output()
        .expect("failed to execute omp-topology");

    assert!(
        output.status.success(),
        "completions bash failed: {output:?}"
    );
    assert!(
        !output.stdout.is_empty(),
        "completions bash emitted an empty script"
    );
}

#[cfg(unix)]
#[test]
fn hidden_completion_preserves_non_utf8_agent_dir_candidate_bytes() {
    let temp = TestDir::new();
    let candidate = temp.0.join(OsString::from_vec(b"candidate-\xff".to_vec()));
    fs::create_dir(&candidate).expect("failed to create non-UTF-8 agent directory");
    let prefix = temp.0.join("candidate-");

    let output = Command::new(env!("CARGO_BIN_EXE_omp-topology"))
        .args(["__complete", "--", "omp-topology", "doctor", "--agent-dir"])
        .arg(&prefix)
        .output()
        .expect("failed to execute omp-topology __complete");

    assert!(
        output.status.success(),
        "__complete failed before producing candidates: {output:?}"
    );
    assert!(
        !output.stdout.is_empty(),
        "__complete produced no candidates: {output:?}"
    );
    let expected = candidate.as_os_str().as_bytes();
    assert!(
        output
            .stdout
            .split(|byte| *byte == b'\0')
            .any(|record| { record.strip_suffix(b"/").unwrap_or(record) == expected }),
        "expected exact candidate bytes {expected:?} in NUL-delimited output, got {:?}",
        output.stdout
    );
}

#[cfg(unix)]
#[test]
fn hidden_completion_preserves_newline_agent_dir_candidate_bytes() {
    let temp = TestDir::new();
    let candidate = temp.0.join("candidate-\nname");
    fs::create_dir(&candidate).expect("failed to create newline agent directory");
    let prefix = temp.0.join("candidate-");

    let output = Command::new(env!("CARGO_BIN_EXE_omp-topology"))
        .args(["__complete", "--", "omp-topology", "doctor", "--agent-dir"])
        .arg(&prefix)
        .output()
        .expect("failed to execute omp-topology __complete");

    assert!(
        output.status.success(),
        "__complete failed before producing candidates: {output:?}"
    );
    assert!(
        !output.stdout.is_empty(),
        "__complete produced no candidates: {output:?}"
    );
    let expected = candidate.as_os_str().as_bytes();
    assert!(
        output
            .stdout
            .split(|byte| *byte == b'\0')
            .any(|record| { record.strip_suffix(b"/").unwrap_or(record) == expected }),
        "expected exact candidate bytes {expected:?} in NUL-delimited output, got {:?}",
        output.stdout
    );
}

fn write_cli_fixture_topology(source_root: &Path) {
    let topology = source_root.join("topologies/cli-contract-fixture");
    let agent_root = topology.join("agent");
    fs::create_dir_all(agent_root.join("skills")).expect("failed to create fixture topology");
    fs::write(
        topology.join("topology.yml"),
        "name: cli-contract-fixture\nversion: 1.0.0\ndescription: test fixture\ninstallable: true\n",
    )
    .expect("failed to write fixture metadata");
    fs::write(agent_root.join("config.yml"), "task:\n  eager: true\n")
        .expect("failed to write fixture config");
    fs::write(agent_root.join("skills/fixture.md"), b"fixture skill\n")
        .expect("failed to write fixture skill");
}

fn install_cli_fixture(
    temp: &TestDir,
    source_root: &Path,
    install_id: &str,
    target: &Path,
    profile: Option<&str>,
) {
    let source = source_root.to_string_lossy().into_owned();
    let target_arg = target.to_string_lossy().into_owned();
    let mut args = vec![
        "install".to_string(),
        "--topology".to_string(),
        "cli-contract-fixture".to_string(),
        "--source".to_string(),
        source,
        "--install-id".to_string(),
        install_id.to_string(),
    ];
    if let Some(profile) = profile {
        args.extend(["--profile".to_string(), profile.to_string()]);
    } else {
        args.extend(["--agent-dir".to_string(), target_arg]);
    }

    let mut command = temp.cli_command(Path::new(env!("CARGO_MANIFEST_DIR")));
    command.args(&args);
    temp.configure_fake_omp(&mut command, target, true);
    let output = command.output().expect("failed to execute fixture install");
    assert!(
        output.status.success(),
        "fixture install failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn configure_doctor_omp(
    temp: &TestDir,
    command: &mut Command,
    agent_dir: &Path,
    expected_profile: Option<&str>,
    create_dir: bool,
) {
    let bin_dir = temp.0.join("doctor-bin");
    fs::create_dir_all(&bin_dir).expect("failed to create isolated doctor PATH");
    let fake_omp = bin_dir.join("omp");
    fs::write(
        &fake_omp,
        "#!/bin/sh\n\
         printf '%s\\n' \"$*\" >> \"$OMP_TEST_OMP_LOG\"\n\
         if [ \"$1\" = \"--version\" ] || [ \"$1\" = \"--help\" ]; then printf 'omp test\\n'; exit 0; fi\n\
         if [ \"$1\" = \"--profile\" ]; then\n\
           if [ -n \"$OMP_TEST_EXPECTED_PROFILE\" ] && [ \"$2\" != \"$OMP_TEST_EXPECTED_PROFILE\" ]; then exit 72; fi\n\
           shift 2\n\
         fi\n\
         if [ \"$1\" != \"config\" ] || [ \"$2\" != \"path\" ]; then exit 64; fi\n\
         if [ \"$OMP_TEST_CREATE_DIR\" = 1 ]; then mkdir -p \"$OMP_TEST_AGENT_DIR\" || exit 1; fi\n\
         printf '%s\\n' \"$OMP_TEST_AGENT_DIR\"\n",
    )
    .expect("failed to write fake doctor OMP");
    fs::set_permissions(&fake_omp, fs::Permissions::from_mode(0o755))
        .expect("failed to make fake doctor OMP executable");

    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(bin_dir).chain(std::env::split_paths(&inherited_path)),
    )
    .expect("failed to build isolated doctor PATH");
    command
        .env("PATH", path)
        .env("OMP_TEST_AGENT_DIR", agent_dir)
        .env(
            "OMP_TEST_EXPECTED_PROFILE",
            expected_profile.unwrap_or_default(),
        )
        .env("OMP_TEST_CREATE_DIR", if create_dir { "1" } else { "0" })
        .env(
            "OMP_TEST_OMP_LOG",
            temp.0.join("doctor-omp-invocations.log"),
        );
}

fn run_doctor(temp: &TestDir, args: &[&str]) -> Output {
    let profile_agent = temp.0.join("home/.omp/profiles/demo/agent");
    let mut command = temp.cli_command(Path::new(env!("CARGO_MANIFEST_DIR")));
    command.args(args);
    configure_doctor_omp(temp, &mut command, &profile_agent, Some("demo"), false);
    command.output().expect("failed to execute doctor")
}

fn doctor_json(output: &Output, expected_code: i32) -> serde_json::Value {
    assert_eq!(
        output.status.code(),
        Some(expected_code),
        "unexpected doctor exit status:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("doctor --json stdout was not exactly one JSON document");
    assert!(
        report.is_object(),
        "doctor JSON report was not an object: {report}"
    );
    report
}

fn cli_tree_snapshot(root: &Path) -> CliTreeSnapshot {
    fn visit(root: &Path, directory: &Path, entries: &mut Vec<CliTreeSnapshotEntry>) {
        let mut children = fs::read_dir(directory)
            .expect("failed to read snapshot directory")
            .map(|entry| entry.expect("failed to read snapshot entry").path())
            .collect::<Vec<_>>();
        children.sort();
        for path in children {
            let metadata = fs::symlink_metadata(&path).expect("failed to inspect snapshot entry");
            let relative = path
                .strip_prefix(root)
                .expect("snapshot entry escaped its root")
                .to_path_buf();
            let mode = metadata.permissions().mode();
            if metadata.file_type().is_symlink() {
                let target = fs::read_link(&path)
                    .expect("failed to read snapshot symlink")
                    .to_string_lossy()
                    .as_bytes()
                    .to_vec();
                entries.push((relative, 2, mode, target));
            } else if metadata.is_dir() {
                entries.push((relative, 1, mode, Vec::new()));
                visit(root, &path, entries);
            } else if metadata.is_file() {
                entries.push((
                    relative,
                    3,
                    mode,
                    fs::read(&path).expect("failed to read snapshot file"),
                ));
            } else {
                entries.push((relative, 4, mode, Vec::new()));
            }
        }
    }

    if fs::symlink_metadata(root).is_err() {
        return None;
    }
    let mut entries = Vec::new();
    if root.is_dir() {
        visit(root, root, &mut entries);
    } else {
        entries.push((
            PathBuf::new(),
            3,
            fs::symlink_metadata(root)
                .expect("failed to inspect snapshot root")
                .permissions()
                .mode(),
            fs::read(root).expect("failed to read snapshot root"),
        ));
    }
    Some(entries)
}

fn snapshot_roots(roots: &[PathBuf]) -> Vec<CliTreeSnapshot> {
    roots.iter().map(|root| cli_tree_snapshot(root)).collect()
}

fn cli_manifest_path(temp: &TestDir, agent_dir: &Path) -> PathBuf {
    let resolved = fs::canonicalize(agent_dir).expect("failed to resolve fixture agent dir");
    let mut encoded = String::new();
    for byte in resolved.to_string_lossy().bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    temp.0
        .join("state/omp-topology")
        .join(encoded)
        .join("manifest.json")
}

fn doctor_fixture(temp: &TestDir) -> (PathBuf, PathBuf, PathBuf) {
    let source_root = temp.0.join("source");
    let profile_agent = temp.0.join("home/.omp/profiles/demo/agent");
    let explicit_agent = temp.0.join("targets/explicit-agent");
    write_cli_fixture_topology(&source_root);
    install_cli_fixture(
        temp,
        &source_root,
        "doctor-profile",
        &profile_agent,
        Some("demo"),
    );
    install_cli_fixture(temp, &source_root, "doctor-explicit", &explicit_agent, None);
    (source_root, profile_agent, explicit_agent)
}

#[test]
fn doctor_json_scopes_are_read_only_and_human_output_is_not_json() {
    let temp = TestDir::new();
    let (source_root, profile_agent, explicit_agent) = doctor_fixture(&temp);
    let roots = [
        temp.0.join("home"),
        temp.0.join("xdg-config"),
        temp.0.join("state"),
        source_root,
        profile_agent,
        explicit_agent.clone(),
    ];
    let before = snapshot_roots(&roots);

    let all = run_doctor(&temp, &["doctor", "--json"]);
    let all_doc = doctor_json(&all, 0);
    let all_text = all_doc.to_string();
    assert!(
        all_text.contains("doctor-profile"),
        "default scope omitted a managed install: {all_doc}"
    );
    assert!(
        all_text.contains("doctor-explicit"),
        "default scope omitted a managed install: {all_doc}"
    );

    let by_id = doctor_json(
        &run_doctor(
            &temp,
            &["doctor", "--install-id", "doctor-profile", "--json"],
        ),
        0,
    )
    .to_string();
    assert!(by_id.contains("doctor-profile"));
    assert!(!by_id.contains("doctor-explicit"));

    let by_profile = doctor_json(
        &run_doctor(&temp, &["doctor", "--profile", "demo", "--json"]),
        0,
    )
    .to_string();
    assert!(by_profile.contains("doctor-profile"));
    assert!(!by_profile.contains("doctor-explicit"));

    let agent_arg = explicit_agent.to_string_lossy().into_owned();
    let by_agent_dir = doctor_json(
        &run_doctor(&temp, &["doctor", "--agent-dir", &agent_arg, "--json"]),
        0,
    )
    .to_string();
    assert!(by_agent_dir.contains("doctor-explicit"));
    assert!(!by_agent_dir.contains("doctor-profile"));

    let human = run_doctor(&temp, &["doctor", "--install-id", "doctor-profile"]);
    assert_eq!(
        human.status.code(),
        Some(0),
        "human doctor output failed: {human:?}"
    );
    assert!(
        serde_json::from_slice::<serde_json::Value>(&human.stdout).is_err(),
        "human doctor output was JSON: {}",
        String::from_utf8_lossy(&human.stdout)
    );
    assert_eq!(
        snapshot_roots(&roots),
        before,
        "doctor changed managed state, target files, profiles, or user config"
    );
}

#[test]
fn doctor_selector_conflicts_are_usage_errors_before_state_or_omp_access() {
    let conflicts: &[&[&str]] = &[
        &["doctor", "--install-id", "fixture", "--profile", "demo"],
        &[
            "doctor",
            "--install-id",
            "fixture",
            "--agent-dir",
            "/tmp/target",
        ],
        &["doctor", "--profile", "demo", "--agent-dir", "/tmp/target"],
        &[
            "doctor",
            "--install-id",
            "fixture",
            "--profile",
            "demo",
            "--agent-dir",
            "/tmp/target",
        ],
    ];
    for args in conflicts {
        let temp = TestDir::new();
        let blocked_state = temp.0.join("blocked-state");
        let sentinel = b"selector conflict must not access state\n";
        fs::write(&blocked_state, sentinel).expect("failed to create blocked state sentinel");
        let profile_agent = temp.0.join("home/.omp/profiles/demo/agent");
        let mut command = temp.cli_command(Path::new(env!("CARGO_MANIFEST_DIR")));
        command
            .args(args.iter().copied())
            .env("XDG_STATE_HOME", &blocked_state);
        configure_doctor_omp(&temp, &mut command, &profile_agent, Some("demo"), false);
        let output = command
            .output()
            .expect("failed to execute conflicting doctor");

        assert_eq!(
            output.status.code(),
            Some(2),
            "expected Clap usage failure: {output:?}"
        );
        assert!(
            output.stdout.is_empty(),
            "usage error wrote stdout: {output:?}"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("Usage:"), "missing usage text: {stderr}");
        assert!(
            !stderr.contains("unrecognized subcommand"),
            "doctor command was not registered: {stderr}"
        );
        let conflicting_selectors = ["--install-id", "--profile", "--agent-dir"]
            .iter()
            .filter(|selector| stderr.contains(*selector))
            .count();
        assert!(
            conflicting_selectors >= 2,
            "usage error did not identify conflicting selectors: {stderr}"
        );
        assert_eq!(
            fs::read(&blocked_state).expect("state sentinel disappeared"),
            sentinel,
            "usage error accessed or changed state"
        );
        assert!(
            !temp.0.join("doctor-omp-invocations.log").exists(),
            "usage error invoked OMP before rejecting the selector combination"
        );
    }
}

#[test]
fn doctor_unknown_and_unregistered_data_are_distinguished_without_adoption() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let orphan_agent = temp.0.join("targets/orphan-agent");
    let unrelated = temp.0.join("unrelated");
    write_cli_fixture_topology(&source_root);
    install_cli_fixture(&temp, &source_root, "orphan-id", &orphan_agent, None);
    fs::create_dir_all(&unrelated).expect("failed to create unrelated canary");
    fs::write(
        unrelated.join("sentinel.txt"),
        b"outside the selected scope\n",
    )
    .expect("failed to write unrelated canary");

    let registry_path = temp.0.join("state/omp-topology/registry.json");
    let registry_bytes = fs::read(&registry_path).expect("fixture registry is missing");
    let mut registry: serde_json::Value =
        serde_json::from_slice(&registry_bytes).expect("fixture registry is invalid JSON");
    registry["aliases"]
        .as_object_mut()
        .expect("fixture registry aliases are missing")
        .remove("orphan-id")
        .expect("fixture install ID is missing from registry");
    let orphaned_registry =
        serde_json::to_vec_pretty(&registry).expect("failed to encode orphaned registry");
    fs::write(&registry_path, &orphaned_registry).expect("failed to orphan fixture registry entry");
    let manifest = cli_manifest_path(&temp, &orphan_agent);
    let manifest_bytes = fs::read(&manifest).expect("orphaned ownership manifest is missing");
    let roots = [
        temp.0.join("state"),
        orphan_agent.clone(),
        unrelated.clone(),
    ];
    let before = snapshot_roots(&roots);

    let unknown = run_doctor(&temp, &["doctor", "--install-id", "orphan-id", "--json"]);
    let unknown_doc = doctor_json(&unknown, 1);
    assert!(
        unknown_doc
            .to_string()
            .to_ascii_lowercase()
            .contains("orphan-id"),
        "unknown install ID was not identified: {unknown_doc}"
    );

    let agent_arg = orphan_agent.to_string_lossy().into_owned();
    let unregistered = run_doctor(&temp, &["doctor", "--agent-dir", &agent_arg, "--json"]);
    let unregistered_doc = doctor_json(&unregistered, 0);
    let unregistered_text = unregistered_doc.to_string().to_ascii_lowercase();
    assert!(
        unregistered_text.contains("unregistered"),
        "orphaned data was not labeled unregistered: {unregistered_doc}"
    );
    assert!(
        unregistered_text.contains("warning"),
        "orphaned data was not distinguished as a warning: {unregistered_doc}"
    );
    assert!(
        !unregistered_text.contains(&unrelated.to_string_lossy().to_ascii_lowercase()),
        "doctor reported data outside the selected target: {unregistered_doc}"
    );
    assert_eq!(
        fs::read(&manifest).expect("doctor removed the orphaned manifest"),
        manifest_bytes,
        "doctor adopted or changed the orphaned ownership manifest"
    );
    assert_eq!(
        fs::read(&registry_path).expect("doctor changed the registry"),
        orphaned_registry,
        "doctor adopted or changed the registry"
    );
    assert_eq!(
        snapshot_roots(&roots),
        before,
        "doctor changed orphaned or unrelated data"
    );
}

#[test]
fn doctor_reports_stale_per_alias_manifest_with_registered_sibling() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("home/.omp/profiles/demo/agent");
    write_cli_fixture_topology(&source_root);
    install_cli_fixture(
        &temp,
        &source_root,
        "registered-id",
        &agent_dir,
        Some("demo"),
    );

    let stale_topology = "cli-stale-fixture";
    let stale_topology_root = source_root.join("topologies").join(stale_topology);
    let stale_agent_root = stale_topology_root.join("agent");
    fs::create_dir_all(stale_agent_root.join("skills"))
        .expect("failed to create stale fixture topology");
    fs::write(
        stale_topology_root.join("topology.yml"),
        "name: cli-stale-fixture\nversion: 1.0.0\ndescription: stale fixture\ninstallable: true\n",
    )
    .expect("failed to write stale fixture metadata");
    fs::write(stale_agent_root.join("config.yml"), "stale_fixture: true\n")
        .expect("failed to write stale fixture config");
    fs::write(
        stale_agent_root.join("skills/stale.md"),
        b"stale fixture skill\n",
    )
    .expect("failed to write stale fixture skill");

    let source_arg = source_root.to_string_lossy().into_owned();
    let mut install = temp.cli_command(Path::new(env!("CARGO_MANIFEST_DIR")));
    install.args([
        "install",
        "--topology",
        stale_topology,
        "--source",
        &source_arg,
        "--install-id",
        "stale-id",
        "--profile",
        "demo",
    ]);
    temp.configure_fake_omp(&mut install, &agent_dir, true);
    let installed = install
        .output()
        .expect("failed to execute stale fixture install");
    assert!(
        installed.status.success(),
        "stale fixture install failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&installed.stdout),
        String::from_utf8_lossy(&installed.stderr)
    );

    let state_root = temp.0.join("state");
    let registry_path = state_root.join("omp-topology/registry.json");
    let registry_bytes = fs::read(&registry_path).expect("fixture registry is missing");
    let mut registry: serde_json::Value =
        serde_json::from_slice(&registry_bytes).expect("fixture registry is invalid JSON");
    assert_eq!(
        registry["aliases"]["registered-id"]["agent_dir"],
        fs::canonicalize(&agent_dir)
            .expect("fixture agent dir is missing")
            .display()
            .to_string()
    );
    assert_eq!(
        registry["aliases"]["stale-id"]["agent_dir"],
        registry["aliases"]["registered-id"]["agent_dir"],
        "fixture installs did not share their agent directory"
    );
    assert_eq!(
        registry["aliases"]["stale-id"]["profile"], "demo",
        "stale fixture install did not use the shared profile"
    );

    let legacy_manifest = cli_manifest_path(&temp, &agent_dir);
    let stale_manifest = legacy_manifest
        .parent()
        .expect("fixture manifest has no parent")
        .join("manifests/stale-id.json");
    let stale_manifest_bytes =
        fs::read(&stale_manifest).expect("stale per-install manifest is missing");
    registry["aliases"]
        .as_object_mut()
        .expect("fixture registry aliases are missing")
        .remove("stale-id")
        .expect("stale fixture install ID is missing from registry");
    fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&registry).expect("failed to encode stale fixture registry"),
    )
    .expect("failed to remove stale fixture ID from registry");
    let orphaned_registry_bytes =
        fs::read(&registry_path).expect("orphaned fixture registry is missing");
    let before = snapshot_roots(&[state_root, agent_dir.clone()]);

    let agent_arg = agent_dir.to_string_lossy().into_owned();
    let outputs = [
        run_doctor(&temp, &["doctor", "--json"]),
        run_doctor(&temp, &["doctor", "--profile", "demo", "--json"]),
        run_doctor(&temp, &["doctor", "--agent-dir", &agent_arg, "--json"]),
    ];
    for (scan, output) in ["default", "profile", "agent-dir"].into_iter().zip(outputs) {
        let report = doctor_json(&output, 0);
        assert_eq!(
            report["status"], "warning",
            "{scan} scan did not report only a warning: {report}"
        );
        assert_eq!(
            report["omp"]["status"], "available",
            "{scan} scan did not see healthy OMP: {report}"
        );
        assert!(
            report["installs"]
                .as_array()
                .is_some_and(|installs| installs
                    .iter()
                    .any(|install| install["install_id"] == "registered-id"
                        && install["status"] == "ok")),
            "{scan} scan omitted the healthy registered sibling: {report}"
        );
        let findings = report["findings"]
            .as_array()
            .expect("doctor findings are not an array");
        assert!(
            !findings.is_empty()
                && findings
                    .iter()
                    .all(|finding| finding["severity"] == "warning"),
            "{scan} scan reported an error or omitted its warning finding: {report}"
        );
        let warning_text = serde_json::to_string(findings)
            .expect("failed to encode doctor warning findings")
            .to_ascii_lowercase();
        assert!(
            warning_text.contains("stale-id") && warning_text.contains(stale_topology),
            "{scan} scan did not identify the stale install ID and topology in a warning: {report}"
        );
        assert_eq!(
            snapshot_roots(&[temp.0.join("state"), agent_dir.clone()]),
            before,
            "{scan} scan changed registry, manifests, or target files"
        );
        assert_eq!(
            fs::read(&registry_path).expect("doctor changed the registry"),
            orphaned_registry_bytes,
            "{scan} scan changed registry bytes"
        );
        assert_eq!(
            fs::read(&stale_manifest).expect("doctor removed the stale manifest"),
            stale_manifest_bytes,
            "{scan} scan changed stale manifest bytes"
        );
    }
}

#[test]
fn doctor_default_reports_stale_manifest_in_registered_explicit_agent_dir() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("targets/explicit-agent");
    write_cli_fixture_topology(&source_root);
    install_cli_fixture(
        &temp,
        &source_root,
        "registered-explicit-id",
        &agent_dir,
        None,
    );
    let stale_topology = "cli-stale-explicit-fixture";
    let stale_topology_root = source_root.join("topologies").join(stale_topology);
    let stale_agent_root = stale_topology_root.join("agent");
    fs::create_dir_all(stale_agent_root.join("skills"))
        .expect("failed to create stale fixture topology");
    fs::write(
        stale_topology_root.join("topology.yml"),
        "name: cli-stale-explicit-fixture\nversion: 1.0.0\ndescription: stale fixture\ninstallable: true\n",
    )
    .expect("failed to write stale fixture metadata");
    fs::write(stale_agent_root.join("config.yml"), "stale_fixture: true\n")
        .expect("failed to write stale fixture config");
    fs::write(
        stale_agent_root.join("skills/stale-explicit.md"),
        b"stale fixture skill\n",
    )
    .expect("failed to write stale fixture skill");
    let source_arg = source_root.to_string_lossy().into_owned();
    let target_arg = agent_dir.to_string_lossy().into_owned();
    let mut install = temp.cli_command(Path::new(env!("CARGO_MANIFEST_DIR")));
    install.args([
        "install",
        "--topology",
        stale_topology,
        "--source",
        &source_arg,
        "--install-id",
        "stale-explicit-id",
        "--agent-dir",
        &target_arg,
    ]);
    temp.configure_fake_omp(&mut install, &agent_dir, true);
    let installed = install
        .output()
        .expect("failed to execute stale fixture install");
    assert!(
        installed.status.success(),
        "stale fixture install failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&installed.stdout),
        String::from_utf8_lossy(&installed.stderr)
    );

    let registry_path = temp.0.join("state/omp-topology/registry.json");
    let registry_bytes = fs::read(&registry_path).expect("fixture registry is missing");
    let mut registry: serde_json::Value =
        serde_json::from_slice(&registry_bytes).expect("fixture registry is invalid JSON");
    assert_eq!(
        registry["aliases"]["registered-explicit-id"]["agent_dir"],
        fs::canonicalize(&agent_dir)
            .expect("fixture agent dir is missing")
            .display()
            .to_string()
    );
    assert_eq!(
        registry["aliases"]["stale-explicit-id"]["agent_dir"],
        registry["aliases"]["registered-explicit-id"]["agent_dir"],
        "fixture installs did not share the explicit agent directory"
    );

    let manifest_root = cli_manifest_path(&temp, &agent_dir)
        .parent()
        .expect("fixture manifest has no parent")
        .to_path_buf();
    let stale_manifest = manifest_root.join("manifests/stale-explicit-id.json");
    assert!(
        stale_manifest.is_file(),
        "stale fixture manifest was not created"
    );
    registry["aliases"]
        .as_object_mut()
        .expect("fixture registry aliases are missing")
        .remove("stale-explicit-id")
        .expect("stale fixture install ID is missing from registry");
    fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&registry).expect("failed to encode stale fixture registry"),
    )
    .expect("failed to remove stale fixture ID from registry");

    let output = run_doctor(&temp, &["doctor", "--json"]);
    let report = doctor_json(&output, 0);
    let findings = report["findings"]
        .as_array()
        .expect("doctor findings are not an array");
    let warning_text = serde_json::to_string(findings)
        .expect("failed to encode doctor findings")
        .to_ascii_lowercase();
    assert!(
        report["status"] == "warning"
            && warning_text.contains("stale-explicit-id")
            && warning_text.contains("unregistered"),
        "default scan omitted the stale ownership manifest from its registered explicit agent directory: {report}"
    );
    assert!(
        report["installs"]
            .as_array()
            .is_some_and(|installs| installs.iter().any(|install| {
                install["install_id"] == "registered-explicit-id" && install["status"] == "ok"
            })),
        "default scan did not retain the healthy registered install: {report}"
    );
}

#[test]
fn doctor_registry_and_manifest_errors_are_json_and_read_only() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("targets/broken-agent");
    write_cli_fixture_topology(&source_root);
    install_cli_fixture(&temp, &source_root, "broken-id", &agent_dir, None);
    let registry_path = temp.0.join("state/omp-topology/registry.json");
    let valid_registry = fs::read(&registry_path).expect("fixture registry is missing");
    let manifest = cli_manifest_path(&temp, &agent_dir);

    fs::write(&registry_path, b"{ malformed registry\n").expect("failed to corrupt registry");
    let registry_before = snapshot_roots(&[temp.0.join("state"), agent_dir.clone()]);
    let registry_error = run_doctor(&temp, &["doctor", "--json"]);
    let registry_doc = doctor_json(&registry_error, 1);
    assert!(
        registry_doc
            .to_string()
            .to_ascii_lowercase()
            .contains("registry"),
        "registry failure was not diagnosed: {registry_doc}"
    );
    assert_eq!(
        snapshot_roots(&[temp.0.join("state"), agent_dir.clone()]),
        registry_before,
        "doctor changed malformed registry state"
    );

    fs::write(&registry_path, valid_registry).expect("failed to restore registry");
    fs::write(&manifest, b"{ malformed manifest\n").expect("failed to corrupt manifest");
    let manifest_before = snapshot_roots(&[temp.0.join("state"), agent_dir.clone()]);
    let manifest_error = run_doctor(&temp, &["doctor", "--install-id", "broken-id", "--json"]);
    let manifest_doc = doctor_json(&manifest_error, 1);
    assert!(
        manifest_doc
            .to_string()
            .to_ascii_lowercase()
            .contains("manifest"),
        "ownership manifest failure was not diagnosed: {manifest_doc}"
    );
    assert_eq!(
        snapshot_roots(&[temp.0.join("state"), agent_dir]),
        manifest_before,
        "doctor changed malformed ownership state"
    );
}

#[test]
fn doctor_reports_missing_omp_without_creating_state() {
    let temp = TestDir::new();
    let empty_path = temp.0.join("empty-path");
    fs::create_dir_all(&empty_path).expect("failed to create empty PATH");
    let mut command = temp.cli_command(Path::new(env!("CARGO_MANIFEST_DIR")));
    command.args(["doctor", "--json"]).env("PATH", &empty_path);
    let output = command
        .output()
        .expect("failed to execute doctor without OMP");
    let report = doctor_json(&output, 1);
    assert!(
        report.to_string().to_ascii_lowercase().contains("omp"),
        "missing OMP was not diagnosed: {report}"
    );
    assert!(
        !temp.0.join("state").exists(),
        "read-only doctor created state while reporting missing OMP"
    );
}

#[test]
fn doctor_does_not_evaluate_shell_config_or_refresh_remote_sources() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let profile_agent = temp.0.join("home/.omp/profiles/demo/agent");
    write_cli_fixture_topology(&source_root);
    install_cli_fixture(
        &temp,
        &source_root,
        "remote-fixture",
        &profile_agent,
        Some("demo"),
    );

    let registry_path = temp.0.join("state/omp-topology/registry.json");
    let mut registry: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("fixture registry is missing"))
            .expect("fixture registry is invalid JSON");
    registry["aliases"]["remote-fixture"]["source"] = serde_json::json!({
        "kind": "github",
        "owner": "test-owner",
        "repo": "test-repo"
    });
    registry["aliases"]["remote-fixture"]["resolved_commit"] =
        serde_json::Value::String("0123456789abcdef0123456789abcdef01234567".to_string());
    fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&registry).expect("failed to encode remote-source registry"),
    )
    .expect("failed to seed remote-source registry");

    let home = temp.0.join("home");
    let xdg_config = temp.0.join("xdg-config");
    let marker = temp.0.join("shell-config-was-evaluated");
    let secret = "doctor-private-config-sentinel";
    for config in [
        home.join(".bashrc"),
        home.join(".zshrc"),
        xdg_config.join("fish/config.fish"),
    ] {
        fs::create_dir_all(config.parent().expect("shell config has no parent"))
            .expect("failed to create shell config directory");
        fs::write(
            config,
            format!(
                "printf 'executed\\n' > '{}'\n# {secret}\n",
                marker.display()
            ),
        )
        .expect("failed to write shell config sentinel");
    }

    let mut command = temp.cli_command(Path::new(env!("CARGO_MANIFEST_DIR")));
    command.args(["doctor", "--install-id", "remote-fixture", "--json"]);
    configure_doctor_omp(&temp, &mut command, &profile_agent, Some("demo"), false);
    let bin_dir = temp.0.join("doctor-bin");
    let external_log = temp.0.join("external-command-invocations.log");
    for (name, label) in [("git", "git"), ("curl", "curl")] {
        let fake = bin_dir.join(name);
        fs::write(
            &fake,
            format!(
                "#!/bin/sh\nprintf '{label} %s\\n' \"$*\" >> \"$OMP_TEST_EXTERNAL_LOG\"\nexit 91\n"
            ),
        )
        .expect("failed to write external-command tripwire");
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755))
            .expect("failed to make external-command tripwire executable");
    }
    command.env("OMP_TEST_EXTERNAL_LOG", &external_log);

    let roots = [
        home,
        xdg_config,
        temp.0.join("state"),
        profile_agent,
        source_root,
    ];
    let before = snapshot_roots(&roots);
    let output = command.output().expect("failed to execute doctor");
    let report = doctor_json(&output, 0);
    let output_text =
        format!("{}{}", report, String::from_utf8_lossy(&output.stderr)).to_ascii_lowercase();
    assert!(
        !output_text.contains(&secret.to_ascii_lowercase()),
        "doctor leaked a shell-config sentinel"
    );
    for forbidden_claim in [
        "alias healthy",
        "alias is healthy",
        "alias status: healthy",
        "alias status: ok",
        "shell alias is configured",
        "shell alias is available",
    ] {
        assert!(
            !output_text.contains(forbidden_claim),
            "doctor claimed shell alias health: {forbidden_claim}"
        );
    }
    assert!(!marker.exists(), "doctor evaluated shell configuration");
    assert!(
        !external_log.exists(),
        "doctor ran git or curl for source freshness"
    );
    assert_eq!(
        snapshot_roots(&roots),
        before,
        "doctor changed config, profile, target, registry, manifest, or source cache"
    );
}

fn completion_script(temp: &TestDir, shell: &str, script_path: &Path) {
    let mut command = temp.cli_command(Path::new(env!("CARGO_MANIFEST_DIR")));
    command.args(["completions", shell]);
    let output = command
        .output()
        .expect("failed to execute completion command");
    assert!(
        output.status.success(),
        "completions {shell} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.stdout.is_empty(),
        "completions {shell} emitted no script"
    );
    assert!(
        output.stderr.is_empty(),
        "completions {shell} wrote non-script output to stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::write(script_path, output.stdout).expect("failed to save completion script in tempdir");
}

fn completion_runtime_path(temp: &TestDir) -> std::ffi::OsString {
    let binary = Path::new(env!("CARGO_BIN_EXE_omp-topology"));
    let mut entries = vec![
        binary
            .parent()
            .expect("CLI binary has no parent")
            .to_path_buf(),
        temp.0.join("bin"),
        temp.0.join("doctor-bin"),
    ];
    if let Some(path) = std::env::var_os("PATH") {
        entries.extend(std::env::split_paths(&path));
    }
    std::env::join_paths(entries).expect("failed to build completion runtime PATH")
}

fn shell_command(temp: &TestDir, shell: &str, script: &Path, line: &str) -> Command {
    let mut command = Command::new(shell);
    command
        .env_remove("OMP_HOME")
        .env_remove("OMP_PROFILE")
        .env_remove("OMP_TEST_AGENT_DIR")
        .env_remove("OMP_TEST_CREATE_DIR")
        .env_remove("OMP_TEST_OMP_LOG")
        .env_remove("OMP_TEST_EXPECTED_PROFILE")
        .env_remove("OMP_TEST_EXTERNAL_LOG")
        .env("HOME", temp.0.join("home"))
        .env("XDG_CONFIG_HOME", temp.0.join("xdg-config"))
        .env("XDG_STATE_HOME", temp.0.join("state"))
        .env("PATH", completion_runtime_path(temp))
        .env("OMP_COMPLETION_SCRIPT", script)
        .env("OMP_COMPLETION_LINE", line)
        .env(
            "OMP_COMPLETION_DIR",
            script.parent().unwrap_or(Path::new(".")),
        )
        .env("ZDOTDIR", temp.0.join("zsh-home"));
    command
}

fn zsh_completion_candidates(temp: &TestDir, script: &Path, line: &str) -> String {
    let zsh_dir = temp.0.join("zsh-fpath");
    fs::create_dir_all(temp.0.join("zsh-home")).expect("failed to create isolated Zsh home");
    fs::create_dir_all(&zsh_dir).expect("failed to create temporary Zsh fpath");
    fs::copy(script, zsh_dir.join("_omp-topology"))
        .expect("failed to stage Zsh completion in temporary fpath");
    let mut command = shell_command(temp, "zsh", script, line);
    command
        .args([
            "-f",
            "-c",
            r#"
zmodload zsh/zpty || exit 81
zpty -b omp_completion zsh -f -i || exit 82
zpty -w omp_completion $'PROMPT=\nRPROMPT=\nfpath=("$OMP_COMPLETION_DIR" $fpath)\nautoload -Uz compinit\ncompinit -i -d "$ZDOTDIR/.zcompdump"\nsetopt AUTO_LIST LIST_AMBIGUOUS\nfunction _omp_completion_done { print -r -- __OMP_CANDIDATES_DONE__; zle send-break }\nzle -N _omp_completion_done\nbindkey "^X^X" _omp_completion_done\nprint -r -- __OMP_READY__\n'
zpty -r omp_completion ready '*__OMP_READY__*' || exit 83
zpty -w -n omp_completion "$OMP_COMPLETION_LINE"$'\t\t'
zpty -w -n omp_completion $'\030\030'
zpty -r omp_completion candidate_output '*__OMP_CANDIDATES_DONE__*' || exit 84
print -r -- "$candidate_output"
zpty -d omp_completion
"#,
        ])
        .env("OMP_COMPLETION_DIR", &zsh_dir);
    let output = command
        .output()
        .expect("failed to run Zsh completion probe");
    assert!(
        output.status.success(),
        "Zsh completion probe failed for {line:?}:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}
fn zsh_completion_inserted_words(temp: &TestDir, script: &Path, line: &str) -> String {
    let zsh_dir = temp.0.join("zsh-insert-fpath");
    fs::create_dir_all(temp.0.join("zsh-home")).expect("failed to create isolated Zsh home");
    fs::create_dir_all(&zsh_dir).expect("failed to create temporary Zsh fpath");
    fs::copy(script, zsh_dir.join("_omp-topology"))
        .expect("failed to stage Zsh completion in temporary fpath");
    let mut command = shell_command(temp, "zsh", script, line);
    command
        .args([
            "-f",
            "-c",
            r#"
zmodload zsh/zpty || exit 81
zpty -b omp_completion zsh -f -i || exit 82
zpty -w omp_completion $'PROMPT=\nRPROMPT=\nfpath=("$OMP_COMPLETION_DIR" $fpath)\nautoload -Uz compinit\ncompinit -i -d "$ZDOTDIR/.zcompdump"\nfunction _omp_completion_inspect { local -a parsed; parsed=("${(z)BUFFER}"); print -r -- "__OMP_WORD_COUNT__${#parsed[@]}"; print -r -- "__OMP_LAST_WORD__${parsed[-1]}"; print -r -- "__OMP_PREVIOUS_WORD__${parsed[-2]}"; zle send-break }\nzle -N _omp_completion_inspect\nbindkey "^X^X" _omp_completion_inspect\nprint -r -- __OMP_READY__\n'
zpty -r omp_completion ready '*__OMP_READY__*' || exit 83
zpty -w -n omp_completion "$OMP_COMPLETION_LINE"$'\t'
zpty -w -n omp_completion $'\030\030'
zpty -r omp_completion inserted_output '*__OMP_PREVIOUS_WORD__*' || exit 84
print -r -- "$inserted_output"
zpty -d omp_completion
"#,
        ])
        .env("OMP_COMPLETION_DIR", &zsh_dir);
    let output = command.output().expect("failed to run Zsh insertion probe");
    assert!(
        output.status.success(),
        "Zsh insertion probe failed for {line:?}:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn shell_completion_candidates(temp: &TestDir, shell: &str, script: &Path, line: &str) -> String {
    match shell {
        "bash" => {
            let mut command = shell_command(temp, shell, script, line);
            command.args([
                "--noprofile",
                "--norc",
                "-c",
                r#"
source "$OMP_COMPLETION_SCRIPT" || exit 81
COMP_LINE="$OMP_COMPLETION_LINE"
COMP_POINT=${#COMP_LINE}
COMP_TYPE=9
read -r -a COMP_WORDS <<< "$COMP_LINE"
if [[ "$COMP_LINE" == *" " ]]; then COMP_WORDS+=(""); fi
COMP_CWORD=$((${#COMP_WORDS[@]} - 1))
registration=$(complete -p omp-topology 2>/dev/null) || exit 82
read -r -a registration_words <<< "$registration"
completion_function=""
for ((i = 0; i < ${#registration_words[@]}; i++)); do
  if [[ "${registration_words[i]}" == "-F" ]]; then
    completion_function="${registration_words[i + 1]}"
    break
  fi
done
[[ -n "$completion_function" ]] || exit 83
"$completion_function"
printf '%s\n' "${COMPREPLY[@]}"
"#,
            ]);
            let output = command
                .output()
                .expect("failed to run Bash completion probe");
            assert!(
                output.status.success(),
                "Bash completion probe failed for {line:?}:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
        "fish" => {
            let mut command = shell_command(temp, shell, script, line);
            command.args([
                "--no-config",
                "-c",
                r#"source "$OMP_COMPLETION_SCRIPT"; complete -C "$OMP_COMPLETION_LINE""#,
            ]);
            let output = command
                .output()
                .expect("failed to run Fish completion probe");
            assert!(
                output.status.success(),
                "Fish completion probe failed for {line:?}:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
        "zsh" => zsh_completion_candidates(temp, script, line),
        other => panic!("unsupported completion test shell {other}"),
    }
}

fn normalized_candidates(raw: &str, query: &str) -> String {
    let mut output = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            for control in chars.by_ref() {
                if control.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if ch != '\r' {
            output.push(ch);
        }
    }
    output.replace(query, "")
}

fn completion_has_candidate(output: &str, candidate: &str) -> bool {
    output
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_')))
        .any(|word| word == candidate)
}

fn assert_completion_candidates(
    temp: &TestDir,
    shell: &str,
    script: &Path,
    line: &str,
    expected: &[&str],
    forbidden: &[&str],
) {
    let raw = shell_completion_candidates(temp, shell, script, line);
    let output = normalized_candidates(&raw, line);
    for candidate in expected {
        assert!(
            completion_has_candidate(&output, candidate),
            "{shell} did not offer {candidate:?} for {line:?}; candidates:\n{raw}"
        );
    }
    for candidate in forbidden {
        assert!(
            !completion_has_candidate(&output, candidate),
            "{shell} offered invalid candidate {candidate:?} for {line:?}; candidates:\n{raw}"
        );
    }
}

#[test]
fn completion_scripts_are_stdout_only_and_do_not_mutate_user_state() {
    for shell in ["bash", "fish", "zsh"] {
        let temp = TestDir::new();
        let mut command = temp.cli_command(Path::new(env!("CARGO_MANIFEST_DIR")));
        let roots = [
            temp.0.join("home"),
            temp.0.join("xdg-config"),
            temp.0.join("state"),
        ];
        let before = snapshot_roots(&roots);
        command.args(["completions", shell]);
        let output = command
            .output()
            .expect("failed to execute completion command");
        assert!(
            output.status.success(),
            "completions {shell} failed: {output:?}"
        );
        assert!(
            !output.stdout.is_empty(),
            "completions {shell} emitted no script"
        );
        assert!(
            output.stderr.is_empty(),
            "completions {shell} wrote to stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            snapshot_roots(&roots),
            before,
            "completions {shell} changed HOME, XDG config, or state"
        );
    }

    let temp = TestDir::new();
    let output = temp
        .cli_command(Path::new(env!("CARGO_MANIFEST_DIR")))
        .args(["completions", "powershell"])
        .output()
        .expect("failed to execute invalid completion command");
    assert_eq!(
        output.status.code(),
        Some(2),
        "unsupported shell was not a usage error"
    );
    assert!(
        output.stdout.is_empty(),
        "unsupported shell emitted a partial script"
    );
}

#[test]
fn completion_actual_bash_fish_zsh_candidates_filter_invalid_combinations() {
    let temp = TestDir::new();
    let (_source_root, _, _) = doctor_fixture(&temp);
    let completion_dir = temp.0.join("generated-completions");
    fs::create_dir_all(&completion_dir).expect("failed to create completion output dir");

    let probe_script = completion_dir.join("runtime-probe");
    for shell in ["bash", "fish", "zsh"] {
        let args: &[&str] = match shell {
            "bash" => &["--noprofile", "--norc", "-c", "exit 0"],
            "fish" => &["--no-config", "-c", "exit 0"],
            "zsh" => &["-f", "-c", "exit 0"],
            _ => unreachable!(),
        };
        let mut probe = shell_command(&temp, shell, &probe_script, "");
        probe.args(args);
        let available = probe.output();
        assert!(
            available.is_ok_and(|output| output.status.success()),
            "{shell} runtime is required for actual completion acceptance"
        );
    }

    let roots = [
        temp.0.join("home"),
        temp.0.join("xdg-config"),
        temp.0.join("state"),
    ];
    let before = snapshot_roots(&roots);
    for shell in ["bash", "fish", "zsh"] {
        let script = completion_dir.join(format!("{shell}.completion"));
        completion_script(&temp, shell, &script);
        let queries: &[(&str, &[&str], &[&str])] = &[
            ("omp-topology ", &["doctor", "completions"], &["powershell"]),
            (
                "omp-topology completions ",
                &["bash", "fish", "zsh"],
                &["powershell"],
            ),
            (
                "omp-topology doctor ",
                &["--install-id", "--profile", "--agent-dir", "--json"],
                &[],
            ),
            (
                "omp-topology doctor --profile demo ",
                &["--json"],
                &["--install-id", "--agent-dir"],
            ),
            (
                "omp-topology doctor --json --install-id doctor-profile ",
                &[],
                &["--profile", "--agent-dir"],
            ),
            (
                "omp-topology doctor --install-id ",
                &["doctor-profile", "doctor-explicit"],
                &["unknown-install"],
            ),
            (
                "omp-topology doctor --profile ",
                &["demo"],
                &["missing-profile"],
            ),
            (
                "omp-topology install --topology fixture --profile demo ",
                &["--alias"],
                &["--agent-dir"],
            ),
            (
                "omp-topology install --topology fixture --agent-dir /tmp/target ",
                &[],
                &["--profile", "--alias"],
            ),
            (
                "omp-topology install --topology fixture --config-merge ",
                &["--config-precedence"],
                &["--config-replace"],
            ),
            (
                "omp-topology install --topology fixture --config-replace ",
                &[],
                &["--config-merge", "--config-precedence"],
            ),
            (
                "omp-topology install --topology fixture ",
                &["--source"],
                &["--ref"],
            ),
            (
                "omp-topology install --topology fixture --source /tmp/source ",
                &["--ref"],
                &[],
            ),
            (
                "omp-topology update --install-id doctor-profile ",
                &["--source"],
                &["--ref"],
            ),
            (
                "omp-topology update --install-id doctor-profile --source /tmp/source ",
                &[],
                &["--ref"],
            ),
            (
                "omp-topology status --install-id doctor-profile ",
                &[],
                &["--profile", "--agent-dir"],
            ),
            (
                "omp-topology remove --install-id doctor-profile ",
                &[],
                &["--profile", "--agent-dir"],
            ),
        ];
        for (line, expected, forbidden) in queries {
            assert_completion_candidates(&temp, shell, &script, line, expected, forbidden);
        }

        assert_eq!(
            snapshot_roots(&roots),
            before,
            "completion probes changed user state while running {shell}"
        );
    }
}

#[test]
fn completion_update_ref_tracks_effective_source_for_all_shells() {
    let temp = TestDir::new();
    let (source_root, profile_agent, explicit_agent) = doctor_fixture(&temp);
    let registry_path = temp.0.join("state/omp-topology/registry.json");
    let mut registry: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("fixture registry is missing"))
            .expect("fixture registry is invalid JSON");
    registry["aliases"]["doctor-profile"]["source"] = serde_json::json!({
        "kind": "github",
        "owner": "test-owner",
        "repo": "test-repo"
    });
    registry["aliases"]["doctor-profile"]["resolved_commit"] =
        serde_json::Value::String("0123456789abcdef0123456789abcdef01234567".to_string());
    fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&registry).expect("failed to encode GitHub fixture registry"),
    )
    .expect("failed to seed registered GitHub source");

    let external_log = temp.0.join("unexpected-network-tool.log");
    let bin_dir = temp.0.join("bin");
    for tool in ["git", "curl"] {
        let executable = bin_dir.join(tool);
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '{tool} %s\\n' \"$*\" >> '{}'\nexit 99\n",
                external_log.display()
            ),
        )
        .expect("failed to write network tripwire");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
            .expect("failed to make network tripwire executable");
    }

    let completion_dir = temp.0.join("source-aware-completions");
    fs::create_dir_all(&completion_dir).expect("failed to create completion output dir");
    let probe_script = completion_dir.join("runtime-probe");
    for shell in ["bash", "fish", "zsh"] {
        let args: &[&str] = match shell {
            "bash" => &["--noprofile", "--norc", "-c", "exit 0"],
            "fish" => &["--no-config", "-c", "exit 0"],
            "zsh" => &["-f", "-c", "exit 0"],
            _ => unreachable!(),
        };
        let mut probe = shell_command(&temp, shell, &probe_script, "");
        probe.args(args);
        let output = probe.output().expect("failed to probe shell runtime");
        assert!(
            output.status.success(),
            "{shell} runtime is required for source-aware candidate acceptance"
        );
    }

    let source_arg = source_root.to_string_lossy().into_owned();
    let cases = [
        (
            "omp-topology update --install-id doctor-profile ".to_string(),
            true,
            "registered GitHub source without override",
        ),
        (
            "omp-topology update --install-id doctor-explicit ".to_string(),
            false,
            "registered local source without override",
        ),
        (
            "omp-topology update --install-id doctor-explicit --source github:test-owner/test-repo "
                .to_string(),
            true,
            "explicit GitHub source override",
        ),
        (
            format!(
                "omp-topology update --install-id doctor-profile --source {source_arg} "
            ),
            false,
            "explicit local source override",
        ),
    ];
    let roots = [
        temp.0.join("home"),
        temp.0.join("xdg-config"),
        temp.0.join("state"),
        source_root,
        profile_agent,
        explicit_agent,
    ];
    let before = snapshot_roots(&roots);
    let mut failures = Vec::new();

    for shell in ["bash", "fish", "zsh"] {
        let script = completion_dir.join(format!("{shell}.completion"));
        completion_script(&temp, shell, &script);
        for (line, expected_ref, context) in &cases {
            let raw = shell_completion_candidates(&temp, shell, &script, line);
            let candidates = normalized_candidates(&raw, line);
            let found_ref = completion_has_candidate(&candidates, "--ref");
            if found_ref != *expected_ref {
                failures.push(format!(
                    "{shell}: {context}: expected --ref candidate={expected_ref}, got {found_ref}\nquery: {line:?}\ncandidates:\n{raw}"
                ));
            }
        }
    }

    assert!(
        !external_log.exists(),
        "source-aware completion invoked git or curl"
    );
    assert_eq!(
        snapshot_roots(&roots),
        before,
        "source-aware completion changed profile, registry, source, or config state"
    );
    assert!(
        failures.is_empty(),
        "update --ref candidates ignored the effective source:\n{}",
        failures.join("\n")
    );
}

#[test]
fn bash_completion_preserves_profile_names_with_spaces() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let profile_name = "profile with spaces";
    let profile_agent = temp
        .0
        .join("home/.omp/profiles")
        .join(profile_name)
        .join("agent");
    write_cli_fixture_topology(&source_root);
    install_cli_fixture(
        &temp,
        &source_root,
        "space-profile",
        &profile_agent,
        Some(profile_name),
    );

    let script = temp.0.join("bash.completion");
    completion_script(&temp, "bash", &script);
    let raw = shell_completion_candidates(&temp, "bash", &script, "omp-topology doctor --profile ");

    assert_eq!(
        raw,
        format!("{profile_name}\n"),
        "Bash must return the profile name as one exact completion candidate"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn bash_completion_quotes_profile_on_actual_insertion() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let profile_name = "profile with spaces";
    let profile_agent = temp
        .0
        .join("home/.omp/profiles")
        .join(profile_name)
        .join("agent");
    write_cli_fixture_topology(&source_root);
    install_cli_fixture(
        &temp,
        &source_root,
        "space-profile",
        &profile_agent,
        Some(profile_name),
    );

    let script_path = temp.0.join("bash.completion");
    completion_script(&temp, "bash", &script_path);
    let mut command = shell_command(&temp, "script", &script_path, "");
    command
        .args(["-qefc", "bash --noprofile --norc -i", "/dev/null"])
        .env("TERM", "dumb")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("failed to start Bash under a PTY");
    let input = concat!(
        "source \"$OMP_COMPLETION_SCRIPT\" || exit 81\n",
        "__omp_inspect_completion() {\n",
        "  local -a parsed\n",
        "  eval \"parsed=( $READLINE_LINE )\"\n",
        "  printf '\\n__OMP_WORD_COUNT__%d\\n' \"${#parsed[@]}\"\n",
        "  printf '__OMP_PROFILE_ARG__%s\\n' \"${parsed[3]-<missing>}\"\n",
        "  READLINE_LINE='exit 0'\n",
        "  READLINE_POINT=${#READLINE_LINE}\n",
        "}\n",
        "bind -x '\"\\C-x\\C-x\":__omp_inspect_completion' || exit 82\n",
        "omp-topology doctor --profile pro\t\x18\x18\n",
    );
    child
        .stdin
        .take()
        .expect("PTY input was not piped")
        .write_all(input.as_bytes())
        .expect("failed to send interactive Bash input");
    let output = child
        .wait_with_output()
        .expect("failed to collect interactive Bash output");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "interactive Bash completion probe failed:\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output_lines = stdout.lines().map(|line| line.trim_end_matches('\r'));
    let markers: Vec<_> = output_lines.collect();
    assert!(
        markers.contains(&"__OMP_WORD_COUNT__4"),
        "profile completion should parse as four shell words; PTY output:\n{stdout}"
    );
    assert!(
        markers.contains(&"__OMP_PROFILE_ARG__profile with spaces"),
        "the profile argument should retain its full value; PTY output:\n{stdout}"
    );
}

#[test]
fn bash_completion_handles_profile_equals_wordbreak() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let profile_name = "design-profile";
    let profile_agent = temp
        .0
        .join("home/.omp/profiles")
        .join(profile_name)
        .join("agent");
    write_cli_fixture_topology(&source_root);
    install_cli_fixture(
        &temp,
        &source_root,
        "design-profile",
        &profile_agent,
        Some(profile_name),
    );

    let script = temp.0.join("bash.completion");
    completion_script(&temp, "bash", &script);
    let mut command = shell_command(&temp, "bash", &script, "");
    command.args([
        "--noprofile",
        "--norc",
        "-c",
        r#"
source "$OMP_COMPLETION_SCRIPT" || exit 81
COMP_WORDS=(omp-topology doctor --profile = des)
COMP_CWORD=4
COMP_LINE="omp-topology doctor --profile=des"
COMP_POINT=${#COMP_LINE}
COMP_TYPE=9
[[ ${#COMP_WORDS[@]} -eq 5 && "${COMP_WORDS[3]}" == "=" && "${COMP_WORDS[4]}" == "des" ]] || exit 87
registration=$(complete -p omp-topology 2>/dev/null) || exit 82
read -r -a registration_words <<< "$registration"
completion_function=""
for ((i = 0; i < ${#registration_words[@]}; i++)); do
  if [[ "${registration_words[i]}" == "-F" ]]; then
    completion_function="${registration_words[i + 1]}"
    break
  fi
done
[[ -n "$completion_function" ]] || exit 83
"$completion_function"
printf 'COMPREPLY:%s\n' "${COMPREPLY[@]}"
"#,
    ]);
    let output = command
        .output()
        .expect("failed to run Bash equals-wordbreak completion probe");
    assert!(
        output.status.success(),
        "Bash completion probe failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line == "COMPREPLY:design-profile"),
        "Bash did not return the expected profile candidate for the equals-wordbreak input; probe output:\n{stdout}"
    );
}

#[test]
fn bash_completion_handles_empty_profile_equals_wordbreak() {
    let temp = TestDir::new();
    doctor_fixture(&temp);

    let script = temp.0.join("bash.completion");
    completion_script(&temp, "bash", &script);
    let mut command = shell_command(&temp, "bash", &script, "");
    command.args([
        "--noprofile",
        "--norc",
        "-c",
        r#"
source "$OMP_COMPLETION_SCRIPT" || exit 81
COMP_WORDS=(omp-topology doctor --profile =)
COMP_CWORD=3
COMP_LINE="omp-topology doctor --profile="
COMP_POINT=${#COMP_LINE}
COMP_TYPE=9
registration=$(complete -p omp-topology 2>/dev/null) || exit 82
read -r -a registration_words <<< "$registration"
completion_function=""
for ((i = 0; i < ${#registration_words[@]}; i++)); do
  if [[ "${registration_words[i]}" == "-F" ]]; then
    completion_function="${registration_words[i + 1]}"
    break
  fi
done
[[ -n "$completion_function" ]] || exit 83
"$completion_function"
printf 'COMPREPLY:%s\n' "${COMPREPLY[@]}"
"#,
    ]);
    let output = command
        .output()
        .expect("failed to run Bash empty equals-wordbreak completion probe");
    assert!(
        output.status.success(),
        "Bash completion probe failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.lines().any(|line| line == "COMPREPLY:demo"),
        "Bash did not return the profile candidate for the empty equals-wordbreak value; probe output:\n{stdout}"
    );
    assert!(
        !stdout.lines().any(|line| line == "COMPREPLY:--profile"),
        "Bash re-completed the --profile option instead of its empty value; probe output:\n{stdout}"
    );
}

#[test]
fn bash_completion_handles_github_url_wordbreaks() {
    let temp = TestDir::new();
    doctor_fixture(&temp);

    let registry_path = temp.0.join("state/omp-topology/registry.json");
    let mut registry: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("fixture registry is missing"))
            .expect("fixture registry is invalid JSON");
    registry["aliases"]["doctor-profile"]["source"] = serde_json::json!({
        "kind": "github",
        "owner": "acme",
        "repo": "repo"
    });
    fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&registry).expect("failed to encode GitHub fixture registry"),
    )
    .expect("failed to seed registered GitHub source");

    let script = temp.0.join("bash.completion");
    completion_script(&temp, "bash", &script);
    let mut command = shell_command(&temp, "bash", &script, "");
    command.args([
        "--noprofile",
        "--norc",
        "-c",
        r#"
source "$OMP_COMPLETION_SCRIPT" || exit 81
COMP_WORDBREAKS=$' \t\n"\'@><=;|&(:/'
[[ "$COMP_WORDBREAKS" == *:* && "$COMP_WORDBREAKS" == */* ]] || exit 84
registration=$(complete -p omp-topology 2>/dev/null) || exit 82
read -r -a registration_words <<< "$registration"
completion_function=""
for ((i = 0; i < ${#registration_words[@]}; i++)); do
  if [[ "${registration_words[i]}" == "-F" ]]; then
    completion_function="${registration_words[i + 1]}"
    break
  fi
done
[[ -n "$completion_function" ]] || exit 83

COMP_LINE='omp-topology update --install-id doctor-profile '
COMP_POINT=${#COMP_LINE}
COMP_TYPE=9
COMP_WORDS=(omp-topology update --install-id doctor-profile)
COMP_CWORD=${#COMP_WORDS[@]}
"$completion_function"
printf 'CONTROL:%s\n' "${COMPREPLY[@]}"
COMP_LINE='omp-topology update --install-id doctor-profile --source /tmp/local '
COMP_POINT=${#COMP_LINE}
COMP_WORDS=(omp-topology update --install-id doctor-profile --source /tmp/local)
COMP_CWORD=${#COMP_WORDS[@]}
"$completion_function"
printf 'LOCAL:%s\n' "${COMPREPLY[@]}"


COMP_LINE='omp-topology update --install-id doctor-profile --source=https://github.com/acme/repo '
COMP_POINT=${#COMP_LINE}
COMP_WORDS=()
word=''
for ((i = 0; i < ${#COMP_LINE}; i++)); do
  char=${COMP_LINE:i:1}
  if [[ "$char" == ' ' ]]; then
    [[ -z "$word" ]] || COMP_WORDS+=("$word")
    word=''
  elif [[ "$COMP_WORDBREAKS" == *"$char"* ]]; then
    [[ -z "$word" ]] || COMP_WORDS+=("$word")
    COMP_WORDS+=("$char")
    word=''
  else
    word+="$char"
  fi
done
[[ -z "$word" ]] || COMP_WORDS+=("$word")
COMP_CWORD=${#COMP_WORDS[@]}
[[ "${COMP_WORDS[*]}" == 'omp-topology update --install-id doctor-profile --source = https : / / github.com / acme / repo' ]] || exit 85
"$completion_function"
printf 'URL:%s\n' "${COMPREPLY[@]}"
printf '__OMP_ADAPTER_INVOKED__\n'
"#,
    ]);
    let output = command
        .output()
        .expect("failed to run Bash URL-wordbreak completion probe");
    assert!(
        output.status.success(),
        "Bash URL-wordbreak completion probe failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.lines().any(|line| line == "__OMP_ADAPTER_INVOKED__"),
        "Bash generated adapter did not run; probe output:\n{stdout}"
    );
    assert!(
        stdout.lines().any(|line| line == "CONTROL:--ref"),
        "Bash control did not offer --ref for the registered GitHub source; probe output:\n{stdout}"
    );
    assert!(
        !stdout.lines().any(|line| line == "LOCAL:--ref"),
        "Bash offered --ref for the explicit local source; probe output:\n{stdout}"
    );
    assert!(
        stdout.lines().any(|line| line == "URL:--ref"),
        "Bash did not offer --ref for the colon/slash-broken GitHub URL source; probe output:\n{stdout}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn fish_completion_preserves_newlines_in_profile_prefix_tokens() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let newline_profile = "line\nbreak-profile";
    let decoy_profile = "linebreak-decoy";
    write_cli_fixture_topology(&source_root);
    for (install_id, profile_name) in [
        ("newline-profile", newline_profile),
        ("newline-decoy", decoy_profile),
    ] {
        let profile_agent = temp
            .0
            .join("home/.omp/profiles")
            .join(profile_name)
            .join("agent");
        install_cli_fixture(
            &temp,
            &source_root,
            install_id,
            &profile_agent,
            Some(profile_name),
        );
    }

    let mut backend = temp.cli_command(Path::new(env!("CARGO_MANIFEST_DIR")));
    backend
        .args(["__complete", "--", "omp-topology", "doctor", "--profile"])
        .arg("line\n");
    let backend_output = backend
        .output()
        .expect("failed to verify newline profile completion candidate");
    assert!(
        backend_output.status.success(),
        "hidden completion backend failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&backend_output.stdout),
        String::from_utf8_lossy(&backend_output.stderr)
    );
    let backend_candidates = backend_output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|candidate| !candidate.is_empty())
        .collect::<Vec<_>>();
    assert!(
        backend_candidates.contains(&newline_profile.as_bytes()),
        "hidden completion backend did not provide the newline profile candidate: {backend_candidates:?}"
    );
    assert!(
        !backend_candidates.contains(&decoy_profile.as_bytes()),
        "hidden completion backend matched the decoy for the newline-containing prefix: {backend_candidates:?}"
    );

    let script = temp.0.join("fish.completion");
    completion_script(&temp, "fish", &script);
    let mut command = shell_command(&temp, "script", &script, "");
    command
        .args(["-qefc", "fish --no-config -i", "/dev/null"])
        .env_remove("CARGO_BIN_EXE_omp-topology")
        .env(
            "OMP_COMPLETION_LINE",
            "omp-topology doctor --profile 'line\n",
        )
        .env("TERM", "xterm-256color")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("failed to start Fish under a PTY");
    let mut input = String::from(
        r#"source "$OMP_COMPLETION_SCRIPT" || exit 81
function __omp_inspect_completion
    commandline -r "$OMP_COMPLETION_LINE"
    commandline -f complete
    commandline -r "$OMP_COMPLETION_LINE"
    set -l candidates (__omp_topology_complete)
    printf '\n__OMP_ADAPTER_INVOKED__\n'
    for candidate in $candidates
        printf '__OMP_CANDIDATE__%s\n' (string escape -- "$candidate")
    end
    commandline -r 'exit 0'
end
bind \t __omp_inspect_completion || exit 82
"#,
    );
    input.push_str("\t\n");
    child
        .stdin
        .take()
        .expect("PTY input was not piped")
        .write_all(input.as_bytes())
        .expect("failed to send Fish completion input");
    let output = child
        .wait_with_output()
        .expect("failed to collect Fish completion output");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "interactive Fish completion probe failed:\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("__OMP_ADAPTER_INVOKED__"),
        "interactive Fish did not invoke the generated adapter: PTY output:\n{stdout}"
    );
    assert!(
        stdout.contains("__OMP_CANDIDATE__line\\nbreak-profile"),
        "Fish adapter did not return the exact profile candidate with an embedded newline in its prefix; PTY output:\n{stdout}"
    );
    assert!(
        !stdout.contains("__OMP_CANDIDATE__linebreak-decoy"),
        "Fish split the newline-containing profile prefix and matched the decoy; PTY output:\n{stdout}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn fish_completion_preserves_newline_in_completed_argument() {
    let temp = TestDir::new();
    let source_roots = [temp.0.join("source"), temp.0.join("source\n--config-merge")];
    for source_root in &source_roots {
        write_cli_fixture_topology(source_root);
    }

    let script = temp.0.join("fish.completion");
    completion_script(&temp, "fish", &script);

    for (case, source_root) in ["control", "newline-path"].into_iter().zip(source_roots) {
        let source = source_root.to_string_lossy();
        let line = format!("omp-topology update --source '{source}' --config-r");
        let mut command = shell_command(&temp, "script", &script, "");
        command
            .args(["-qefc", "fish --no-config -i", "/dev/null"])
            .env_remove("CARGO_BIN_EXE_omp-topology")
            .env("OMP_COMPLETION_LINE", &line)
            .env("TERM", "xterm-256color")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("failed to start Fish under a PTY");
        let mut input = String::from(
            r#"source "$OMP_COMPLETION_SCRIPT" || exit 81
function __omp_inspect_completion
    commandline -r "$OMP_COMPLETION_LINE"
    set -l candidates (__omp_topology_complete)
    printf '\n__OMP_ADAPTER_INVOKED__\n'
    for candidate in $candidates
        printf '__OMP_CANDIDATE__%s\n' (string escape -- "$candidate")
    end
    commandline -r 'exit 0'
end
bind \t __omp_inspect_completion || exit 82
"#,
        );
        input.push_str("\t\n");
        child
            .stdin
            .take()
            .expect("PTY input was not piped")
            .write_all(input.as_bytes())
            .expect("failed to send Fish completion input");
        let output = child
            .wait_with_output()
            .expect("failed to collect Fish completion output");
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(
            output.status.success(),
            "{case} Fish completion probe failed:\nstdout:\n{stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("__OMP_ADAPTER_INVOKED__"),
            "{case} Fish did not invoke the generated adapter: PTY output:\n{stdout}"
        );
        assert!(
            stdout.contains("__OMP_CANDIDATE__--config-replace"),
            "{case} Fish adapter did not return --config-replace: PTY output:\n{stdout}"
        );
    }
}

#[test]
fn fish_noninteractive_completion_preserves_newline_in_completed_argument() {
    let temp = TestDir::new();
    let source_roots = [temp.0.join("source"), temp.0.join("source\n--config-merge")];
    for source_root in &source_roots {
        write_cli_fixture_topology(source_root);
    }

    let script = temp.0.join("fish.completion");
    completion_script(&temp, "fish", &script);

    let control_source = source_roots[0].to_string_lossy();
    let control_line = format!(
        "omp-topology install --topology cli-contract-fixture --source '{control_source}' "
    );
    let control = shell_completion_candidates(&temp, "fish", &script, &control_line);
    assert!(
        completion_has_candidate(&control, "--config-replace"),
        "Fish did not offer --config-replace for the valid control input; candidates:\n{control}"
    );
    assert!(
        !completion_has_candidate(&control, "--config-precedence"),
        "Fish offered --config-precedence without --config-merge for the valid control input; candidates:\n{control}"
    );

    let newline_source = source_roots[1].to_string_lossy();
    let newline_line = format!(
        "omp-topology install --topology cli-contract-fixture --source '{newline_source}' "
    );
    let newline_candidates = shell_completion_candidates(&temp, "fish", &script, &newline_line);
    assert!(
        !completion_has_candidate(&newline_candidates, "--config-precedence"),
        "Fish split the embedded newline in the completed --source token into a separate --config-merge option; candidates:\n{newline_candidates}"
    );
    assert!(
        completion_has_candidate(&newline_candidates, "--config-replace"),
        "Fish did not offer --config-replace for the newline-bearing --source token; candidates:\n{newline_candidates}"
    );
}
#[test]
fn fish_noninteractive_completion_keeps_command_substitution_as_one_token() {
    let temp = TestDir::new();
    let script = temp.0.join("fish.completion");
    completion_script(&temp, "fish", &script);

    let control_line = "omp-topology install --topology cli-contract-fixture --source /tmp/source ";
    let control = shell_completion_candidates(&temp, "fish", &script, control_line);
    assert!(
        completion_has_candidate(&control, "--config-replace"),
        "Fish control did not offer --config-replace; candidates:\n{control}"
    );
    assert!(
        !completion_has_candidate(&control, "--config-precedence"),
        "Fish control offered --config-precedence without --config-merge; candidates:\n{control}"
    );

    let substitution_line = "omp-topology install --topology cli-contract-fixture --source (printf '%s' --config-merge ) ";
    let substitution = shell_completion_candidates(&temp, "fish", &script, substitution_line);
    assert!(
        completion_has_candidate(&substitution, "--config-replace"),
        "Fish treated --config-merge inside one command-substitution token as an option and suppressed --config-replace; candidates:\n{substitution}"
    );
    assert!(
        !completion_has_candidate(&substitution, "--config-precedence"),
        "Fish treated --config-merge inside one command-substitution token as an option; candidates:\n{substitution}"
    );
}

#[test]
fn fish_noninteractive_completion_removes_backslash_newline_continuation() {
    let temp = TestDir::new();
    doctor_fixture(&temp);
    let registry_path = temp.0.join("state/omp-topology/registry.json");
    let mut registry: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("fixture registry is missing"))
            .expect("fixture registry is invalid JSON");
    registry["aliases"]["doctor-profile"]["source"] = serde_json::json!({
        "kind": "github",
        "owner": "acme",
        "repo": "repo"
    });
    fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&registry).expect("failed to encode GitHub fixture registry"),
    )
    .expect("failed to seed GitHub source for continuation control");

    let script = temp.0.join("fish.completion");
    completion_script(&temp, "fish", &script);
    let control_line = "omp-topology update --install-id doctor-profile ";
    let control = shell_completion_candidates(&temp, "fish", &script, control_line);
    assert!(
        completion_has_candidate(&control, "--ref"),
        "Fish control did not offer --ref for the registered GitHub source; candidates:\n{control}"
    );

    let continued_line = "omp-topology update --install-id doctor-\\\nprofile ";
    let continued = shell_completion_candidates(&temp, "fish", &script, continued_line);
    assert!(
        completion_has_candidate(&continued, "--ref"),
        "Fish did not recognize the shell-effective doctor-profile value across backslash-newline continuation; candidates:\n{continued}"
    );
}

#[test]
fn bash_completion_handles_trailing_space_without_empty_word() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let profile_name = "trailing-space-profile";
    let profile_agent = temp
        .0
        .join("home/.omp/profiles")
        .join(profile_name)
        .join("agent");
    write_cli_fixture_topology(&source_root);
    install_cli_fixture(
        &temp,
        &source_root,
        "trailing-space-profile",
        &profile_agent,
        Some(profile_name),
    );

    let script = temp.0.join("bash.completion");
    completion_script(&temp, "bash", &script);
    let mut command = shell_command(&temp, "bash", &script, "omp-topology doctor --profile ");
    command.args([
        "--noprofile",
        "--norc",
        "-c",
        r#"
source "$OMP_COMPLETION_SCRIPT" || exit 81
COMP_LINE="$OMP_COMPLETION_LINE"
COMP_POINT=${#COMP_LINE}
COMP_TYPE=9
read -r -a COMP_WORDS <<< "$COMP_LINE"
COMP_CWORD=${#COMP_WORDS[@]}
[[ "$COMP_LINE" == *" " ]] || exit 84
[[ ${#COMP_WORDS[@]} -eq COMP_CWORD ]] || exit 85
[[ "${COMP_WORDS[*]}" == "omp-topology doctor --profile" ]] || exit 86
registration=$(complete -p omp-topology 2>/dev/null) || exit 82
read -r -a registration_words <<< "$registration"
completion_function=""
for ((i = 0; i < ${#registration_words[@]}; i++)); do
  if [[ "${registration_words[i]}" == "-F" ]]; then
    completion_function="${registration_words[i + 1]}"
    break
  fi
done
[[ -n "$completion_function" ]] || exit 83
"$completion_function"
printf 'COMPREPLY:%s\n' "${COMPREPLY[@]}"
"#,
    ]);
    let output = command
        .output()
        .expect("failed to run Bash trailing-space completion probe");
    assert!(
        output.status.success(),
        "Bash completion probe failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line == format!("COMPREPLY:{profile_name}")),
        "Bash did not return the expected profile candidate in COMPREPLY; probe output:\n{stdout}"
    );
}

#[test]
fn bash_completion_ignores_words_after_cursor() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let profile_name = "cursor-profile";
    let profile_agent = temp
        .0
        .join("home/.omp/profiles")
        .join(profile_name)
        .join("agent");
    let tail_agent_dir = temp.0.join("tail-agent-dir");
    write_cli_fixture_topology(&source_root);
    install_cli_fixture(
        &temp,
        &source_root,
        "cursor-profile",
        &profile_agent,
        Some(profile_name),
    );
    fs::create_dir_all(&tail_agent_dir).expect("failed to create tail agent directory");

    let script = temp.0.join("bash.completion");
    completion_script(&temp, "bash", &script);
    let mut command = shell_command(&temp, "bash", &script, "");
    command
        .env("OMP_COMPLETION_TAIL_PATH", &tail_agent_dir)
        .args([
            "--noprofile",
            "--norc",
            "-c",
            r#"
source "$OMP_COMPLETION_SCRIPT" || exit 81
COMP_WORDS=(omp-topology doctor --profile "" --agent-dir "$OMP_COMPLETION_TAIL_PATH")
COMP_CWORD=3
COMP_LINE="omp-topology doctor --profile \"\" --agent-dir $OMP_COMPLETION_TAIL_PATH"
cursor_prefix='omp-topology doctor --profile ""'
COMP_POINT=${#cursor_prefix}
COMP_TYPE=9
registration=$(complete -p omp-topology 2>/dev/null) || exit 82
read -r -a registration_words <<< "$registration"
completion_function=""
for ((i = 0; i < ${#registration_words[@]}; i++)); do
  if [[ "${registration_words[i]}" == "-F" ]]; then
    completion_function="${registration_words[i + 1]}"
    break
  fi
done
[[ -n "$completion_function" ]] || exit 83
"$completion_function"
printf 'COMPREPLY:%s\n' "${COMPREPLY[@]}"
"#,
        ]);
    let output = command
        .output()
        .expect("failed to run Bash mid-line completion probe");
    assert!(
        output.status.success(),
        "Bash completion probe failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line == format!("COMPREPLY:{profile_name}")),
        "Bash did not return the profile candidate before the post-cursor words; probe output:\n{stdout}"
    );
}

#[test]
fn bash_completion_does_not_overwrite_global_candidate() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let profile_name = "candidate-profile";
    let profile_agent = temp
        .0
        .join("home/.omp/profiles")
        .join(profile_name)
        .join("agent");
    write_cli_fixture_topology(&source_root);
    install_cli_fixture(
        &temp,
        &source_root,
        "candidate-profile",
        &profile_agent,
        Some(profile_name),
    );

    let script = temp.0.join("bash.completion");
    completion_script(&temp, "bash", &script);
    let mut command = shell_command(&temp, "bash", &script, "omp-topology doctor --profile ");
    command.args([
        "--noprofile",
        "--norc",
        "-c",
        r#"
source "$OMP_COMPLETION_SCRIPT" || exit 81
COMP_LINE="$OMP_COMPLETION_LINE"
COMP_POINT=${#COMP_LINE}
COMP_TYPE=9
read -r -a COMP_WORDS <<< "$COMP_LINE"
if [[ "$COMP_LINE" == *" " ]]; then COMP_WORDS+=(""); fi
COMP_CWORD=$((${#COMP_WORDS[@]} - 1))
registration=$(complete -p omp-topology 2>/dev/null) || exit 82
read -r -a registration_words <<< "$registration"
completion_function=""
for ((i = 0; i < ${#registration_words[@]}; i++)); do
  if [[ "${registration_words[i]}" == "-F" ]]; then
    completion_function="${registration_words[i + 1]}"
    break
  fi
done
[[ -n "$completion_function" ]] || exit 83
candidate=sentinel
"$completion_function"
printf 'GLOBAL:%s\n' "$candidate"
printf 'COMPREPLY:%s\n' "${COMPREPLY[@]}"
"#,
    ]);
    let output = command
        .output()
        .expect("failed to run Bash completion global-variable probe");
    assert!(
        output.status.success(),
        "Bash completion probe failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line == format!("COMPREPLY:{profile_name}")),
        "Bash did not return the expected profile candidate; probe output:\n{stdout}"
    );
    assert!(
        stdout.lines().any(|line| line == "GLOBAL:sentinel"),
        "Bash completion overwrote the caller's global candidate; probe output:\n{stdout}"
    );
}

#[test]
fn zsh_completion_inserts_profile_names_with_spaces_as_one_word() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let profile_name = "profile with spaces";
    let profile_agent = temp
        .0
        .join("home/.omp/profiles")
        .join(profile_name)
        .join("agent");
    write_cli_fixture_topology(&source_root);
    install_cli_fixture(
        &temp,
        &source_root,
        "space-profile",
        &profile_agent,
        Some(profile_name),
    );

    let script = temp.0.join("zsh.completion");
    completion_script(&temp, "zsh", &script);
    let output = zsh_completion_inserted_words(&temp, &script, "omp-topology doctor --profile ");

    assert!(
        output.contains("__OMP_WORD_COUNT__4"),
        "expected completed profile to remain one shell word; ZLE parsed:\n{output}"
    );
    assert!(
        output.contains("__OMP_LAST_WORD__profile\\ with\\ spaces"),
        "expected final parsed word to be the complete escaped profile name; ZLE parsed:\n{output}"
    );
}
