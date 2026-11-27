use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

const TOPOLOGY: &str = "digital-product-studio";
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before UNIX epoch")
            .as_nanos();
        Self::create_with_timestamp(unique)
    }

    fn create_with_timestamp(unique: u128) -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "omp-topology-installer-{}-{unique}-{id}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("failed to create isolated test directory");
        Self(path)
    }

    fn state_home(&self) -> PathBuf {
        self.0.join("state")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn test_dir_creation_is_collision_resistant_under_concurrency() {
    const THREADS: usize = 16;
    let start = std::sync::Arc::new(std::sync::Barrier::new(THREADS));
    let workers = (0..THREADS)
        .map(|_| {
            let start = std::sync::Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                TestDir::create_with_timestamp(42)
            })
        })
        .collect::<Vec<_>>();
    let results = workers
        .into_iter()
        .map(|worker| worker.join())
        .collect::<Vec<_>>();

    assert!(
        results.iter().all(Result::is_ok),
        "concurrent TestDir creation failed"
    );
    let dirs = results.into_iter().map(Result::unwrap).collect::<Vec<_>>();
    let paths = dirs.iter().map(|dir| dir.0.clone()).collect::<Vec<_>>();
    for (index, path) in paths.iter().enumerate() {
        assert!(
            path.is_dir(),
            "created test directory is missing: {}",
            path.display()
        );
        assert!(
            !paths[..index].contains(path),
            "concurrent TestDir instances share a path: {}",
            path.display()
        );
    }
    drop(dirs);
    for path in paths {
        assert!(
            !path.exists(),
            "TestDir did not clean up: {}",
            path.display()
        );
    }
}

fn cli_command(temp: &TestDir, cwd: &Path) -> Command {
    let home = temp.0.join("home");
    let xdg_config = temp.0.join("xdg-config");
    fs::create_dir_all(&home).expect("failed to create isolated HOME");
    fs::create_dir_all(&xdg_config).expect("failed to create isolated XDG_CONFIG_HOME");

    let mut command = Command::new(env!("CARGO_BIN_EXE_omp-profile"));
    command
        .current_dir(cwd)
        .env_remove("OMP_HOME")
        .env_remove("OMP_PROFILE")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", xdg_config)
        .env("XDG_STATE_HOME", temp.state_home());
    command
}

fn run_cli_at(temp: &TestDir, agent_dir: &Path, cwd: &Path, args: &[&str]) -> Output {
    let mut command = cli_command(temp, cwd);
    command.args(args);
    if args.first() == Some(&"install") && !args.contains(&"--source") {
        let source = default_legacy_source(temp);
        command.arg("--source").arg(source);
    }
    command
        .arg("--agent-dir")
        .arg(agent_dir)
        .output()
        .expect("failed to execute omp-profile")
}

fn run_cli(temp: &TestDir, agent_dir: &Path, args: &[&str]) -> Output {
    run_cli_at(temp, agent_dir, Path::new(env!("CARGO_MANIFEST_DIR")), args)
}

fn run_cli_with_skills(
    temp: &TestDir,
    agent_dir: &Path,
    skills_dir: &Path,
    args: &[&str],
) -> Output {
    let mut command = cli_command(temp, Path::new(env!("CARGO_MANIFEST_DIR")));
    command.args(args);
    if args.first() == Some(&"install") && !args.contains(&"--source") {
        let source = default_legacy_source(temp);
        command.arg("--source").arg(source);
    }
    command
        .arg("--skills-dir")
        .arg(skills_dir)
        .arg("--agent-dir")
        .arg(agent_dir)
        .output()
        .expect("failed to execute omp-profile")
}

fn default_legacy_source(temp: &TestDir) -> PathBuf {
    local_topology_source(
        temp,
        "task:\n  eager: always\n  autoParallel: true\n  maxConcurrency: 4\n  maxRecursionDepth: 2\ntool:\n  web:\n    enabled: true\n    merge: patch\n",
        &[
            ("RULES.md", b"# Installer fixture rules\n"),
            (
                "skills/marketing-charter/SKILL.md",
                b"# Marketing charter fixture\n",
            ),
            (
                "agents/marketing.md",
                b"---\nname: marketing\n---\nInstaller test fixture.\n",
            ),
        ],
    )
}

fn local_topology_source(temp: &TestDir, config: &str, files: &[(&str, &[u8])]) -> PathBuf {
    local_named_topology_source(temp, "source", TOPOLOGY, config, files)
}

fn local_named_topology_source(
    temp: &TestDir,
    source_name: &str,
    topology_name: &str,
    config: &str,
    files: &[(&str, &[u8])],
) -> PathBuf {
    let source = temp.0.join(source_name);
    let topology = source.join("topologies").join(topology_name);
    let agent = topology.join("agent");
    fs::create_dir_all(&agent).expect("failed to create local topology fixture");
    fs::write(
        topology.join("topology.yml"),
        format!(
            "name: {topology_name}\nversion: 1.0.0\ndescription: installer fixture\ninstallable: true\n"
        ),
    )
    .expect("failed to write local topology metadata");
    fs::write(agent.join("config.yml"), config).expect("failed to write local topology config");
    for (relative, contents) in files {
        let path = agent.join(relative);
        fs::create_dir_all(path.parent().expect("topology file has no parent"))
            .expect("failed to create local topology file parent");
        fs::write(path, contents).expect("failed to write local topology file");
    }
    source
}


#[cfg(unix)]
fn run_cli_with_fake_omp(
    temp: &TestDir,
    default_agent_dir: &Path,
    profile_name: &str,
    profile_agent_dir: &Path,
    args: &[&str],
) -> Output {
    let bin_dir = temp.0.join("fake-omp-bin");
    fs::create_dir_all(&bin_dir).expect("failed to create fake PATH");
    let fake_omp = bin_dir.join("omp");
    fs::write(
        &fake_omp,
        "#!/bin/sh\n\
         if [ \"$1\" = \"--profile\" ]; then\n\
         \t[ \"$2\" = \"$OMP_TEST_PROFILE\" ] || exit 64\n\
         \tshift 2\n\
         \tif [ \"$1\" = \"config\" ] && [ \"$2\" = \"path\" ]; then\n\
         \t\tprintf '%s\\n' \"$OMP_TEST_PROFILE_AGENT_DIR\"\n\
         \t\texit 0\n\
         \tfi\n\
         \texit 64\n\
         fi\n\
         if [ \"$1\" = \"config\" ] && [ \"$2\" = \"path\" ]; then\n\
         \tprintf '%s\\n' \"$OMP_TEST_AGENT_DIR\"\n\
         \texit 0\n\
         fi\n\
         exit 64\n",
    )
    .expect("failed to write fake omp");
    fs::set_permissions(&fake_omp, fs::Permissions::from_mode(0o755))
        .expect("failed to make fake omp executable");
    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(bin_dir).chain(std::env::split_paths(&inherited_path)),
    )
    .expect("failed to build isolated PATH");

    cli_command(temp, Path::new(env!("CARGO_MANIFEST_DIR")))
        .args(args)
        .env("PATH", path)
        .env("OMP_TEST_AGENT_DIR", default_agent_dir)
        .env("OMP_TEST_PROFILE", profile_name)
        .env("OMP_TEST_PROFILE_AGENT_DIR", profile_agent_dir)
        .output()
        .expect("failed to execute omp-profile")
}

fn install_from_source(
    temp: &TestDir,
    agent_dir: &Path,
    source: &Path,
    extra_args: &[&str],
) -> Output {
    let source = source.to_str().expect("local source path is not UTF-8");
    let mut args = vec!["install", "-t", TOPOLOGY, "--source", source];
    args.extend_from_slice(extra_args);
    run_cli(temp, agent_dir, &args)
}
#[cfg(unix)]
fn run_install_from_source_with_path(
    temp: &TestDir,
    agent_dir: &Path,
    source: &Path,
    alias: &str,
    dry_run: bool,
    path: &Path,
) -> Output {
    let source = source.to_str().expect("local source path is not UTF-8");
    let profile = "path-preflight";
    let omp_log = temp.0.join("omp-invocations.log");
    let alias_marker = temp.0.join("omp-alias-created");
    let fake_omp = path.join("omp");
    fs::write(
        &fake_omp,
        "#!/bin/sh\n\
         printf '%s\\n' \"$*\" >> \"$OMP_TEST_OMP_LOG\"\n\
         if [ \"$#\" -eq 4 ] && [ \"$1\" = \"--profile\" ] && [ \"$2\" = \"$OMP_TEST_PROFILE\" ] && [ \"$3\" = \"config\" ] && [ \"$4\" = \"path\" ]; then\n\
         \tprintf '%s\\n' \"$OMP_TEST_PROFILE_AGENT_DIR\"\n\
         \texit 0\n\
         fi\n\
         if [ \"$#\" -eq 4 ] && [ \"$1\" = \"--profile\" ] && [ \"$2\" = \"$OMP_TEST_PROFILE\" ] && [ \"$3\" = \"--alias\" ]; then\n\
         \t: > \"$OMP_TEST_ALIAS_MARKER\"\n\
         \texit 0\n\
         fi\n\
         exit 64\n",
    )
    .expect("failed to write fake omp");
    fs::set_permissions(&fake_omp, fs::Permissions::from_mode(0o755))
        .expect("failed to make fake omp executable");
    let mut command = cli_command(temp, Path::new(env!("CARGO_MANIFEST_DIR")));
    command
        .args([
            "install",
            "-t",
            TOPOLOGY,
            "--source",
            source,
            "--profile",
            profile,
            "--alias",
            alias,
        ])
        .env("PATH", path)
        .env("OMP_TEST_PROFILE", profile)
        .env("OMP_TEST_PROFILE_AGENT_DIR", agent_dir)
        .env("OMP_TEST_OMP_LOG", omp_log)
        .env("OMP_TEST_ALIAS_MARKER", alias_marker);
    if dry_run {
        command.arg("--dry-run");
    }
    command.output().expect("failed to execute omp-profile")
}

#[cfg(unix)]
fn create_path_dir(temp: &TestDir) -> PathBuf {
    let path = temp.0.join("path");
    fs::create_dir(&path).expect("failed to create isolated PATH directory");
    path
}

#[cfg(unix)]
fn add_path_executable(path_dir: &Path, name: &str) -> PathBuf {
    let executable = path_dir.join(name);
    fs::write(&executable, b"#!/bin/sh\nexit 0\n")
        .expect("failed to create PATH executable fixture");
    let mut permissions = fs::metadata(&executable)
        .expect("failed to inspect PATH executable fixture")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions)
        .expect("failed to mark PATH executable fixture executable");
    executable
}

#[cfg(unix)]
fn assert_choose_another_alias_hint(stderr: &str) {
    let lower = stderr.to_ascii_lowercase();
    assert!(
        lower.contains("--alias")
            && (lower.contains("choose another")
                || lower.contains("choose a different")
                || lower.contains("use another")
                || lower.contains("use a different")),
        "error did not direct the user to choose another --alias:\n{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn installer_alias_preflight_refuses_path_executable_before_mutation() {
    let temp = TestDir::new();
    let path_dir = create_path_dir(&temp);
    let executable = add_path_executable(&path_dir, "teddy");
    let source = local_topology_source(&temp, "fixture: true\n", &[("payload.txt", b"payload")]);
    let agent_dir = temp.0.join("agent");
    assert!(!agent_dir.exists(), "test target must begin absent");
    assert!(
        !temp.state_home().exists(),
        "test state home must begin absent"
    );

    let output =
        run_install_from_source_with_path(&temp, &agent_dir, &source, "teddy", false, &path_dir);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "install unexpectedly succeeded despite PATH executable collision; target_exists={}, state_home_exists={}, registry_exists={}\nstdout:\n{}\nstderr:\n{}",
        agent_dir.exists(),
        temp.state_home().exists(),
        temp.state_home()
            .join("omp-topology/registry.json")
            .exists(),
        String::from_utf8_lossy(&output.stdout),
        stderr
    );
    assert!(
        stderr.contains("teddy"),
        "error did not name the colliding alias:\n{stderr}"
    );
    assert!(
        stderr.contains(executable.to_str().expect("executable path is not UTF-8")),
        "error did not include the full colliding executable path {}:\n{stderr}",
        executable.display()
    );
    assert_choose_another_alias_hint(&stderr);
    assert!(
        !agent_dir.exists(),
        "collision refusal created the install target"
    );
    assert!(
        !temp.state_home().exists(),
        "collision refusal created installer state"
    );
    assert!(
        !temp.0.join("omp-invocations.log").exists(),
        "PATH collision preflight invoked OMP before refusing the shell alias"
    );
}

#[cfg(unix)]
#[test]
fn installer_alias_preflight_refuses_path_executable_during_dry_run() {
    let temp = TestDir::new();
    let path_dir = create_path_dir(&temp);
    let executable = add_path_executable(&path_dir, "teddy");
    let source = local_topology_source(&temp, "fixture: true\n", &[("payload.txt", b"payload")]);
    let agent_dir = temp.0.join("agent");

    let output =
        run_install_from_source_with_path(&temp, &agent_dir, &source, "teddy", true, &path_dir);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "dry-run unexpectedly succeeded despite PATH executable collision; target_exists={}, state_home_exists={}, registry_exists={}\nstdout:\n{}\nstderr:\n{}",
        agent_dir.exists(),
        temp.state_home().exists(),
        temp.state_home()
            .join("omp-topology/registry.json")
            .exists(),
        String::from_utf8_lossy(&output.stdout),
        stderr
    );
    assert!(stderr.contains("teddy"), "error omitted alias:\n{stderr}");
    assert!(
        stderr.contains(executable.to_str().expect("executable path is not UTF-8")),
        "error omitted full colliding executable path {}:\n{stderr}",
        executable.display()
    );
    assert_choose_another_alias_hint(&stderr);
    assert!(
        !agent_dir.exists(),
        "dry-run collision refusal created the install target"
    );
    assert!(
        !temp.state_home().exists(),
        "dry-run collision refusal created installer state"
    );
    assert!(
        !temp.0.join("omp-invocations.log").exists(),
        "PATH collision dry-run preflight invoked OMP before refusing the shell alias"
    );
}

#[cfg(unix)]
#[test]
fn installer_alias_preflight_allows_non_executable_path_file() {
    let temp = TestDir::new();
    let path_dir = create_path_dir(&temp);
    let same_named_file = path_dir.join("teddy");
    fs::write(&same_named_file, b"not executable").expect("failed to create non-executable file");
    let mut permissions = fs::metadata(&same_named_file)
        .expect("failed to inspect non-executable file")
        .permissions();
    permissions.set_mode(0o644);
    fs::set_permissions(&same_named_file, permissions)
        .expect("failed to mark PATH fixture non-executable");
    let source = local_topology_source(&temp, "fixture: true\n", &[("payload.txt", b"payload")]);
    let agent_dir = temp.0.join("agent");

    let output =
        run_install_from_source_with_path(&temp, &agent_dir, &source, "teddy", false, &path_dir);
    assert_success(&output);
    assert_eq!(
        fs::read(agent_dir.join("payload.txt")).expect("install did not write its payload"),
        b"payload"
    );
    let registry: serde_json::Value = serde_json::from_slice(
        &fs::read(temp.state_home().join("omp-topology/registry.json"))
            .expect("successful install did not register its profile install ID"),
    )
    .expect("registry is not valid JSON");
    assert!(
        registry["aliases"]["path-preflight"].is_object(),
        "successful install did not register its profile install ID"
    );
}

#[cfg(unix)]
#[test]
fn installer_alias_preflight_allows_distinct_alias_when_other_executable_exists() {
    let temp = TestDir::new();
    let path_dir = create_path_dir(&temp);
    add_path_executable(&path_dir, "teddy");
    let source = local_topology_source(&temp, "fixture: true\n", &[("payload.txt", b"payload")]);
    let agent_dir = temp.0.join("agent");

    let output = run_install_from_source_with_path(
        &temp,
        &agent_dir,
        &source,
        "omp-teddy",
        false,
        &path_dir,
    );
    assert_success(&output);
    assert_eq!(
        fs::read(agent_dir.join("payload.txt")).expect("install did not write its payload"),
        b"payload"
    );
    let registry: serde_json::Value = serde_json::from_slice(
        &fs::read(temp.state_home().join("omp-topology/registry.json"))
            .expect("successful install did not register its profile install ID"),
    )
    .expect("registry is not valid JSON");
    assert!(
        registry["aliases"]["path-preflight"].is_object(),
        "successful install did not register the profile install ID"
    );
    assert!(
        registry["aliases"]["teddy"].is_null(),
        "install registered the unrelated executable name instead of the profile install ID"
    );
}

#[cfg(unix)]
#[test]
fn installer_same_install_id_upsert_allows_alias_matching_path_executable() {
    let temp = TestDir::new();
    let path_dir = create_path_dir(&temp);
    let profile = "path-install-id";
    let agent_dir = temp
        .0
        .join("home/.omp/profiles")
        .join(profile)
        .join("agent");
    let source = local_topology_source(&temp, "fixture: true\n", &[("payload.txt", b"payload")]);
    let source = source.to_str().expect("source path is not UTF-8");
    let omp_log = temp.0.join("omp-invocations.log");
    let alias_marker = temp.0.join("omp-alias-created");
    let fake_omp = path_dir.join("omp");
    fs::write(
        &fake_omp,
        "#!/bin/sh\n\
         printf '%s\\n' \"$*\" >> \"$OMP_TEST_OMP_LOG\"\n\
         if [ \"$#\" -eq 4 ] && [ \"$1\" = \"--profile\" ] && [ \"$2\" = \"$OMP_TEST_PROFILE\" ] && [ \"$3\" = \"config\" ] && [ \"$4\" = \"path\" ]; then\n\
         \tprintf '%s\\n' \"$OMP_TEST_PROFILE_AGENT_DIR\"\n\
         \texit 0\n\
         fi\n\
         if [ \"$#\" -eq 4 ] && [ \"$1\" = \"--profile\" ] && [ \"$2\" = \"$OMP_TEST_PROFILE\" ] && [ \"$3\" = \"--alias\" ]; then\n\
         \t: > \"$OMP_TEST_ALIAS_MARKER\"\n\
         \texit 0\n\
         fi\n\
         exit 64\n",
    )
    .expect("failed to write fake omp");
    fs::set_permissions(&fake_omp, fs::Permissions::from_mode(0o755))
        .expect("failed to make fake omp executable");

    let first = cli_command(&temp, Path::new(env!("CARGO_MANIFEST_DIR")))
        .args([
            "install",
            "-t",
            TOPOLOGY,
            "--source",
            source,
            "--profile",
            profile,
            "--install-id",
            "teddy",
        ])
        .env("PATH", &path_dir)
        .env("OMP_TEST_PROFILE", profile)
        .env("OMP_TEST_PROFILE_AGENT_DIR", &agent_dir)
        .env("OMP_TEST_OMP_LOG", &omp_log)
        .env("OMP_TEST_ALIAS_MARKER", &alias_marker)
        .output()
        .expect("failed to create the existing install ID");
    assert_success(&first);
    let target_before = file_snapshot(&agent_dir);
    let state_before = file_snapshot(&temp.state_home());
    add_path_executable(&path_dir, "teddy");

    let second = cli_command(&temp, Path::new(env!("CARGO_MANIFEST_DIR")))
        .args([
            "install",
            "-t",
            TOPOLOGY,
            "--source",
            source,
            "--profile",
            profile,
            "--install-id",
            "teddy",
            "--alias",
            "teddy",
            "--dry-run",
        ])
        .env("PATH", &path_dir)
        .env("OMP_TEST_PROFILE", profile)
        .env("OMP_TEST_PROFILE_AGENT_DIR", &agent_dir)
        .env("OMP_TEST_OMP_LOG", &omp_log)
        .env("OMP_TEST_ALIAS_MARKER", &alias_marker)
        .output()
        .expect("failed to dry-run the same-install-ID upsert");
    assert_success(&second);
    assert_eq!(
        file_snapshot(&agent_dir),
        target_before,
        "dry-run re-upsert changed target files"
    );
    assert_eq!(
        file_snapshot(&temp.state_home()),
        state_before,
        "dry-run re-upsert changed installer state"
    );
    let invocations = fs::read_to_string(&omp_log).expect("OMP invocation log is missing");
    let expected_config_path = format!("--profile {profile} config path");
    assert!(
        !invocations.is_empty()
            && invocations
                .lines()
                .all(|invocation| invocation == expected_config_path),
        "re-upsert invoked OMP shell-alias creation or another unexpected command:\n{invocations}"
    );
    assert!(
        !alias_marker.exists(),
        "dry-run re-upsert created the shell alias"
    );
}
#[cfg(unix)]
#[test]
fn installer_install_id_collision_refuses_another_target_without_mutation() {
    let temp = TestDir::new();
    let source = local_topology_source(&temp, "fixture: true\n", &[("payload.txt", b"payload")]);
    let existing_agent_dir = temp.0.join("existing-agent");
    let requested_agent_dir = temp.0.join("requested-agent");
    let install_id = "registered-teddy";

    let initial = install_from_source(
        &temp,
        &existing_agent_dir,
        &source,
        &["--install-id", install_id],
    );
    assert_success(&initial);
    let registry_path = temp.state_home().join("omp-topology/registry.json");
    let registry_before = fs::read(&registry_path).expect("initial install omitted registry");
    let state_before = file_snapshot(&temp.state_home());
    let target_before = file_snapshot(&existing_agent_dir);

    let output = install_from_source(
        &temp,
        &requested_agent_dir,
        &source,
        &["--install-id", install_id],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "install reused an ID already mapped to another target\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        stderr
    );
    assert!(
        stderr.contains(install_id) && stderr.to_ascii_lowercase().contains("already registered"),
        "registry collision error did not identify the install ID:\n{stderr}"
    );
    assert_eq!(
        fs::read(&registry_path).expect("registry disappeared after collision refusal"),
        registry_before,
        "registry collision refusal changed the registry"
    );
    assert_eq!(
        file_snapshot(&temp.state_home()),
        state_before,
        "registry collision refusal changed installer state"
    );
    assert_eq!(
        file_snapshot(&existing_agent_dir),
        target_before,
        "registry collision refusal changed the existing target"
    );
    assert!(
        !requested_agent_dir.exists(),
        "registry collision refusal created the requested target"
    );
    let lower = stderr.to_ascii_lowercase();
    assert!(
        (lower.contains("update") && lower.contains("existing install"))
            || lower.contains("choose another")
            || lower.contains("choose a different")
            || lower.contains("use another")
            || lower.contains("use a different"),
        "registry collision error did not direct the user to update the existing install or choose another:\n{stderr}"
    );
}
#[cfg(unix)]
#[test]
fn installer_alias_preflight_refuses_before_named_profile_resolution() {
    let temp = TestDir::new();
    let home = temp.0.join("home");
    let profile = "new-profile";
    let omp_root = home.join(".omp");
    let profiles_root = omp_root.join("profiles");
    let agent_dir = profiles_root.join(profile).join("agent");
    let path_dir = create_path_dir(&temp);
    let executable = add_path_executable(&path_dir, "teddy");
    let fake_omp = path_dir.join("omp");
    let omp_log = temp.0.join("omp-invocations.log");
    fs::write(
        &fake_omp,
        "#!/bin/sh\n\
         printf '%s\\n' \"$*\" >> \"$OMP_TEST_OMP_LOG\"\n\
         if [ \"$1\" = \"--profile\" ] && [ \"$2\" = \"$OMP_TEST_PROFILE\" ] && [ \"$3\" = \"config\" ] && [ \"$4\" = \"path\" ]; then\n\
         \tmkdir -p \"$OMP_TEST_AGENT_DIR\"\n\
         \tprintf '%s\\n' \"$OMP_TEST_AGENT_DIR\"\n\
         \texit 0\n\
         fi\n\
         exit 64\n",
    )
    .expect("failed to write fake omp");
    let mut permissions = fs::metadata(&fake_omp)
        .expect("failed to inspect fake omp")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_omp, permissions).expect("failed to make fake omp executable");
    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(path_dir).chain(std::env::split_paths(&inherited_path)),
    )
    .expect("failed to build isolated PATH");
    let source = local_topology_source(&temp, "fixture: true\n", &[("payload.txt", b"payload")]);
    let source = source.to_str().expect("local source path is not UTF-8");

    let output = cli_command(&temp, Path::new(env!("CARGO_MANIFEST_DIR")))
        .args([
            "install",
            "-t",
            TOPOLOGY,
            "--source",
            source,
            "--profile",
            profile,
            "--alias",
            "teddy",
        ])
        .env("PATH", path)
        .env("OMP_TEST_PROFILE", profile)
        .env("OMP_TEST_AGENT_DIR", &agent_dir)
        .env("OMP_TEST_OMP_LOG", &omp_log)
        .output()
        .expect("failed to execute installer with named profile");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let omp_invoked = omp_log.exists();
    let omp_log_contents = fs::read_to_string(&omp_log).unwrap_or_default();
    let state_home = temp.state_home();
    let registry_path = state_home.join("omp-topology/registry.json");
    assert!(
        !output.status.success(),
        "install unexpectedly proceeded despite PATH executable collision; omp_invoked={omp_invoked}, omp_log={omp_log_contents:?}, profile_root_exists={}, target_exists={}, state_home_exists={}, registry_exists={}\nstdout:\n{}\nstderr:\n{}",
        profiles_root.exists(),
        agent_dir.exists(),
        state_home.exists(),
        registry_path.exists(),
        String::from_utf8_lossy(&output.stdout),
        stderr
    );
    assert!(
        stderr.contains("teddy"),
        "collision error did not name alias 'teddy':\n{stderr}"
    );
    assert!(
        stderr.contains(executable.to_str().expect("executable path is not UTF-8")),
        "collision error did not include full executable path {}:\n{stderr}",
        executable.display()
    );
    assert_choose_another_alias_hint(&stderr);
    let omp_root_exists = omp_root.exists();
    let profiles_root_exists = profiles_root.exists();
    let target_exists = agent_dir.exists();
    let state_home_exists = state_home.exists();
    let registry_exists = registry_path.exists();
    assert!(
        !omp_invoked
            && !omp_root_exists
            && !profiles_root_exists
            && !target_exists
            && !state_home_exists
            && !registry_exists,
        "collision preflight had side effects: omp_invoked={omp_invoked}, omp_log={omp_log_contents:?}, omp_root_exists={omp_root_exists}, profiles_root_exists={profiles_root_exists}, target_exists={target_exists}, state_home_exists={state_home_exists}, registry_exists={registry_exists}"
    );
}

#[cfg(unix)]
#[test]
fn install_alias_invokes_omp_after_registering_topology() {
    let temp = TestDir::new();
    let profile = "demo";
    let agent_dir = temp
        .0
        .join("home/.omp/profiles")
        .join(profile)
        .join("agent");
    let source = local_named_topology_source(
        &temp,
        "source",
        "alpha",
        "fixture: true\n",
        &[("payload.txt", b"installed alpha")],
    );
    let source = source.to_str().expect("local source path is not UTF-8");
    let bin_dir = temp.0.join("fake-omp-bin");
    fs::create_dir_all(&bin_dir).expect("failed to create fake PATH");
    let fake_omp = bin_dir.join("omp");
    let invocation_log = temp.0.join("omp-invocations.log");
    let alias_marker = temp.0.join("omp-alias-created");
    fs::write(
        &fake_omp,
        "#!/bin/sh\n\
         printf 'call' >> \"$OMP_TEST_OMP_LOG\"\n\
         for arg in \"$@\"; do printf '\\t<%s>' \"$arg\" >> \"$OMP_TEST_OMP_LOG\"; done\n\
         printf '\\n' >> \"$OMP_TEST_OMP_LOG\"\n\
         if [ \"$#\" -eq 4 ] && [ \"$1\" = \"--profile\" ] && [ \"$2\" = \"$OMP_TEST_PROFILE\" ] && [ \"$3\" = \"config\" ] && [ \"$4\" = \"path\" ]; then\n\
         \tprintf '%s\\n' \"$OMP_TEST_PROFILE_AGENT_DIR\"\n\
         \texit 0\n\
         fi\n\
         if [ \"$#\" -eq 4 ] && [ \"$1\" = \"--profile\" ] && [ \"$2\" = \"$OMP_TEST_PROFILE\" ] && [ \"$3\" = \"--alias\" ] && [ \"$4\" = \"omp-demo\" ]; then\n\
         \t[ -f \"$OMP_TEST_INSTALLED_FILE\" ] || exit 65\n\
         \t: > \"$OMP_TEST_ALIAS_MARKER\"\n\
         \texit 0\n\
         fi\n\
         exit 64\n",
    )
    .expect("failed to write fake omp");
    fs::set_permissions(&fake_omp, fs::Permissions::from_mode(0o755))
        .expect("failed to make fake omp executable");
    let output = cli_command(&temp, Path::new(env!("CARGO_MANIFEST_DIR")))
        .args([
            "install",
            "--topology",
            "alpha",
            "--source",
            source,
            "--profile",
            profile,
            "--alias",
            "omp-demo",
        ])
        .env("PATH", &bin_dir)
        .env("OMP_TEST_PROFILE", profile)
        .env("OMP_TEST_PROFILE_AGENT_DIR", &agent_dir)
        .env("OMP_TEST_INSTALLED_FILE", agent_dir.join("payload.txt"))
        .env("OMP_TEST_ALIAS_MARKER", &alias_marker)
        .env("OMP_TEST_OMP_LOG", &invocation_log)
        .output()
        .expect("failed to execute installer with named profile");
    assert_success(&output);
    assert_eq!(
        fs::read(agent_dir.join("payload.txt")).expect("topology payload was not installed"),
        b"installed alpha"
    );

    let registry_path = temp.state_home().join("omp-topology/registry.json");
    let registry: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("registry is missing"))
            .expect("registry is not valid JSON");
    assert_eq!(
        registry["aliases"]["demo"]["topology"], "alpha",
        "registry did not retain the install under its profile-derived install ID"
    );
    assert_eq!(
        registry["aliases"]["demo"]["agent_dir"],
        fs::canonicalize(&agent_dir)
            .expect("installed profile target is missing")
            .display()
            .to_string(),
        "registry did not point to the selected profile target"
    );

    let invocations = fs::read_to_string(&invocation_log)
        .expect("fake OMP was not invoked")
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(
        invocations,
        [
            "call\t<--profile>\t<demo>\t<config>\t<path>",
            "call\t<--profile>\t<demo>\t<--alias>\t<omp-demo>",
        ],
        "OMP argv/order did not resolve the profile and then create its shell alias"
    );
    assert!(
        alias_marker.is_file(),
        "fake OMP did not create its marker for the alias invocation"
    );
}

#[cfg(unix)]
#[test]
fn install_id_and_shell_alias_have_independent_roles() {
    let temp = TestDir::new();
    let profile = "demo";
    let agent_dir = temp
        .0
        .join("home/.omp/profiles")
        .join(profile)
        .join("agent");
    let source = local_named_topology_source(
        &temp,
        "source",
        "alpha",
        "fixture: true\n",
        &[("payload.txt", b"installed alpha")],
    );
    let source = source.to_str().expect("local source path is not UTF-8");
    let bin_dir = temp.0.join("fake-omp-bin");
    fs::create_dir_all(&bin_dir).expect("failed to create fake PATH");
    let fake_omp = bin_dir.join("omp");
    let invocation_log = temp.0.join("omp-invocations.log");
    let alias_marker = temp.0.join("omp-alias-created");
    fs::write(
        &fake_omp,
        "#!/bin/sh\n\
         printf 'call' >> \"$OMP_TEST_OMP_LOG\"\n\
         for arg in \"$@\"; do printf '\\t<%s>' \"$arg\" >> \"$OMP_TEST_OMP_LOG\"; done\n\
         printf '\\n' >> \"$OMP_TEST_OMP_LOG\"\n\
         if [ \"$#\" -eq 4 ] && [ \"$1\" = \"--profile\" ] && [ \"$2\" = \"$OMP_TEST_PROFILE\" ] && [ \"$3\" = \"config\" ] && [ \"$4\" = \"path\" ]; then\n\
         \tprintf '%s\\n' \"$OMP_TEST_PROFILE_AGENT_DIR\"\n\
         \texit 0\n\
         fi\n\
         if [ \"$#\" -eq 4 ] && [ \"$1\" = \"--profile\" ] && [ \"$2\" = \"$OMP_TEST_PROFILE\" ] && [ \"$3\" = \"--alias\" ] && [ \"$4\" = \"omp-demo\" ]; then\n\
         \t[ -f \"$OMP_TEST_INSTALLED_FILE\" ] || exit 65\n\
         \t: > \"$OMP_TEST_ALIAS_MARKER\"\n\
         \texit 0\n\
         fi\n\
         exit 64\n",
    )
    .expect("failed to write fake omp");
    fs::set_permissions(&fake_omp, fs::Permissions::from_mode(0o755))
        .expect("failed to make fake omp executable");
    let output = cli_command(&temp, Path::new(env!("CARGO_MANIFEST_DIR")))
        .args([
            "install",
            "--topology",
            "alpha",
            "--source",
            source,
            "--profile",
            profile,
            "--install-id",
            "internal-demo",
            "--alias",
            "omp-demo",
        ])
        .env("PATH", &bin_dir)
        .env("OMP_TEST_PROFILE", profile)
        .env("OMP_TEST_PROFILE_AGENT_DIR", &agent_dir)
        .env("OMP_TEST_INSTALLED_FILE", agent_dir.join("payload.txt"))
        .env("OMP_TEST_ALIAS_MARKER", &alias_marker)
        .env("OMP_TEST_OMP_LOG", &invocation_log)
        .output()
        .expect("failed to execute installer with named profile");
    assert_success(&output);
    assert_eq!(
        fs::read(agent_dir.join("payload.txt")).expect("topology payload was not installed"),
        b"installed alpha"
    );

    let registry_path = temp.state_home().join("omp-topology/registry.json");
    let registry: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("registry is missing"))
            .expect("registry is not valid JSON");
    assert!(
        registry["aliases"]["internal-demo"].is_object(),
        "registry did not retain the install under its install ID"
    );
    assert!(
        registry["aliases"].get("omp-demo").is_none(),
        "shell alias was incorrectly used as a registry key"
    );

    let invocations = fs::read_to_string(&invocation_log)
        .expect("fake OMP was not invoked")
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(
        invocations,
        [
            "call\t<--profile>\t<demo>\t<config>\t<path>",
            "call\t<--profile>\t<demo>\t<--alias>\t<omp-demo>",
        ],
        "OMP argv/order did not resolve the profile and then create its shell alias"
    );
    assert!(
        alias_marker.is_file(),
        "fake OMP did not create its marker for the alias invocation"
    );
    let human_output = output_text(&output);
    assert!(
        human_output.contains("recorded install ID 'internal-demo'"),
        "human output did not identify the recorded install ID:\n{human_output}"
    );
    assert!(
        !human_output.contains("recorded alias 'omp-demo'"),
        "human output identified the shell alias as the recorded install ID:\n{human_output}"
    );
}

#[test]
fn installer_regression_dry_run_file_update_tracks_manifest() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    let source = local_topology_source(
        &temp,
        "task:\n  eager: always\n",
        &[("rules.md", b"source v1\n")],
    );
    let installed = install_from_source(&temp, &agent_dir, &source, &[]);
    assert_success(&installed);
    let manifest = manifest_path(&temp, &agent_dir);
    let manifest_before = fs::read(&manifest).expect("ownership manifest is missing");
    let target_file = agent_dir.join("rules.md");
    assert_eq!(fs::read(&target_file).unwrap(), b"source v1\n");

    fs::write(
        source
            .join("topologies")
            .join(TOPOLOGY)
            .join("agent/rules.md"),
        b"source v2\n",
    )
    .expect("failed to update local topology source file");
    let dry_run = install_from_source(&temp, &agent_dir, &source, &["--dry-run"]);
    assert_success(&dry_run);
    let report = output_text(&dry_run);
    assert!(
        report.contains("would write: rules.md"),
        "dry-run omitted the file-only source update:\n{report}"
    );
    assert!(
        report.contains("would update: ownership manifest"),
        "dry-run omitted the manifest change for the file-only update:\n{report}"
    );
    assert_eq!(
        fs::read(&target_file).unwrap(),
        b"source v1\n",
        "dry-run changed the installed file"
    );
    assert_eq!(
        fs::read(&manifest).unwrap(),
        manifest_before,
        "dry-run changed the ownership manifest"
    );
}

#[test]
fn installer_regression_relative_target_path_installs() {
    let temp = TestDir::new();
    let cwd = temp.0.join("caller");
    fs::create_dir_all(&cwd).expect("failed to create caller working directory");
    let relative_agent = Path::new("relative-agent");
    let source = local_topology_source(
        &temp,
        "task:\n  eager: always\n",
        &[("relative.md", b"relative target fixture\n")],
    );
    let source_arg = source.to_str().expect("local source path is not UTF-8");
    let output = run_cli_at(
        &temp,
        relative_agent,
        &cwd,
        &["install", "-t", TOPOLOGY, "--source", source_arg],
    );
    assert_success(&output);
    assert!(
        cwd.join(relative_agent).join("relative.md").is_file(),
        "install did not resolve the relative target from its working directory"
    );
}

#[test]
fn installer_regression_boolean_value_survives_install_remove() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create config target");
    let source = local_topology_source(&temp, "task:\n  eager: always\n", &[]);
    let config = agent_dir.join("config.yml");
    fs::write(&config, "task:\n  eager: TRUE\n").expect("failed to seed config");

    let installed = install_from_source(&temp, &agent_dir, &source, &["--config-merge"]);
    assert_success(&installed);

    let removed = run_cli(&temp, &agent_dir, &["remove", "-t", TOPOLOGY]);
    assert_success(&removed);

    let restored = fs::read_to_string(&config).expect("remove deleted config.yml");
    assert!(
        restored.lines().any(|line| line.trim() == "eager: true"),
        "preexisting TRUE was not restored as YAML true:\n{restored}"
    );
}

#[test]
fn installer_regression_numeric_nested_mapping_refuses_ambiguous_key() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    let source = local_topology_source(
        &temp,
        "task:\n  \"123\":\n    owned: true\n",
        &[("RULES.md", b"fixture source file\n")],
    );
    fs::create_dir_all(&agent_dir).expect("failed to create config target");
    let config = agent_dir.join("config.yml");
    let original = b"task:\n  123:\n    local: retained\n";
    fs::write(&config, original).expect("failed to seed numeric-key config");

    let output = install_from_source(&temp, &agent_dir, &source, &["--config-merge"]);
    assert!(
        !output.status.success(),
        "installer guessed that a numeric key matched the topology's string key"
    );
    let report = output_text(&output);
    assert!(
        report.contains("could not be matched exactly"),
        "installer did not report the ambiguous numeric-key refusal:\n{report}"
    );
    assert_eq!(
        fs::read(&config).expect("numeric-key config disappeared"),
        original,
        "installer mutated config after an ambiguous numeric-key match"
    );
    assert!(
        !agent_dir.join("RULES.md").exists(),
        "installer wrote topology files before refusing the ambiguous key"
    );
    assert!(
        !manifest_path(&temp, &agent_dir).exists(),
        "installer created ownership state before refusing the ambiguous key"
    );
}
#[cfg(unix)]
#[test]
fn installer_regression_rollback_restores_dropped_file_mode() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    let skills_dir = temp.0.join("skills");
    let source = local_topology_source(
        &temp,
        "task:\n  eager: always\n",
        &[("retired.md", b"retired source file\n")],
    );
    let skills_arg = skills_dir.to_str().expect("skills path is not UTF-8");
    let extra_args = ["--skills-dir", skills_arg];
    let installed = install_from_source(&temp, &agent_dir, &source, &extra_args);
    assert_success(&installed);

    let retired = agent_dir.join("retired.md");
    fs::set_permissions(&retired, fs::Permissions::from_mode(0o640))
        .expect("failed to set dropped file mode");
    let blocked_parent = skills_dir.join("blocked");
    fs::create_dir_all(&skills_dir).expect("failed to create skills root");
    fs::write(&blocked_parent, b"not a directory\n").expect("failed to block later skills write");
    let source_agent = source.join("topologies").join(TOPOLOGY).join("agent");
    fs::remove_file(source_agent.join("retired.md")).expect("failed to drop source file");
    let blocked_skill = source_agent.join("skills/blocked/child.md");
    fs::create_dir_all(blocked_skill.parent().unwrap())
        .expect("failed to create source skill parent");
    fs::write(&blocked_skill, b"skill source\n").expect("failed to write source skill");

    let failed_update = install_from_source(&temp, &agent_dir, &source, &extra_args);
    assert!(
        !failed_update.status.success(),
        "install unexpectedly succeeded despite an uncreatable skills target"
    );
    let report = output_text(&failed_update);
    assert!(
        report.contains("install failed and changes were rolled back"),
        "install did not fail after beginning its transaction:\n{report}"
    );
    assert_eq!(
        fs::read(&retired).expect("rollback did not restore dropped file"),
        b"retired source file\n",
        "rollback did not restore dropped file bytes"
    );
    assert_eq!(
        fs::metadata(&retired)
            .expect("rollback did not restore dropped file")
            .permissions()
            .mode()
            & 0o7777,
        0o640,
        "rollback did not restore the dropped file's original mode"
    );
}

#[test]
fn installer_regression_empty_maps_survive_config_merge() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    let source = local_topology_source(&temp, "task:\n  eager: always\nsourceEmpty: {}\n", &[]);
    fs::create_dir_all(&agent_dir).expect("failed to create config target");
    let config = agent_dir.join("config.yml");
    fs::write(&config, "localEmpty: {}\ntask:\n  eager: always\n")
        .expect("failed to seed config with an empty map");

    let output = install_from_source(&temp, &agent_dir, &source, &["--config-merge"]);
    assert_success(&output);
    let merged = fs::read_to_string(config).expect("merged config disappeared");
    for key in ["localEmpty: {}", "sourceEmpty: {}"] {
        assert!(
            merged.contains(key),
            "config merge dropped empty mapping {key:?}:\n{merged}"
        );
    }
}

#[test]
fn installer_regression_exact_empty_map_preserves_live_entries() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    let source = local_topology_source(&temp, "foo: {}\n", &[]);
    fs::create_dir_all(&agent_dir).expect("failed to create config target");
    let config = agent_dir.join("config.yml");
    fs::write(&config, "foo:\n  bar: baz\n").expect("failed to seed live config");

    let output = install_from_source(&temp, &agent_dir, &source, &["--config-merge"]);
    assert_success(&output);
    let merged = fs::read_to_string(config).expect("merged config disappeared");
    assert!(
        merged.contains("foo:\n  bar: baz"),
        "topology empty map replaced the live nonempty map:\n{merged}"
    );
}

#[test]
fn installer_regression_exact_numeric_key_collision() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    let source = local_topology_source(&temp, "foo:\n  1: true\n", &[]);
    fs::create_dir_all(&agent_dir).expect("failed to create config target");
    let config = agent_dir.join("config.yml");
    let original = b"foo:\n  1: false\n";
    fs::write(&config, original).expect("failed to seed numeric-key config");

    let output = install_from_source(&temp, &agent_dir, &source, &["--config-merge"]);
    if !output.status.success() {
        assert_eq!(
            fs::read(&config).expect("numeric-key config disappeared"),
            original,
            "installer mutated config while refusing the numeric-key merge"
        );
        return;
    }

    let merged = fs::read_to_string(config).expect("merged config disappeared");
    let keys: Vec<_> = merged
        .lines()
        .filter_map(|line| {
            let key = line.trim().split_once(':')?.0.trim();
            matches!(key, "1" | "\"1\"" | "'1'").then_some(key)
        })
        .collect();
    assert_eq!(
        keys,
        ["1"],
        "numeric key was changed to a string or emitted as a colliding duplicate:\n{merged}"
    );
}

#[test]
fn installer_regression_numeric_keys_are_preserved_during_merge() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    let source = local_topology_source(&temp, "task:\n  eager: always\n", &[]);
    fs::create_dir_all(&agent_dir).expect("failed to create config target");
    let config = agent_dir.join("config.yml");
    fs::write(
        &config,
        "task:\n  eager: always\nlocal:\n  123: preserved\n",
    )
    .expect("failed to seed config with a numeric key");

    let output = install_from_source(&temp, &agent_dir, &source, &["--config-merge"]);
    assert_success(&output);
    let merged = fs::read_to_string(config).expect("merged config disappeared");
    assert!(
        merged.contains("  123: preserved"),
        "config merge dropped or changed the numeric-keyed user entry:\n{merged}"
    );
}

#[test]
fn installer_regression_explicit_null_root_accepts_topology_merge() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    let source = local_topology_source(&temp, "task:\n  eager: always\n", &[]);
    fs::create_dir_all(&agent_dir).expect("failed to create config target");
    let config = agent_dir.join("config.yml");
    fs::write(&config, "null\n").expect("failed to seed explicit-null config");

    let output = install_from_source(&temp, &agent_dir, &source, &["--config-merge"]);
    assert_success(&output);
    let merged = fs::read_to_string(config).expect("merged config disappeared");
    assert!(
        merged.contains("task:") && merged.contains("eager: always"),
        "explicit-null config root was not replaced with the topology mapping:\n{merged}"
    );
}

#[test]
fn installer_regression_tagged_mapping_survives_config_merge() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    let source = local_topology_source(&temp, "task:\n  eager: always\n  enabled: true\n", &[]);
    fs::create_dir_all(&agent_dir).expect("failed to create config target");
    let config = agent_dir.join("config.yml");
    fs::write(
        &config,
        "task: !custom { eager: always, local: retained }\n",
    )
    .expect("failed to seed tagged mapping config");

    let output = install_from_source(&temp, &agent_dir, &source, &["--config-merge"]);
    assert_success(&output);
    let merged = fs::read_to_string(config).expect("merged config disappeared");
    for expected in ["!custom", "local: retained", "enabled: true"] {
        assert!(
            merged.contains(expected),
            "config merge did not preserve tagged mapping content {expected:?}:\n{merged}"
        );
    }
}

fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "CLI failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn files_under(root: &Path) -> Vec<PathBuf> {
    fn visit(directory: &Path, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(directory).expect("failed to read test directory") {
            let entry = entry.expect("failed to read test directory entry");
            let file_type = entry
                .file_type()
                .expect("failed to inspect test directory entry");
            if file_type.is_dir() {
                visit(&entry.path(), files);
            } else if file_type.is_file() {
                files.push(entry.path());
            }
        }
    }

    if !root.is_dir() {
        return Vec::new();
    }
    let mut files = Vec::new();
    visit(root, &mut files);
    files.sort();
    files
}
fn file_snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    files_under(root)
        .into_iter()
        .map(|path| {
            (
                path.strip_prefix(root)
                    .expect("file escaped snapshot root")
                    .to_path_buf(),
                fs::read(path).expect("failed to read snapshot file"),
            )
        })
        .collect()
}

fn state_snapshot_without_manifest(state_home: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let root = state_home.join("omp-topology");
    files_under(&root)
        .into_iter()
        .filter(|path| path.file_name().is_none_or(|name| name != "manifest.json"))
        .map(|path| {
            (
                path.strip_prefix(&root)
                    .expect("state file escaped isolated state directory")
                    .to_path_buf(),
                fs::read(path).expect("failed to read state snapshot"),
            )
        })
        .collect()
}

fn percent_encode_path(path: &Path) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let text = path.display().to_string();
    let mut encoded = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

fn manifest_path(temp: &TestDir, agent_dir: &Path) -> PathBuf {
    let resolved_agent = fs::canonicalize(agent_dir).expect("failed to resolve agent directory");
    temp.state_home()
        .join("omp-topology")
        .join(percent_encode_path(&resolved_agent))
        .join("manifest.json")
}

fn block_manifest_write(temp: &TestDir, agent_dir: &Path) {
    let manifest = manifest_path(temp, agent_dir);
    fs::create_dir_all(manifest.parent().expect("manifest has no parent"))
        .expect("failed to create isolated manifest directory");
    fs::create_dir(&manifest).expect("failed to block manifest file creation");
}

#[test]
fn install_merges_topology_config_preserving_user_content() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");

    let config = agent_dir.join("config.yml");
    fs::write(
        &config,
        r#"# local config comment
task:
  eager: never # keep this comment
  localNested:
    enabled: true
unrelated:
  keep: value

# tail comment stays
"#,
    )
    .expect("failed to seed config.yml");

    let output = run_cli(
        &temp,
        &agent_dir,
        &[
            "install",
            "-t",
            TOPOLOGY,
            "--config-merge",
            "--config-precedence=topology",
        ],
    );
    assert_success(&output);

    let merged = fs::read_to_string(&config).expect("failed to read merged config.yml");
    for preserved in [
        "# local config comment",
        "# keep this comment",
        "localNested:",
        "    enabled: true",
        "unrelated:",
        "  keep: value",
        "# tail comment stays",
    ] {
        assert!(
            merged.contains(preserved),
            "merged config omitted {preserved:?}:\n{merged}"
        );
    }
    for installed in [
        "  eager: always",
        "  autoParallel: true",
        "  maxConcurrency: 4",
        "  maxRecursionDepth: 2",
        "    enabled: true",
        "    merge: patch",
    ] {
        assert!(
            merged.contains(installed),
            "merged config omitted {installed:?}:\n{merged}"
        );
    }
    assert!(
        agent_dir.join("agents/marketing.md").is_file(),
        "install did not create agents/marketing.md"
    );
    let config_before_reinstall = fs::read(&config).expect("failed to reread merged config");
    let agent_file = agent_dir.join("agents/marketing.md");
    let agent_before_reinstall = fs::read(&agent_file).expect("installed agent file is missing");
    let manifest = manifest_path(&temp, &agent_dir);
    let manifest_before_reinstall = fs::read(&manifest).expect("ownership manifest is missing");
    let state_before_reinstall = state_snapshot_without_manifest(&temp.state_home());
    let reinstall = run_cli(
        &temp,
        &agent_dir,
        &[
            "install",
            "-t",
            TOPOLOGY,
            "--config-merge",
            "--config-precedence=topology",
        ],
    );
    assert_success(&reinstall);
    assert_eq!(
        fs::read(&config).expect("failed to reread merged config"),
        config_before_reinstall,
        "reinstall changed an existing merged config"
    );
    assert_eq!(
        fs::read(&agent_file).expect("installed agent file disappeared"),
        agent_before_reinstall,
        "reinstall changed owned agent bytes"
    );
    assert_eq!(
        fs::read(&manifest).expect("ownership manifest disappeared"),
        manifest_before_reinstall,
        "reinstall changed the ownership manifest"
    );
    assert_eq!(
        state_snapshot_without_manifest(&temp.state_home()),
        state_before_reinstall,
        "reinstall created new backup state for an unchanged config"
    );
}

#[test]
fn fresh_install_migrates_auto_parallel_and_reinstall_is_a_noop() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let config = agent_dir.join("config.yml");
    assert!(!config.exists(), "test requires an initially absent config");

    let first = run_cli(&temp, &agent_dir, &["install", "-t", TOPOLOGY]);
    assert_success(&first);
    let first_config = fs::read(&config).expect("fresh install did not write config.yml");
    let first_text = String::from_utf8_lossy(&first_config);
    assert!(
        first_text.contains("autoParallel: true"),
        "fresh config did not migrate autoParallel to true:\n{first_text}"
    );
    assert!(
        !first_text.contains("autoParallel: on"),
        "fresh config retained the YAML 1.1 spelling:\n{first_text}"
    );
    let install_snapshot = files_under(&temp.state_home().join("omp-topology"))
        .into_iter()
        .find(|path| path.file_name().is_some_and(|name| name == "snapshot.json"))
        .expect("fresh install omitted transaction snapshot");
    let snapshot_text =
        fs::read_to_string(&install_snapshot).expect("failed to read install snapshot");
    assert!(
        snapshot_text.contains("\"config.yml\"") && snapshot_text.contains("\"absent\": true"),
        "install snapshot did not record the initially absent config:\n{snapshot_text}"
    );
    assert!(
        install_snapshot
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name.to_string_lossy().starts_with("install-")),
        "install snapshot was not placed in a timestamped transaction directory"
    );
    let first_backups = state_snapshot_without_manifest(&temp.state_home());

    let second = run_cli(&temp, &agent_dir, &["install", "-t", TOPOLOGY]);
    assert_success(&second);
    assert_eq!(
        fs::read(&config).expect("failed to reread config.yml"),
        first_config,
        "second install changed config bytes"
    );
    assert_eq!(
        state_snapshot_without_manifest(&temp.state_home()),
        first_backups,
        "second install created or changed backup state"
    );
}

#[test]
fn failed_manifest_write_rolls_back_a_new_config_to_absent() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    block_manifest_write(&temp, &agent_dir);

    let output = run_cli(&temp, &agent_dir, &["install", "-t", TOPOLOGY]);
    assert!(
        !output.status.success(),
        "install unexpectedly succeeded despite blocked manifest write"
    );
    assert!(
        !agent_dir.join("config.yml").exists(),
        "rollback left a config that was absent before install"
    );
    assert!(
        !agent_dir.join("RULES.md").exists(),
        "rollback left an installed file behind"
    );
    assert!(
        !agent_dir.join("agents/marketing.md").exists(),
        "rollback left an installed agent file behind"
    );
}

#[cfg(unix)]
#[test]
fn failed_manifest_write_preserves_existing_config_mode_during_rollback() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let config = agent_dir.join("config.yml");
    let original = b"task:\n  eager: never\nlocal: preserved\n";
    fs::write(&config, original).expect("failed to seed config.yml");
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600))
        .expect("failed to set config mode");
    block_manifest_write(&temp, &agent_dir);

    let output = run_cli(
        &temp,
        &agent_dir,
        &["install", "-t", TOPOLOGY, "--config-merge"],
    );
    assert!(
        !output.status.success(),
        "install unexpectedly succeeded despite blocked manifest write"
    );
    assert_eq!(
        fs::read(&config).expect("rollback removed existing config"),
        original,
        "rollback did not restore the original config bytes"
    );
    assert_eq!(
        fs::metadata(&config)
            .expect("rollback removed existing config")
            .permissions()
            .mode()
            & 0o7777,
        0o600,
        "rollback did not preserve the original config mode"
    );
}

#[test]
fn malformed_manifest_hash_fails_closed_without_overwriting_an_edited_file() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let first = run_cli(&temp, &agent_dir, &["install", "-t", TOPOLOGY]);
    assert_success(&first);

    let owned_file = agent_dir.join("agents/marketing.md");
    let edit = b"user edit that must not be overwritten\n";
    fs::write(&owned_file, edit).expect("failed to edit owned file");
    let manifest = manifest_path(&temp, &agent_dir);
    let mut manifest_text =
        fs::read_to_string(&manifest).expect("failed to read ownership manifest");
    let entry = "\"agents/marketing.md\": \"";
    let value_start = manifest_text
        .find(entry)
        .expect("manifest omitted owned agent hash")
        + entry.len()
        - 1;
    let value_end = value_start
        + manifest_text[value_start + 1..]
            .find('"')
            .expect("manifest hash was not terminated")
        + 2;
    manifest_text.replace_range(value_start..value_end, "null");
    fs::write(&manifest, manifest_text).expect("failed to corrupt manifest hash");
    let config_before = fs::read(agent_dir.join("config.yml")).expect("missing installed config");

    let second = run_cli(
        &temp,
        &agent_dir,
        &["install", "-t", TOPOLOGY, "--config-merge"],
    );
    assert!(
        !second.status.success(),
        "install accepted a non-string ownership hash"
    );
    assert!(
        fs::read(&owned_file)
            .expect("edited owned file disappeared")
            .as_slice()
            == edit,
        "install overwrote the edited owned file"
    );
    assert_eq!(
        fs::read(agent_dir.join("config.yml")).expect("installed config disappeared"),
        config_before,
        "failed install mutated config"
    );
}

#[cfg(unix)]
#[test]
fn remove_does_not_prune_an_external_directory_through_a_symlinked_parent() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let installed = run_cli(&temp, &agent_dir, &["install", "-t", TOPOLOGY]);
    assert_success(&installed);

    let manifest = manifest_path(&temp, &agent_dir);
    let manifest_before = fs::read(&manifest).expect("ownership manifest is missing");
    let config = agent_dir.join("config.yml");
    let config_before = fs::read(&config).expect("installed config is missing");

    let skills_link = agent_dir.join("skills");
    fs::remove_dir_all(&skills_link).expect("failed to clear installed skills tree");
    let outside = temp.0.join("outside");
    let external_child = outside.join("marketing-charter");
    fs::create_dir_all(&external_child).expect("failed to create external directory");
    symlink(&outside, &skills_link).expect("failed to link agent skills path outside");

    let removed = run_cli(&temp, &agent_dir, &["remove", "-t", TOPOLOGY]);
    assert!(
        !removed.status.success(),
        "remove accepted a symlinked skills parent resolving outside ~/.omp:\n{}",
        output_text(&removed)
    );
    assert_eq!(
        fs::read(&manifest).expect("remove deleted the ownership manifest"),
        manifest_before,
        "remove changed the ownership manifest before rejecting the external link"
    );
    assert_eq!(
        fs::read(&config).expect("remove deleted the installed config"),
        config_before,
        "remove changed config before rejecting the external link"
    );
    assert!(
        skills_link.is_symlink(),
        "remove replaced the symlinked parent"
    );
    assert!(
        external_child.is_dir(),
        "remove pruned an external directory through the symlinked parent"
    );
}

#[test]
fn list_and_install_resolve_legacy_catalog_source_outside_the_caller_directory() {
    let temp = TestDir::new();

    let outside_cwd = temp.0.join("outside-cwd");
    let source = local_named_topology_source(
        &temp,
        "legacy-catalog",
        TOPOLOGY,
        "task:\n  eager: always\n  autoParallel: true\n",
        &[("catalog.md", b"legacy catalog payload\n")],
    );
    let decoy_topology = outside_cwd.join("topologies").join(TOPOLOGY);
    fs::create_dir_all(decoy_topology.join("agent")).expect("failed to create decoy topology");
    fs::write(
        decoy_topology.join("topology.yml"),
        "name: digital-product-studio\n\
         version: 9.9.9\n\
         description: unrelated current-directory topology\n\
         installable: true\n",
    )
    .expect("failed to seed decoy topology metadata");
    fs::write(decoy_topology.join("agent/config.yml"), "decoy: true\n")
        .expect("failed to seed decoy topology config");
    fs::write(
        decoy_topology.join("agent/decoy.md"),
        "wrong topology source\n",
    )
    .expect("failed to seed decoy topology file");
    let source_arg = source.to_str().expect("fixture source path is not UTF-8");

    let listing = cli_command(&temp, &outside_cwd)
        .args(["list", "--source", source_arg])
        .output()
        .expect("failed to execute omp-profile list");
    assert_success(&listing);
    let listing_text = String::from_utf8_lossy(&listing.stdout);
    assert!(
        listing_text.contains("digital-product-studio 1.0.0"),
        "list did not use the supplied legacy catalog:\n{listing_text}"
    );
    assert!(
        !listing_text.contains("9.9.9"),
        "list selected the unrelated current-directory topology:\n{listing_text}"
    );

    let agent_dir = temp.0.join("agent");
    let installed = run_cli_at(
        &temp,
        &agent_dir,
        &outside_cwd,
        &["install", "-t", TOPOLOGY, "--source", source_arg],
    );
    assert_success(&installed);
    assert!(
        agent_dir.join("catalog.md").is_file(),
        "install did not use the supplied legacy catalog files"
    );
    assert!(
        !agent_dir.join("decoy.md").exists(),
        "install used topology files from the caller's working directory"
    );
    let config = fs::read_to_string(agent_dir.join("config.yml"))
        .expect("install did not create config from the legacy catalog");
    assert!(
        config.contains("autoParallel: true"),
        "install did not use the supplied legacy catalog config:\n{config}"
    );
    assert!(
        !config.contains("decoy: true"),
        "install used topology config from the caller's working directory:\n{config}"
    );
}

#[test]
fn example_profile_package_lists_and_installs_from_nested_source() {
    let temp = TestDir::new();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("omp-profile-example/profile");
    let source_arg = source.to_str().expect("example source path is not UTF-8");

    let listing = run_cli_without_agent_dir(&temp, &["list", "--source", source_arg, "--json"]);
    assert_success(&listing);
    let listing: serde_json::Value =
        serde_json::from_slice(&listing.stdout).expect("list output is not valid JSON");
    let packages = listing["topologies"]
        .as_array()
        .expect("list output omitted the package array");
    assert_eq!(packages.len(), 1, "list should expose only the example package");
    assert_eq!(packages[0]["name"], "omp-profile-example");
    assert_eq!(packages[0]["version"], "1.0.0");
    assert_eq!(
        packages[0]["description"],
        "Minimal standalone OMP profile package used to demonstrate the installer."
    );

    let agent_dir = temp.0.join("fresh-agent");
    assert!(!agent_dir.exists(), "test target unexpectedly exists");
    let installed = run_cli(
        &temp,
        &agent_dir,
        &[
            "install",
            "-t",
            "omp-profile-example",
            "--source",
            source_arg,
        ],
    );
    assert_success(&installed);
    assert_eq!(
        fs::read_to_string(agent_dir.join("RULES.md"))
            .expect("install did not copy the example profile payload"),
        fs::read_to_string(source.join("agent/RULES.md"))
            .expect("example profile payload is missing")
    );
}

#[test]
fn list_and_install_resolve_root_level_topology_source() {
    let temp = TestDir::new();
    let source = temp.0.join("root-package");
    let agent = source.join("agent");
    fs::create_dir_all(&agent).expect("failed to create root-level package agent directory");
    fs::write(
        source.join("topology.yml"),
        "name: root-package-contract\n\
         version: 1.0.0\n\
         description: root-level source fixture\n\
         installable: true\n",
    )
    .expect("failed to write root-level package metadata");
    fs::write(agent.join("config.yml"), "rootPackageContract: true\n")
        .expect("failed to write root-level package config");
    fs::write(agent.join("contract.md"), "root-level package payload\n")
        .expect("failed to write root-level package payload");

    let source_arg = source.to_str().expect("local source path is not UTF-8");
    let listing = run_cli_without_agent_dir(
        &temp,
        &["list", "--source", source_arg, "--json"],
    );
    assert_success(&listing);
    let listing: serde_json::Value =
        serde_json::from_slice(&listing.stdout).expect("list output is not valid JSON");
    let packages = listing["topologies"]
        .as_array()
        .expect("list output omitted the package array");
    assert_eq!(packages.len(), 1, "list should expose only the root package");
    assert_eq!(packages[0]["name"], "root-package-contract");
    assert_eq!(packages[0]["version"], "1.0.0");

    let agent_dir = temp.0.join("installed-agent");
    let installed = run_cli(
        &temp,
        &agent_dir,
        &[
            "install",
            "-t",
            "root-package-contract",
            "--source",
            source_arg,
        ],
    );
    assert_success(&installed);
    assert_eq!(
        fs::read_to_string(agent_dir.join("contract.md"))
            .expect("install did not copy the root-level package payload"),
        "root-level package payload\n"
    );
    let config = fs::read_to_string(agent_dir.join("config.yml"))
        .expect("install did not create config from the root-level package");
    assert!(config.contains("rootPackageContract: true"), "{config}");
}

#[test]
fn list_and_install_without_source_require_explicit_source_before_mutation() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create target agent directory");
    fs::write(agent_dir.join("existing.md"), "preserve this file\n")
        .expect("failed to seed target agent directory");
    let target_before = file_snapshot(&agent_dir);
    let state_before = file_snapshot(&temp.state_home());

    for args in [&["list", "--json"][..], &["install", "-t", TOPOLOGY][..]] {
        let mut command = cli_command(&temp, Path::new(env!("CARGO_MANIFEST_DIR")));
        command.args(args);
        if args[0] == "install" {
            command.arg("--agent-dir").arg(&agent_dir);
        }
        let output = command.output().expect("failed to execute omp-profile");
        let text = output_text(&output);

        assert!(
            !output.status.success(),
            "{} without --source unexpectedly succeeded:\n{text}",
            args[0]
        );
        assert!(
            text.contains("--source"),
            "{} rejection did not explain how to provide --source:\n{text}",
            args[0]
        );
        assert_eq!(
            file_snapshot(&agent_dir),
            target_before,
            "{} without --source mutated the target directory",
            args[0]
        );
        assert_eq!(
            file_snapshot(&temp.state_home()),
            state_before,
            "{} without --source mutated installer state",
            args[0]
        );
    }
}

#[test]
fn dry_run_creates_no_target_or_state_files() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    assert!(!agent_dir.exists(), "test target unexpectedly exists");

    let output = run_cli(&temp, &agent_dir, &["install", "-t", TOPOLOGY, "--dry-run"]);
    assert_success(&output);
    assert!(
        !agent_dir.exists(),
        "dry-run created the target agent directory"
    );
    assert!(
        !temp.state_home().exists(),
        "dry-run created XDG state files or directories"
    );
}

#[test]
fn unowned_collision_refusal_does_not_partially_mutate_target() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(agent_dir.join("agents")).expect("failed to create collision directory");
    let config = agent_dir.join("config.yml");
    let original_config = b"local: unchanged\n";
    fs::write(&config, original_config).expect("failed to seed config.yml");
    let collision = agent_dir.join("agents/marketing.md");
    let collision_bytes = b"user-owned collision\n";
    fs::write(&collision, collision_bytes).expect("failed to seed unowned collision");

    let output = run_cli(
        &temp,
        &agent_dir,
        &["install", "-t", TOPOLOGY, "--config-merge"],
    );
    assert!(
        !output.status.success(),
        "install unexpectedly accepted an unowned file collision"
    );
    assert!(
        output_text(&output).to_lowercase().contains("collision")
            || output_text(&output).to_lowercase().contains("unowned"),
        "install failure did not identify the collision:\n{}",
        output_text(&output)
    );
    assert_eq!(
        fs::read(config).expect("config disappeared").as_slice(),
        original_config
    );
    assert_eq!(
        fs::read(collision)
            .expect("collision disappeared")
            .as_slice(),
        collision_bytes
    );
    assert!(!agent_dir.join("RULES.md").exists());
    assert!(!agent_dir.join("skills").exists());
    assert!(
        !temp.state_home().exists(),
        "collision refusal created state before mutation"
    );
}

#[test]
fn split_skills_root_is_recorded_and_used_for_removal() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    let skills_dir = temp.0.join("shared-skills");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");

    let installed =
        run_cli_with_skills(&temp, &agent_dir, &skills_dir, &["install", "-t", TOPOLOGY]);
    assert_success(&installed);
    let skill = skills_dir.join("marketing-charter/SKILL.md");
    assert!(
        skill.is_file(),
        "skill was not installed under the split root"
    );
    assert!(agent_dir.join("RULES.md").is_file());
    assert!(agent_dir.join("agents/marketing.md").is_file());
    assert!(
        !agent_dir.join("skills").exists(),
        "split-root install also wrote into the agent skills directory"
    );
    let manifest = fs::read_to_string(manifest_path(&temp, &agent_dir))
        .expect("split-root install did not write ownership manifest");
    assert!(
        manifest.contains(&skills_dir.display().to_string()),
        "manifest did not record the split skills root"
    );

    let removed = run_cli(&temp, &agent_dir, &["remove", "-t", TOPOLOGY]);
    assert_success(&removed);
    assert!(
        !skill.exists(),
        "remove ignored the skills root recorded in manifest"
    );
    assert!(!agent_dir.join("RULES.md").exists());
    assert!(!agent_dir.join("agents/marketing.md").exists());
    assert!(!agent_dir.join("skills").exists());
}

#[test]
fn install_dry_run_accepts_legacy_config_file_existed_boolean() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let config = agent_dir.join("config.yml");
    fs::write(&config, b"task:\n  eager: always\n").expect("failed to seed config.yml");

    let installed = run_cli(
        &temp,
        &agent_dir,
        &["install", "-t", TOPOLOGY, "--config-merge"],
    );
    assert_success(&installed);

    let manifest = manifest_path(&temp, &agent_dir);
    let ownership: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).expect("ownership manifest is missing"))
            .expect("ownership manifest is not valid JSON");
    assert_eq!(ownership["topology_version"], "1.0.0");
    assert_eq!(
        ownership["config"]["prior"]["[\"task\", \"eager\"]"]["value"], "always",
        "fixture lacks valid prior config ownership"
    );
    assert_eq!(
        ownership["config"]["installed"]["[\"task\", \"eager\"]"]["value"], "always",
        "fixture lacks valid installed config ownership"
    );

    let installed_config =
        fs::read_to_string(&config).expect("failed to read installed config.yml");
    assert!(
        installed_config.contains("eager: always"),
        "initial install did not merge the topology-owned key:\n{installed_config}"
    );
    let edited_config = installed_config.replacen("eager: always", "eager: locally-edited", 1);
    assert_ne!(
        edited_config, installed_config,
        "failed to create an edited config fixture"
    );
    fs::write(&config, edited_config.as_bytes()).expect("failed to edit config.yml");

    // Match the legacy single-root manifest shape while retaining the
    // complete current file ownership inventory from the setup install.
    let legacy_manifest = serde_json::json!({
        "version": 1,
        "topology": TOPOLOGY,
        "topology_version": "2.0.1",
        "files": ownership["files"].clone(),
        "created_dirs": ownership["created_dirs"].clone(),
        "config": {
            "file_existed": true,
            "prior": ownership["config"]["prior"].clone(),
            "installed": ownership["config"]["installed"].clone(),
        }
    });
    let legacy_manifest_bytes = serde_json::to_vec_pretty(&legacy_manifest)
        .expect("failed to serialize legacy ownership manifest");
    fs::write(&manifest, &legacy_manifest_bytes).expect("failed to write legacy manifest");

    let manifest_before = fs::read(&manifest).expect("failed to read legacy manifest");
    let agent_files_before: Vec<_> = files_under(&agent_dir)
        .into_iter()
        .map(|path| {
            let bytes = fs::read(&path).expect("failed to snapshot installed agent file");
            (path, bytes)
        })
        .collect();
    let candidate = agent_dir.join("config.yml.new");
    assert!(
        !candidate.exists(),
        "test fixture unexpectedly contains a config candidate"
    );

    let dry_run = run_cli(
        &temp,
        &agent_dir,
        &["install", "-t", TOPOLOGY, "--dry-run", "--config-merge"],
    );
    assert_success(&dry_run);
    assert_eq!(
        fs::read(&config).expect("config.yml disappeared during dry-run"),
        edited_config.as_bytes(),
        "dry-run changed the user-edited config"
    );
    assert!(!candidate.exists(), "dry-run published a config candidate");
    assert_eq!(
        fs::read(&manifest).expect("legacy manifest disappeared during dry-run"),
        manifest_before,
        "dry-run changed the legacy ownership manifest"
    );
    let agent_files_after: Vec<_> = files_under(&agent_dir)
        .into_iter()
        .map(|path| {
            let bytes = fs::read(&path).expect("failed to read installed agent file");
            (path, bytes)
        })
        .collect();
    assert_eq!(
        agent_files_after, agent_files_before,
        "dry-run changed an installed agent file"
    );
}

#[test]
fn remove_accepts_legacy_single_root_manifest() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    let skill = agent_dir.join("skills/marketing-charter/SKILL.md");
    let rules = agent_dir.join("RULES.md");
    let config = agent_dir.join("config.yml");
    fs::create_dir_all(skill.parent().expect("skill has no parent"))
        .expect("failed to create legacy skill directory");
    fs::write(&skill, b"").expect("failed to seed legacy skill");
    fs::write(&rules, b"").expect("failed to seed legacy rules");
    fs::write(&config, "task:\n  eager: always\n").expect("failed to seed legacy config");

    let manifest = manifest_path(&temp, &agent_dir);
    fs::create_dir_all(manifest.parent().expect("manifest has no parent"))
        .expect("failed to create legacy state directory");
    fs::write(
        &manifest,
        format!(
            r#"{{
  "version": 1,
  "topology": "{TOPOLOGY}",
  "topology_version": "1.0.0",
  "files": {{
    "RULES.md": "{EMPTY_SHA256}",
    "skills/marketing-charter/SKILL.md": "{EMPTY_SHA256}"
  }},
  "created_dirs": ["skills", "skills/marketing-charter"],
  "config": {{
    "prior": {{ "[\"task\",\"eager\"]": {{ "value": "never" }} }},
    "installed": {{ "[\"task\",\"eager\"]": {{ "value": "always" }} }}
  }}
}}
"#
        ),
    )
    .expect("failed to write legacy manifest");

    let removed = run_cli(&temp, &agent_dir, &["remove", "-t", TOPOLOGY]);
    assert_success(&removed);
    assert!(!skill.exists(), "legacy skill was not removed");
    assert!(!rules.exists(), "legacy rules file was not removed");
    let restored_config = fs::read_to_string(config).expect("legacy config was removed");
    assert!(
        restored_config.contains("eager: never"),
        "legacy manifest did not restore the prior config value:\n{restored_config}"
    );
    assert!(
        !manifest.exists(),
        "successful legacy removal left an ownership manifest"
    );
}

#[test]
fn remove_preserves_edited_files_and_config_until_retry() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let config = agent_dir.join("config.yml");
    fs::write(&config, "task:\n  eager: never\n").expect("failed to seed user config");
    let installed = run_cli(
        &temp,
        &agent_dir,
        &[
            "install",
            "-t",
            TOPOLOGY,
            "--config-merge",
            "--config-precedence=topology",
        ],
    );
    assert_success(&installed);

    let edited_file = agent_dir.join("agents/marketing.md");
    let installed_file_bytes = fs::read(&edited_file).expect("installed agent file is missing");
    let edited_file_bytes = b"user's later agent edit\n";
    fs::write(&edited_file, edited_file_bytes).expect("failed to edit installed agent file");
    let installed_config = fs::read_to_string(&config).expect("installed config is missing");
    assert!(
        installed_config.contains("eager: always"),
        "install did not set the config value needed for edit-preservation test:\n{installed_config}"
    );
    let edited_config = installed_config.replace("eager: always", "eager: false");
    assert_ne!(
        edited_config, installed_config,
        "config edit was not applied"
    );
    fs::write(&config, &edited_config).expect("failed to edit installed config");

    let first_remove = run_cli(&temp, &agent_dir, &["remove", "-t", TOPOLOGY]);
    assert_success(&first_remove);
    let report = output_text(&first_remove);
    assert!(report.contains("marketing.md"));
    assert!(report.contains("config.yml"));
    assert_eq!(
        fs::read(&edited_file).expect("remove deleted the edited agent file"),
        edited_file_bytes,
        "remove overwrote an edited owned file"
    );
    let partially_removed_config =
        fs::read_to_string(&config).expect("remove deleted the user config");
    assert!(
        partially_removed_config.contains("eager: false"),
        "remove overwrote the type-edited config value:\n{partially_removed_config}"
    );
    let manifest = manifest_path(&temp, &agent_dir);
    assert!(
        manifest.is_file(),
        "remove discarded ownership state needed to retry preserved edits"
    );

    fs::write(&edited_file, installed_file_bytes).expect("failed to restore owned file bytes");
    fs::write(
        &config,
        partially_removed_config.replace("eager: false", "eager: always"),
    )
    .expect("failed to restore installed config value");
    let retry = run_cli(&temp, &agent_dir, &["remove", "-t", TOPOLOGY]);
    assert_success(&retry);
    assert!(
        !edited_file.exists(),
        "retry did not remove the restored owned file"
    );
    assert!(
        fs::read_to_string(config)
            .expect("retry removed config.yml")
            .contains("eager: never"),
        "retry did not restore the original config value"
    );
    assert!(
        !manifest.exists(),
        "retry completed removal but left the ownership manifest"
    );
}
#[test]
fn install_and_remove_help_expose_target_skills_and_dry_run_flags() {
    let temp = TestDir::new();
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    for command_name in ["install", "remove"] {
        let output = cli_command(&temp, cwd)
            .args([command_name, "--help"])
            .output()
            .expect("failed to read subcommand help");
        assert_success(&output);
        let help = String::from_utf8_lossy(&output.stdout);
        for flag in ["--agent-dir", "--skills-dir", "--dry-run"] {
            assert!(
                help.contains(flag),
                "{command_name} help omitted {flag}:\n{help}"
            );
        }
    }
}

#[test]
fn install_rejects_an_identical_but_unowned_topology_file() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    let config = agent_dir.join("config.yml");
    let original_config = b"unrelated: unchanged\n";
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    fs::write(&config, original_config).expect("failed to seed config.yml");

    let source = default_legacy_source(&temp).join("topologies").join(TOPOLOGY)
        .join("agent/agents/marketing.md");
    let source_bytes = fs::read(source).expect("failed to read fixture topology source file");
    let collision = agent_dir.join("agents/marketing.md");
    fs::create_dir_all(collision.parent().expect("collision has no parent"))
        .expect("failed to create collision directory");
    fs::write(&collision, &source_bytes).expect("failed to seed identical unowned file");

    let output = run_cli(
        &temp,
        &agent_dir,
        &["install", "-t", TOPOLOGY, "--config-merge"],
    );
    assert!(
        !output.status.success(),
        "install accepted a byte-identical but unowned file"
    );
    assert!(
        output_text(&output).to_lowercase().contains("collision")
            || output_text(&output).to_lowercase().contains("unowned"),
        "install did not report the unowned file collision:\n{}",
        output_text(&output)
    );
    assert_eq!(fs::read(&config).unwrap().as_slice(), original_config);
    assert_eq!(fs::read(&collision).unwrap(), source_bytes);
    assert_eq!(files_under(collision.parent().unwrap()), vec![collision]);
    assert!(!temp.state_home().exists());
}

#[test]
fn install_rejects_config_scalar_type_conflicts_without_mutation() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let config = agent_dir.join("config.yml");
    let original = b"task: user-value\nunrelated: unchanged\n";
    fs::write(&config, original).expect("failed to seed scalar config");

    let output = run_cli(
        &temp,
        &agent_dir,
        &["install", "-t", TOPOLOGY, "--config-merge"],
    );
    assert!(
        !output.status.success(),
        "install accepted a scalar where the topology requires a map"
    );
    let message = output_text(&output).to_lowercase();
    assert!(
        message.contains("task")
            && (message.contains("map")
                || message.contains("mapping")
                || message.contains("scalar")
                || message.contains("type")),
        "install did not identify the incompatible config type:\n{message}"
    );
    assert_eq!(fs::read(&config).unwrap().as_slice(), original);
    assert_eq!(files_under(&agent_dir), vec![config]);
    assert!(!temp.state_home().exists());
}

#[test]
fn split_install_dry_run_leaves_agent_skills_and_state_untouched() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    let skills_dir = temp.0.join("shared-skills");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let config = agent_dir.join("config.yml");
    let original_config = b"task:\n  eager: never\n";
    fs::write(&config, original_config).expect("failed to seed config.yml");

    let output = run_cli_with_skills(
        &temp,
        &agent_dir,
        &skills_dir,
        &["install", "-t", TOPOLOGY, "--config-merge", "--dry-run"],
    );
    assert_success(&output);
    assert_eq!(fs::read(&config).unwrap().as_slice(), original_config);
    assert_eq!(files_under(&agent_dir), vec![config]);
    assert!(!skills_dir.exists());
    assert!(!temp.state_home().exists());
}

#[test]
fn split_skill_collision_does_not_partially_mutate_agent_or_config() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    let skills_dir = temp.0.join("shared-skills");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let config = agent_dir.join("config.yml");
    let original_config = b"unrelated: unchanged\n";
    fs::write(&config, original_config).expect("failed to seed config.yml");
    let collision = skills_dir.join("marketing-charter/SKILL.md");
    fs::create_dir_all(collision.parent().unwrap()).expect("failed to create skill collision");
    let user_skill = b"user-owned skill\n";
    fs::write(&collision, user_skill).expect("failed to seed skill collision");

    let output = run_cli_with_skills(
        &temp,
        &agent_dir,
        &skills_dir,
        &["install", "-t", TOPOLOGY, "--config-merge"],
    );
    assert!(
        !output.status.success(),
        "install accepted an unowned skill"
    );
    assert!(
        output_text(&output).to_lowercase().contains("collision")
            || output_text(&output).to_lowercase().contains("unowned"),
        "install did not report the split-skill collision:\n{}",
        output_text(&output)
    );
    assert_eq!(fs::read(&config).unwrap().as_slice(), original_config);
    assert_eq!(fs::read(&collision).unwrap().as_slice(), user_skill);
    assert!(!agent_dir.join("RULES.md").exists());
    assert!(!agent_dir.join("agents").exists());
    assert!(!temp.state_home().exists());
}

#[cfg(unix)]
#[test]
fn split_install_rejects_a_symlinked_skills_root_without_writing_outside() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let config = agent_dir.join("config.yml");
    let original_config = b"unrelated: unchanged\n";
    fs::write(&config, original_config).expect("failed to seed config.yml");
    let outside = temp.0.join("outside-skills");
    fs::create_dir(&outside).expect("failed to create external skills directory");
    let skills_link = temp.0.join("skills-link");
    symlink(&outside, &skills_link).expect("failed to create skills-root symlink");

    let output = run_cli_with_skills(
        &temp,
        &agent_dir,
        &skills_link,
        &["install", "-t", TOPOLOGY, "--config-merge"],
    );
    assert!(
        !output.status.success(),
        "install accepted a symlinked skills root"
    );
    assert!(
        output_text(&output).to_lowercase().contains("symlink"),
        "install did not report the symlinked skills root:\n{}",
        output_text(&output)
    );
    assert_eq!(fs::read(&config).unwrap().as_slice(), original_config);
    assert!(!agent_dir.join("RULES.md").exists());
    assert!(skills_link.is_symlink());
    assert!(fs::read_dir(&outside).unwrap().next().is_none());
    assert!(!temp.state_home().exists());
}

#[test]
fn remove_refuses_a_different_split_skills_root_without_deleting_files() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    let recorded_root = temp.0.join("recorded-skills");
    let other_root = temp.0.join("other-skills");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let installed = run_cli_with_skills(
        &temp,
        &agent_dir,
        &recorded_root,
        &["install", "-t", TOPOLOGY],
    );
    assert_success(&installed);
    let skill = recorded_root.join("marketing-charter/SKILL.md");
    let agent_file = agent_dir.join("agents/marketing.md");

    let output = run_cli_with_skills(&temp, &agent_dir, &other_root, &["remove", "-t", TOPOLOGY]);
    assert!(
        !output.status.success(),
        "remove accepted a skills root different from the recorded root"
    );
    assert!(skill.is_file(), "remove deleted the recorded split skill");
    assert!(agent_file.is_file(), "remove partially deleted agent files");
    assert!(
        !other_root.exists(),
        "remove created the unrecorded skills root"
    );
    assert!(manifest_path(&temp, &agent_dir).is_file());
}

#[test]
fn remove_preserves_an_edited_split_skill_until_retry() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    let skills_dir = temp.0.join("shared-skills");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let installed =
        run_cli_with_skills(&temp, &agent_dir, &skills_dir, &["install", "-t", TOPOLOGY]);
    assert_success(&installed);
    let skill = skills_dir.join("marketing-charter/SKILL.md");
    let edit = b"user's skill edit\n";
    fs::write(&skill, edit).expect("failed to edit split skill");

    let first_remove = run_cli(&temp, &agent_dir, &["remove", "-t", TOPOLOGY]);
    assert_success(&first_remove);
    assert!(output_text(&first_remove).contains("marketing-charter"));
    assert_eq!(fs::read(&skill).unwrap().as_slice(), edit);
    assert!(manifest_path(&temp, &agent_dir).is_file());

    fs::remove_file(&skill).expect("failed to resolve edit by removing the split skill");
    let retry = run_cli(&temp, &agent_dir, &["remove", "-t", TOPOLOGY]);
    assert_success(&retry);
    assert!(
        !skill.exists(),
        "retry left the deleted split skill in place"
    );
}

#[test]
fn remove_dry_run_preserves_files_and_normal_remove_restores_config() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let config = agent_dir.join("config.yml");
    fs::write(&config, "task:\n  eager: never\nunrelated: preserved\n")
        .expect("failed to seed config.yml");
    let installed = run_cli(
        &temp,
        &agent_dir,
        &["install", "-t", TOPOLOGY, "--config-merge"],
    );
    assert_success(&installed);
    let owned_file = agent_dir.join("agents/marketing.md");
    let owned_bytes = fs::read(&owned_file).expect("installed agent file is missing");
    let user_file = agent_dir.join("agents/user-created.md");
    let user_bytes = b"unowned file\n";
    fs::write(&user_file, user_bytes).expect("failed to create unowned agent file");
    let config_before_remove = fs::read(&config).expect("installed config is missing");
    let manifest = manifest_path(&temp, &agent_dir);
    let manifest_before_remove = fs::read(&manifest).expect("ownership manifest is missing");

    let dry_run = run_cli(&temp, &agent_dir, &["remove", "-t", TOPOLOGY, "--dry-run"]);
    assert_success(&dry_run);
    assert_eq!(fs::read(&config).unwrap(), config_before_remove);
    assert_eq!(fs::read(&owned_file).unwrap(), owned_bytes);
    assert_eq!(fs::read(&user_file).unwrap().as_slice(), user_bytes);
    assert_eq!(fs::read(&manifest).unwrap(), manifest_before_remove);

    let removed = run_cli(&temp, &agent_dir, &["remove", "-t", TOPOLOGY]);
    assert_success(&removed);
    let restored = fs::read_to_string(&config).expect("remove deleted config.yml");
    assert!(
        restored.contains("eager: never"),
        "prior config value was not restored"
    );
    assert!(restored.contains("unrelated: preserved"));
    assert!(!owned_file.exists(), "remove kept an unchanged owned file");
    assert_eq!(fs::read(&user_file).unwrap().as_slice(), user_bytes);
    assert!(!agent_dir.join("RULES.md").exists());
    assert!(!manifest.exists());
}

#[test]
fn remove_preserves_a_deleted_installed_config_key_until_retry() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let config = agent_dir.join("config.yml");
    fs::write(&config, "task:\n  maxConcurrency: 8\n").expect("failed to seed prior config value");
    let installed = run_cli(
        &temp,
        &agent_dir,
        &[
            "install",
            "-t",
            TOPOLOGY,
            "--config-merge",
            "--config-precedence=topology",
        ],
    );
    assert_success(&installed);
    let installed_config = fs::read_to_string(&config).expect("installed config is missing");
    assert!(installed_config.contains("maxConcurrency: 4"));
    let deleted_config = installed_config
        .lines()
        .filter(|line| !line.trim_start().starts_with("maxConcurrency:"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&config, deleted_config).expect("failed to delete installed config key");

    let first_remove = run_cli(&temp, &agent_dir, &["remove", "-t", TOPOLOGY]);
    assert_success(&first_remove);
    assert!(
        !fs::read_to_string(&config)
            .expect("remove deleted config.yml")
            .contains("maxConcurrency:"),
        "remove restored a key the user deleted"
    );
    assert!(output_text(&first_remove).contains("maxConcurrency"));
    let manifest = manifest_path(&temp, &agent_dir);
    assert!(
        manifest.is_file(),
        "deleted key conflict did not retain retry state"
    );

    // Recreate the installed leaf as valid YAML, as an editor would before retry.
    fs::write(&config, "task:\n  maxConcurrency: 4\n")
        .expect("failed to restore installed config key");
    let retry = run_cli(&temp, &agent_dir, &["remove", "-t", TOPOLOGY]);
    assert_success(&retry);
    assert!(
        fs::read_to_string(config)
            .expect("retry removed config.yml")
            .contains("maxConcurrency: 8"),
        "retry did not restore the original config value"
    );
    assert!(!manifest.exists());
}

#[cfg(unix)]
#[test]
fn install_refuses_a_symlinked_target_directory_without_writing_outside() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let config = agent_dir.join("config.yml");
    let original_config = b"unrelated: unchanged\n";
    fs::write(&config, original_config).expect("failed to seed config.yml");
    let outside = temp.0.join("outside-agent-files");
    fs::create_dir(&outside).expect("failed to create external agent directory");
    let linked_agents = agent_dir.join("agents");
    symlink(&outside, &linked_agents).expect("failed to link agent target outside");

    let output = run_cli(
        &temp,
        &agent_dir,
        &["install", "-t", TOPOLOGY, "--config-merge"],
    );
    assert!(
        !output.status.success(),
        "install accepted a symlinked target directory"
    );
    assert!(
        output_text(&output).to_lowercase().contains("symlink"),
        "install did not report the symlinked target:\n{}",
        output_text(&output)
    );
    assert_eq!(fs::read(&config).unwrap().as_slice(), original_config);
    assert!(linked_agents.is_symlink());
    assert!(fs::read_dir(&outside).unwrap().next().is_none());
    assert!(!temp.state_home().exists());
}
#[cfg(unix)]
#[test]
fn install_reports_symlink_preflight_refusal_before_registry_lock() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let outside = temp.0.join("outside-agent-files");
    fs::create_dir(&outside).expect("failed to create external agent directory");
    let linked_agents = agent_dir.join("agents");
    symlink(&outside, &linked_agents).expect("failed to link agent target outside");
    let source = local_topology_source(
        &temp,
        "setting: value\n",
        &[("agents/managed.md", b"topology-owned content\n")],
    );

    // Hold the public registry lock path. A preflight refusal must be
    // reported without waiting for or replacing this lock.
    let lock = temp.state_home().join("omp-topology/.registry.lock");
    fs::create_dir_all(lock.parent().expect("registry lock has no parent"))
        .expect("failed to create registry lock directory");
    let lock_contents = b"held by test\n";
    fs::write(&lock, lock_contents).expect("failed to hold registry lock");

    let output = install_from_source(&temp, &agent_dir, &source, &[]);
    assert!(
        !output.status.success(),
        "install accepted a symlinked target directory"
    );
    let message = output_text(&output).to_lowercase();
    assert!(
        message.contains("symlink"),
        "install did not report its symlink preflight refusal:\n{message}"
    );
    assert!(
        !message.contains("registry is locked"),
        "install attempted to acquire the registry lock before rejecting the symlink:\n{message}"
    );
    assert!(
        linked_agents.is_symlink(),
        "install replaced the user symlink"
    );
    assert!(
        fs::read_dir(&outside)
            .expect("failed to inspect external directory")
            .next()
            .is_none(),
        "install wrote a managed file outside the target"
    );
    assert_eq!(
        fs::read(&lock).expect("install removed or changed the held registry lock"),
        lock_contents
    );
    assert!(
        !temp
            .state_home()
            .join("omp-topology/registry.json")
            .exists(),
        "preflight refusal wrote a registry document"
    );
    assert!(
        !manifest_path(&temp, &agent_dir).exists(),
        "preflight refusal wrote an ownership manifest"
    );
}

#[test]
fn install_rejects_a_manifest_owned_by_another_topology_without_mutation() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let config = agent_dir.join("config.yml");
    fs::write(&config, "unrelated: unchanged\n").expect("failed to seed config.yml");
    let installed = run_cli(
        &temp,
        &agent_dir,
        &["install", "-t", TOPOLOGY, "--config-merge"],
    );
    assert_success(&installed);
    let original_config = fs::read(&config).expect("installed config is missing");
    let files_before: Vec<_> = files_under(&agent_dir)
        .into_iter()
        .map(|path| {
            (
                path.strip_prefix(&agent_dir).unwrap().to_path_buf(),
                fs::read(path).unwrap(),
            )
        })
        .collect();
    let manifest = manifest_path(&temp, &agent_dir);
    let old_manifest = fs::read_to_string(&manifest).expect("ownership manifest is missing");
    let altered_manifest = old_manifest.replace(
        &format!("\"topology\": \"{TOPOLOGY}\""),
        "\"topology\": \"another-topology\"",
    );
    assert_ne!(
        altered_manifest, old_manifest,
        "failed to change manifest topology"
    );
    fs::write(&manifest, altered_manifest).expect("failed to alter manifest topology");

    let output = run_cli(
        &temp,
        &agent_dir,
        &["install", "-t", TOPOLOGY, "--config-merge"],
    );
    assert!(
        !output.status.success(),
        "install accepted a manifest for another topology"
    );
    assert!(output_text(&output).contains("another-topology"));
    assert_eq!(fs::read(&config).unwrap(), original_config);
    let files_after: Vec<_> = files_under(&agent_dir)
        .into_iter()
        .map(|path| {
            (
                path.strip_prefix(&agent_dir).unwrap().to_path_buf(),
                fs::read(path).unwrap(),
            )
        })
        .collect();
    assert_eq!(files_after, files_before);
    assert!(
        fs::read_to_string(manifest)
            .unwrap()
            .contains("another-topology")
    );
}

#[cfg(unix)]
#[test]
fn cli_guidance_overlapping_topology_refusal_preserves_state_and_guides_recovery() {
    let temp = TestDir::new();
    let default_agent_dir = temp.0.join("default-agent");
    let profile_agent_dir = temp.0.join("unused-profile-agent");
    let profile_name = "digital-product-studio-plus";
    let source_a = local_named_topology_source(
        &temp,
        "source-a",
        TOPOLOGY,
        "fixture_a: A\n",
        &[("a.txt", b"topology A")],
    );
    let source_a = source_a.to_str().expect("source path is not UTF-8");
    let installed = run_cli_with_fake_omp(
        &temp,
        &default_agent_dir,
        profile_name,
        &profile_agent_dir,
        &[
            "install",
            "--topology",
            TOPOLOGY,
            "--source",
            source_a,
            "--install-id",
            "omp-dps",
        ],
    );
    assert_success(&installed);

    let manifest = manifest_path(&temp, &default_agent_dir);
    let registry = temp.state_home().join("omp-topology/registry.json");
    let registry_value: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry).expect("registry is missing"))
            .expect("registry is not valid JSON");
    assert_eq!(
        registry_value["aliases"]["omp-dps"]["agent_dir"],
        fs::canonicalize(&default_agent_dir)
            .expect("default target is missing")
            .display()
            .to_string(),
        "the existing alias does not own the default target"
    );
    let target_before = file_snapshot(&default_agent_dir);
    let state_root = temp.state_home().join("omp-topology");
    let state_before = file_snapshot(&state_root);
    let config_path = default_agent_dir.join("config.yml");
    let config_before = fs::read(&config_path).expect("installed config is missing");
    let manifest_before = fs::read(&manifest).expect("ownership manifest is missing");
    let registry_before = fs::read(&registry).expect("registry is missing");

    let source_b = local_named_topology_source(
        &temp,
        "source-b",
        "digital-product-studio-plus",
        "fixture_b: B\n",
        &[("a.txt", b"topology B")],
    );
    let source_b = source_b.to_str().expect("source path is not UTF-8");
    let refused = run_cli_with_fake_omp(
        &temp,
        &default_agent_dir,
        profile_name,
        &profile_agent_dir,
        &[
            "install",
            "--topology",
            "digital-product-studio-plus",
            "--source",
            source_b,
            "--install-id",
            "omp-dps-plus",
        ],
    );
    assert_eq!(
        refused.status.code(),
        Some(1),
        "install did not refuse the different topology:\n{}",
        output_text(&refused)
    );
    let diagnostics = output_text(&refused);
    let missing_guidance = [
        (
            "existing target path",
            diagnostics.contains(&default_agent_dir.display().to_string()),
        ),
        (
            "existing and requested topology names",
            diagnostics.contains(TOPOLOGY) && diagnostics.contains("digital-product-studio-plus"),
        ),
        (
            "separate-profile hint",
            diagnostics.contains("--profile digital-product-studio-plus"),
        ),
        (
            "exact existing-install-ID update command",
            diagnostics.contains("omp-profile update --install-id omp-dps"),
        ),
        (
            "update keeps the existing topology",
            diagnostics.contains("keeps 'digital-product-studio'"),
        ),
        (
            "update does not install the requested topology",
            diagnostics.contains("does not install 'digital-product-studio-plus'"),
        ),
    ]
    .into_iter()
    .filter_map(|(guidance, present)| (!present).then_some(guidance))
    .collect::<Vec<_>>();
    let diagnostics_lower = diagnostics.to_lowercase();
    for incorrect_claim in [
        "to point the existing install at the requested topology",
        "to switch the existing install to the requested topology",
    ] {
        assert!(
            !diagnostics_lower.contains(incorrect_claim),
            "recovery guidance falsely claims update changes the installed topology:\n{diagnostics}"
        );
    }

    assert_eq!(file_snapshot(&default_agent_dir), target_before);
    assert_eq!(file_snapshot(&state_root), state_before);
    assert_eq!(fs::read(&config_path).unwrap(), config_before);
    assert_eq!(fs::read(&manifest).unwrap(), manifest_before);
    assert_eq!(fs::read(&registry).unwrap(), registry_before);
    assert!(
        missing_guidance.is_empty(),
        "refusal omitted approved guidance {missing_guidance:?}:\n{diagnostics}"
    );
}

#[cfg(unix)]
#[test]
fn cli_guidance_symlinked_skills_dir_refusal_has_no_cross_topology_guidance() {
    let temp = TestDir::new();
    let default_agent_dir = temp.0.join("default-agent");
    let profile_agent_dir = temp.0.join("unused-profile-agent");
    let profile_name = "digital-product-studio-plus";
    let owner_source = local_named_topology_source(
        &temp,
        "owner-source",
        TOPOLOGY,
        "fixture_owner: owner\n",
        &[("owner.txt", b"existing topology")],
    );
    let owner_source = owner_source
        .to_str()
        .expect("owner source path is not UTF-8");
    let installed = run_cli_with_fake_omp(
        &temp,
        &default_agent_dir,
        profile_name,
        &profile_agent_dir,
        &[
            "install",
            "--topology",
            TOPOLOGY,
            "--source",
            owner_source,
            "--install-id",
            "omp-owner",
        ],
    );
    assert_success(&installed);

    let requested_source = local_named_topology_source(
        &temp,
        "requested-source",
        "digital-product-studio-plus",
        "fixture_requested: requested\n",
        &[("requested.txt", b"requested topology")],
    );
    let requested_source = requested_source
        .to_str()
        .expect("requested source path is not UTF-8");
    let outside_skills = temp.0.join("outside-skills");
    fs::create_dir(&outside_skills).expect("failed to create external skills directory");
    let skills_link = temp.0.join("skills-link");
    symlink(&outside_skills, &skills_link).expect("failed to create skills-root symlink");

    let target_before = file_snapshot(&default_agent_dir);
    let state_root = temp.state_home().join("omp-topology");
    let state_before = file_snapshot(&state_root);
    let refused = run_cli_with_fake_omp(
        &temp,
        &default_agent_dir,
        profile_name,
        &profile_agent_dir,
        &[
            "install",
            "--topology",
            "digital-product-studio-plus",
            "--source",
            requested_source,
            "--install-id",
            "omp-requested",
            "--skills-dir",
            skills_link.to_str().expect("skills path is not UTF-8"),
        ],
    );
    assert_eq!(
        refused.status.code(),
        Some(1),
        "install did not refuse the symlinked skills directory:\n{}",
        output_text(&refused)
    );
    let diagnostics = output_text(&refused);
    assert!(
        diagnostics.to_lowercase().contains("symlink"),
        "install did not report the symlinked skills directory:\n{diagnostics}"
    );
    assert!(
        !diagnostics.contains("guidance:"),
        "unrelated skills-directory refusal received cross-topology guidance:\n{diagnostics}"
    );
    assert!(
        !diagnostics.contains("omp-profile update --install-id"),
        "unrelated skills-directory refusal received an update command:\n{diagnostics}"
    );
    assert_eq!(file_snapshot(&default_agent_dir), target_before);
    assert_eq!(file_snapshot(&state_root), state_before);
}

#[cfg(unix)]
#[test]
fn cli_guidance_multi_owner_collision_names_file_owner_install_id() {
    let temp = TestDir::new();
    let default_agent_dir = temp.0.join("default-agent");
    let profile_agent_dir = temp.0.join("unused-profile-agent");
    let profile_name = "digital-product-studio-plus";
    let first_source = local_named_topology_source(
        &temp,
        "first-source",
        "alpha-topology",
        "fixture_alpha: alpha\n",
        &[("alpha.txt", b"alpha topology")],
    );
    let first_source = first_source
        .to_str()
        .expect("first source path is not UTF-8");
    let first_install = run_cli_with_fake_omp(
        &temp,
        &default_agent_dir,
        profile_name,
        &profile_agent_dir,
        &[
            "install",
            "--topology",
            "alpha-topology",
            "--source",
            first_source,
            "--install-id",
            "omp-alpha",
        ],
    );
    assert_success(&first_install);

    let file_owner_source = local_named_topology_source(
        &temp,
        "file-owner-source",
        "beta-topology",
        "fixture_beta: beta\n",
        &[("owned-collision.txt", b"beta topology")],
    );
    let file_owner_source = file_owner_source
        .to_str()
        .expect("file-owner source path is not UTF-8");
    let file_owner_install = run_cli_with_fake_omp(
        &temp,
        &default_agent_dir,
        profile_name,
        &profile_agent_dir,
        &[
            "install",
            "--topology",
            "beta-topology",
            "--source",
            file_owner_source,
            "--install-id",
            "omp-zeta",
        ],
    );
    assert_success(&file_owner_install);

    let third_source = local_named_topology_source(
        &temp,
        "third-source",
        "gamma-topology",
        "fixture_gamma: gamma\n",
        &[("owned-collision.txt", b"gamma topology")],
    );
    let third_source = third_source
        .to_str()
        .expect("third source path is not UTF-8");
    let target_before = file_snapshot(&default_agent_dir);
    let state_root = temp.state_home().join("omp-topology");
    let state_before = file_snapshot(&state_root);
    let refused = run_cli_with_fake_omp(
        &temp,
        &default_agent_dir,
        profile_name,
        &profile_agent_dir,
        &[
            "install",
            "--topology",
            "gamma-topology",
            "--source",
            third_source,
            "--install-id",
            "omp-gamma",
        ],
    );
    assert_eq!(
        refused.status.code(),
        Some(1),
        "install did not refuse the colliding file:\n{}",
        output_text(&refused)
    );
    let diagnostics = output_text(&refused);
    assert!(
        diagnostics.contains("owned-collision.txt"),
        "refusal did not identify the colliding file:\n{diagnostics}"
    );
    assert!(
        diagnostics.contains("omp-profile update --install-id omp-zeta"),
        "guidance did not name the install ID that owns the colliding file:\n{diagnostics}"
    );
    assert!(
        !diagnostics.contains("omp-profile update --install-id omp-alpha"),
        "guidance incorrectly named the lexicographically first, non-owning install ID:\n{diagnostics}"
    );
    assert_eq!(file_snapshot(&default_agent_dir), target_before);
    assert_eq!(file_snapshot(&state_root), state_before);
}

#[cfg(unix)]
#[test]
fn cli_guidance_disjoint_topologies_share_target_under_independent_aliases() {
    let temp = TestDir::new();
    let default_agent_dir = temp.0.join("default-agent");
    let profile_agent_dir = temp.0.join("unused-profile-agent");
    let profile_name = "digital-product-studio-plus";
    let source_a = local_named_topology_source(
        &temp,
        "source-a",
        TOPOLOGY,
        "fixture_a: A\n",
        &[("a.txt", b"topology A")],
    );
    let source_a = source_a.to_str().expect("source path is not UTF-8");
    let installed_a = run_cli_with_fake_omp(
        &temp,
        &default_agent_dir,
        profile_name,
        &profile_agent_dir,
        &[
            "install",
            "--topology",
            TOPOLOGY,
            "--source",
            source_a,
            "--install-id",
            "omp-dps",
        ],
    );
    assert_success(&installed_a);

    let source_b = local_named_topology_source(
        &temp,
        "source-b",
        "digital-product-studio-plus",
        "fixture_b: B\n",
        &[("b.txt", b"topology B")],
    );
    let source_b = source_b.to_str().expect("source path is not UTF-8");
    let installed_b = run_cli_with_fake_omp(
        &temp,
        &default_agent_dir,
        profile_name,
        &profile_agent_dir,
        &[
            "install",
            "--topology",
            "digital-product-studio-plus",
            "--source",
            source_b,
            "--install-id",
            "omp-dps-plus",
        ],
    );
    assert_success(&installed_b);
    assert_eq!(
        fs::read(default_agent_dir.join("a.txt")).unwrap(),
        b"topology A"
    );
    assert_eq!(
        fs::read(default_agent_dir.join("b.txt")).unwrap(),
        b"topology B"
    );

    let registry = temp.state_home().join("omp-topology/registry.json");
    let registry_value: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry).expect("registry is missing"))
            .expect("registry is not valid JSON");
    let target = fs::canonicalize(&default_agent_dir)
        .expect("default target is missing")
        .display()
        .to_string();
    for (alias, topology) in [
        ("omp-dps", TOPOLOGY),
        ("omp-dps-plus", "digital-product-studio-plus"),
    ] {
        assert_eq!(registry_value["aliases"][alias]["agent_dir"], target);
        assert_eq!(registry_value["aliases"][alias]["topology"], topology);
    }
}

#[cfg(unix)]
#[test]
fn cli_guidance_named_profile_dry_run_preserves_default_state() {
    let temp = TestDir::new();
    let default_agent_dir = temp.0.join("default-agent");
    let profile_name = "digital-product-studio-plus";
    let profile_root = temp.0.join("home/.omp/profiles").join(profile_name);
    let profile_agent_dir = profile_root.join("agent");
    let source_a = local_named_topology_source(
        &temp,
        "source-a",
        TOPOLOGY,
        "fixture_a: A\n",
        &[("a.txt", b"topology A")],
    );
    let source_a = source_a.to_str().expect("source path is not UTF-8");
    let installed = run_cli_with_fake_omp(
        &temp,
        &default_agent_dir,
        profile_name,
        &profile_agent_dir,
        &[
            "install",
            "--topology",
            TOPOLOGY,
            "--source",
            source_a,
            "--install-id",
            "omp-dps",
        ],
    );
    assert_success(&installed);

    let manifest = manifest_path(&temp, &default_agent_dir);
    let registry = temp.state_home().join("omp-topology/registry.json");
    let state_root = temp.state_home().join("omp-topology");
    let target_before = file_snapshot(&default_agent_dir);
    let state_before = file_snapshot(&state_root);
    let manifest_before = fs::read(&manifest).expect("ownership manifest is missing");
    let registry_before = fs::read(&registry).expect("registry is missing");

    let source_b = local_named_topology_source(
        &temp,
        "source-b",
        "digital-product-studio-plus",
        "fixture_b: B\n",
        &[("b.txt", b"topology B")],
    );
    let source_b = source_b.to_str().expect("source path is not UTF-8");
    let planned = run_cli_with_fake_omp(
        &temp,
        &default_agent_dir,
        profile_name,
        &profile_agent_dir,
        &[
            "install",
            "--topology",
            "digital-product-studio-plus",
            "--source",
            source_b,
            "--profile",
            profile_name,
            "--install-id",
            "omp-dps-plus",
            "--dry-run",
        ],
    );
    assert_success(&planned);
    let plan = String::from_utf8_lossy(&planned.stdout);
    assert!(
        plan.contains(&profile_agent_dir.display().to_string()),
        "dry-run plan did not target the named profile agent directory:\n{plan}"
    );
    assert!(
        !plan.contains(&default_agent_dir.display().to_string()),
        "dry-run plan used the existing default target:\n{plan}"
    );
    assert!(
        !profile_root.exists() && !profile_agent_dir.exists(),
        "dry-run created profile or agent directories"
    );
    assert_eq!(file_snapshot(&default_agent_dir), target_before);
    assert_eq!(file_snapshot(&state_root), state_before);
    assert_eq!(fs::read(&manifest).unwrap(), manifest_before);
    assert_eq!(fs::read(&registry).unwrap(), registry_before);
    let registry_value: serde_json::Value =
        serde_json::from_slice(&registry_before).expect("registry is not valid JSON");
    assert!(
        registry_value["aliases"].get("omp-dps-plus").is_none(),
        "dry-run registered the new alias"
    );
}

#[cfg(unix)]
#[test]
fn cli_guidance_status_distinguishes_manifest_from_registry_install_id() {
    let temp = TestDir::new();
    let profile_name = "manifest-only";
    let profile_agent_dir = temp
        .0
        .join("home/.omp/profiles")
        .join(profile_name)
        .join("agent");
    let source = local_named_topology_source(
        &temp,
        "source",
        TOPOLOGY,
        "fixture: true\n",
        &[("payload.txt", b"installed")],
    );
    let installed = install_from_source(&temp, &profile_agent_dir, &source, &[]);
    assert_success(&installed);

    let registry_path = temp.state_home().join("omp-topology/registry.json");
    let mut registry: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("registry is missing"))
            .expect("registry is not valid JSON");
    let aliases = registry["aliases"]
        .as_object_mut()
        .expect("registry aliases are not an object");
    assert!(
        aliases.remove("agent").is_some(),
        "test setup did not register the expected default install ID"
    );
    fs::write(
        &registry_path,
        serde_json::to_vec(&registry).expect("failed to serialize registry fixture"),
    )
    .expect("failed to remove the test install ID registration");
    assert!(
        manifest_path(&temp, &profile_agent_dir).is_file(),
        "test setup lost the existing topology manifest"
    );
    assert!(
        registry["aliases"]
            .as_object()
            .is_some_and(|aliases| aliases.is_empty()),
        "test setup unexpectedly retained an install ID: {registry}"
    );

    let output = run_cli_with_fake_omp(
        &temp,
        &temp.0.join("unused-default-agent"),
        profile_name,
        &profile_agent_dir,
        &["status", "--profile", profile_name],
    );
    assert_success(&output);
    let status = output_text(&output);
    assert!(
        status.contains("topology manifest: digital-product-studio"),
        "status did not identify the topology manifest:\n{status}"
    );
    assert!(
        status.contains("registry install ID: none"),
        "status did not distinguish an absent registry install ID:\n{status}"
    );
    assert!(
        status.contains(&profile_agent_dir.display().to_string()),
        "status omitted the resolved target path:\n{status}"
    );
    assert!(
        status.contains("profile: manifest-only"),
        "status omitted the selected profile:\n{status}"
    );
}

#[cfg(unix)]
#[test]
fn default_target_comes_from_omp_config_path() {
    let temp = TestDir::new();
    let active_dir = temp.0.join("active-omp-agent");
    let bin_dir = temp.0.join("bin");
    fs::create_dir_all(&bin_dir).expect("failed to create fake PATH");
    let fake_omp = bin_dir.join("omp");
    fs::write(
        &fake_omp,
        "#!/bin/sh\n\
         if [ \"$1 $2\" = \"config path\" ]; then\n\
         \tprintf '%s\\n' \"$OMP_TEST_AGENT_DIR\"\n\
         \texit 0\n\
         fi\n\
         exit 64\n",
    )
    .expect("failed to write fake omp");
    fs::set_permissions(&fake_omp, fs::Permissions::from_mode(0o755))
        .expect("failed to make fake omp executable");
    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(bin_dir).chain(std::env::split_paths(&inherited_path)),
    )
    .expect("failed to build isolated PATH");

    let source = default_legacy_source(&temp);
    let output = cli_command(&temp, Path::new(env!("CARGO_MANIFEST_DIR")))
        .args(["install", "-t", TOPOLOGY, "--source"])
        .arg(source)
        .env("PATH", path)
        .env("OMP_TEST_AGENT_DIR", &active_dir)
        .output()
        .expect("failed to execute installer with default target");
    assert_success(&output);
    assert!(
        active_dir.join("agents/marketing.md").is_file(),
        "install did not use omp config path"
    );
    assert!(!temp.0.join("agent").exists());
}

#[cfg(unix)]
#[test]
fn install_and_remove_snapshots_are_timestamped_and_private() {
    fn assert_private_tree(root: &Path) {
        fn check(directory: &Path) {
            assert_eq!(
                fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700,
                "state directory is not owner-only: {}",
                directory.display()
            );
            for entry in fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if entry.file_type().unwrap().is_dir() {
                    check(&path);
                } else if entry.file_type().unwrap().is_file() {
                    assert_eq!(
                        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                        0o600,
                        "state file is not owner-only: {}",
                        path.display()
                    );
                }
            }
        }
        check(root);
    }

    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let config = agent_dir.join("config.yml");
    let original_config = b"local: before-install\n";
    fs::write(&config, original_config).expect("failed to seed config.yml");
    let installed = run_cli(
        &temp,
        &agent_dir,
        &["install", "-t", TOPOLOGY, "--config-merge"],
    );
    assert_success(&installed);

    let state_root = temp.state_home().join("omp-topology");
    let install_backup = files_under(&state_root)
        .into_iter()
        .find(|path| fs::read(path).is_ok_and(|bytes| bytes.as_slice() == original_config))
        .expect("install did not back up the original config");
    assert!(
        install_backup.ancestors().any(|path| path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("install-"))),
        "install backup path was not timestamped"
    );
    let config_before_remove = fs::read(&config).expect("installed config is missing");
    let agent_file = agent_dir.join("agents/marketing.md");
    let agent_bytes = fs::read(&agent_file).expect("installed agent file is missing");

    let removed = run_cli(&temp, &agent_dir, &["remove", "-t", TOPOLOGY]);
    assert_success(&removed);
    let remove_snapshot_files = files_under(&state_root);
    assert!(
        remove_snapshot_files
            .iter()
            .any(|path| fs::read(path).is_ok_and(|bytes| bytes == config_before_remove)),
        "remove did not snapshot config before mutation"
    );
    assert!(
        remove_snapshot_files
            .iter()
            .any(|path| fs::read(path).is_ok_and(|bytes| bytes == agent_bytes)),
        "remove did not snapshot owned files before mutation"
    );
    assert!(
        remove_snapshot_files.iter().any(|path| {
            path.ancestors().any(|parent| {
                parent
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("remove-"))
            })
        }),
        "remove snapshot path was not timestamped"
    );
    assert_private_tree(&temp.state_home());
}
fn run_cli_without_agent_dir(temp: &TestDir, args: &[&str]) -> Output {
    cli_command(temp, Path::new(env!("CARGO_MANIFEST_DIR")))
        .args(args)
        .output()
        .expect("failed to execute omp-profile")
}

#[test]
fn config_yaml_default_install_preserves_existing_config_and_publishes_candidate() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let config = agent_dir.join("config.yml");
    let original = b"# keep this local comment\nlocal:\n  value: unchanged\n";
    fs::write(&config, original).expect("failed to seed config.yml");
    let topology_config = "topology:\n  value: candidate\n";
    let source = local_topology_source(&temp, topology_config, &[]);

    let installed = install_from_source(&temp, &agent_dir, &source, &[]);
    assert_success(&installed);

    assert_eq!(
        fs::read(&config).expect("active config disappeared"),
        original,
        "default install changed an existing active config"
    );
    assert_eq!(
        fs::read(agent_dir.join("config.yml.new")).expect("config candidate was not published"),
        topology_config.as_bytes(),
        "candidate does not contain the topology config"
    );
}
#[test]
fn update_with_version_refreshes_an_existing_recorded_alias() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let source = local_topology_source(&temp, "fixture: true\n", &[("payload.txt", b"v1")]);
    let alias = "semver-existing";

    let installed = install_from_source(&temp, &agent_dir, &source, &["--install-id", alias]);
    assert_success(&installed);

    let payload = agent_dir.join("payload.txt");
    assert_eq!(
        fs::read(&payload).expect("initial payload is missing"),
        b"v1"
    );

    let manifest_path = manifest_path(&temp, &agent_dir);
    let initial_manifest_bytes = fs::read(&manifest_path).expect("initial manifest is missing");
    let initial_manifest: serde_json::Value = serde_json::from_slice(&initial_manifest_bytes)
        .expect("initial manifest is not valid JSON");
    assert_eq!(initial_manifest["topology_version"], "1.0.0");

    let registry_path = temp.state_home().join("omp-topology/registry.json");
    let initial_registry_bytes = fs::read(&registry_path).expect("registry is missing");
    let initial_registry: serde_json::Value =
        serde_json::from_slice(&initial_registry_bytes).expect("registry is not valid JSON");
    let initial_alias = &initial_registry["aliases"][alias];
    assert!(
        !initial_alias.is_null(),
        "install did not register alias '{alias}'"
    );
    let initial_target = initial_alias["agent_dir"]
        .as_str()
        .expect("registry alias has no target")
        .to_owned();
    let initial_target_snapshot = file_snapshot(&agent_dir);
    let topology_dir = source.join("topologies").join(TOPOLOGY);
    fs::write(
        topology_dir.join("topology.yml"),
        format!(
            "name: {TOPOLOGY}\nversion: 1.0.1\ndescription: installer fixture\ninstallable: true\n"
        ),
    )
    .expect("failed to bump fixture topology version");
    fs::write(topology_dir.join("agent/payload.txt"), b"v2")
        .expect("failed to update fixture payload");
    let dry_run = run_cli_without_agent_dir(
        &temp,
        &[
            "update",
            "--install-id",
            alias,
            "--dry-run",
            "--version",
            "1.0.1",
        ],
    );
    assert_success(&dry_run);
    assert_eq!(
        fs::read(&registry_path).expect("registry is missing after dry-run"),
        initial_registry_bytes,
        "dry-run changed the alias registry"
    );
    assert_eq!(
        file_snapshot(&agent_dir),
        initial_target_snapshot,
        "dry-run changed installed target files"
    );
    assert_eq!(
        fs::read(&manifest_path).expect("manifest is missing after dry-run"),
        initial_manifest_bytes,
        "dry-run changed the installed manifest"
    );
    assert_eq!(
        fs::read(&payload).expect("installed payload is missing after dry-run"),
        b"v1",
        "dry-run changed the installed payload"
    );

    let updated = run_cli_without_agent_dir(
        &temp,
        &["update", "--install-id", alias, "--version", "1.0.1"],
    );
    assert_success(&updated);

    assert_eq!(
        fs::read(&payload).expect("updated payload is missing"),
        b"v2",
        "update did not refresh the existing target from its recorded source"
    );
    let updated_manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("updated manifest is missing"))
            .expect("updated manifest is not valid JSON");
    assert_eq!(updated_manifest["topology_version"], "1.0.1");

    let updated_registry: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("updated registry is missing"))
            .expect("updated registry is not valid JSON");
    assert!(
        updated_registry["aliases"][alias].is_object(),
        "update removed or renamed alias '{alias}'"
    );
    assert_eq!(
        updated_registry["aliases"][alias]["agent_dir"], initial_target,
        "update changed the alias target"
    );
}

#[test]
fn update_rejects_downgrade_without_explicit_version() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let source = local_topology_source(&temp, "fixture: true\n", &[("payload.txt", b"NEW")]);
    let alias = "semver-downgrade";
    let topology_dir = source.join("topologies").join(TOPOLOGY);
    fs::write(
        topology_dir.join("topology.yml"),
        format!(
            "name: {TOPOLOGY}\nversion: 1.1.0\ndescription: installer fixture\ninstallable: true\n"
        ),
    )
    .expect("failed to set fixture topology version to 1.1.0");

    let installed = install_from_source(&temp, &agent_dir, &source, &["--install-id", alias]);
    assert_success(&installed);

    let payload = agent_dir.join("payload.txt");
    assert_eq!(
        fs::read(&payload).expect("initial payload is missing"),
        b"NEW"
    );
    let manifest_path = manifest_path(&temp, &agent_dir);
    let initial_manifest = fs::read(&manifest_path).expect("initial manifest is missing");
    let initial_manifest_value: serde_json::Value =
        serde_json::from_slice(&initial_manifest).expect("initial manifest is not valid JSON");
    assert_eq!(initial_manifest_value["topology_version"], "1.1.0");

    let registry_path = temp.state_home().join("omp-topology/registry.json");
    let initial_registry = fs::read(&registry_path).expect("registry is missing");
    let initial_registry_value: serde_json::Value =
        serde_json::from_slice(&initial_registry).expect("registry is not valid JSON");
    let initial_target = initial_registry_value["aliases"][alias]["agent_dir"]
        .as_str()
        .expect("registry alias has no target")
        .to_owned();
    let initial_target_snapshot = file_snapshot(&agent_dir);

    fs::write(
        topology_dir.join("topology.yml"),
        format!(
            "name: {TOPOLOGY}\nversion: 1.0.0\ndescription: installer fixture\ninstallable: true\n"
        ),
    )
    .expect("failed to downgrade fixture topology version to 1.0.0");
    assert_eq!(
        fs::read(topology_dir.join("agent/payload.txt"))
            .expect("source payload is missing after metadata change"),
        b"NEW",
        "downgrade fixture changed the payload instead of only topology metadata"
    );
    let updated = run_cli_without_agent_dir(&temp, &["update", "--install-id", alias]);
    let message = output_text(&updated);
    assert!(
        !updated.status.success(),
        "update unexpectedly permitted downgrade from 1.1.0 to 1.0.0:\n{message}"
    );
    let diagnostic = message.to_ascii_lowercase();
    assert!(
        diagnostic.contains("downgrade") && message.contains("1.1.0") && message.contains("1.0.0"),
        "downgrade diagnostic must identify both versions:\n{message}"
    );

    assert_eq!(fs::read(&payload).expect("payload is missing"), b"NEW");
    assert_eq!(
        file_snapshot(&agent_dir),
        initial_target_snapshot,
        "downgrade changed installed target files"
    );
    assert_eq!(
        fs::read(&manifest_path).expect("manifest is missing"),
        initial_manifest,
        "downgrade changed the installed manifest"
    );
    assert_eq!(
        fs::read(&registry_path).expect("registry is missing"),
        initial_registry,
        "downgrade changed the alias registry"
    );
    let registry_after: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("registry is missing"))
            .expect("updated registry is not valid JSON");
    assert_eq!(
        registry_after["aliases"][alias]["agent_dir"], initial_target,
        "downgrade changed the alias target"
    );
}
#[test]
fn update_without_version_refreshes_metadata_only_local_source() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let source = local_topology_source_with_version(
        &temp,
        "fixture: true\n",
        &[("payload.txt", b"unchanged payload")],
        "1.0.0",
    );
    let alias = "semver-metadata-upgrade";
    let installed = install_from_source(&temp, &agent_dir, &source, &["--install-id", alias]);
    assert_success(&installed);

    let payload = agent_dir.join("payload.txt");
    let payload_before = fs::read(&payload).expect("installed payload is missing");
    let manifest_path = manifest_path(&temp, &agent_dir);
    let registry_path = temp.state_home().join("omp-topology/registry.json");
    let initial_registry: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("registry is missing"))
            .expect("registry is not valid JSON");
    let initial_target = initial_registry["aliases"][alias]["agent_dir"]
        .as_str()
        .expect("registry alias has no target")
        .to_owned();

    let topology_file = source
        .join("topologies")
        .join(TOPOLOGY)
        .join("topology.yml");
    fs::write(
        &topology_file,
        format!(
            "name: {TOPOLOGY}\nversion: 1.0.1\ndescription: installer fixture\ninstallable: true\n"
        ),
    )
    .expect("failed to change only local topology metadata");

    let updated = run_cli_without_agent_dir(&temp, &["update", "--install-id", alias]);
    assert_success(&updated);

    let updated_manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("updated manifest is missing"))
            .expect("updated manifest is not valid JSON");
    assert_eq!(
        updated_manifest["topology_version"], "1.0.1",
        "metadata-only update did not refresh the installed topology version"
    );
    assert_eq!(
        fs::read(&payload).expect("installed payload is missing"),
        payload_before,
        "metadata-only update changed the unchanged payload"
    );
    let updated_registry: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("registry is missing"))
            .expect("updated registry is not valid JSON");
    assert_eq!(
        updated_registry["aliases"][alias]["agent_dir"], initial_target,
        "metadata-only update changed the alias target"
    );
}

#[test]
fn update_rejects_invalid_declared_version_without_mutation() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let source = local_topology_source_with_version(
        &temp,
        "fixture: true\n",
        &[("payload.txt", b"unchanged payload")],
        "1.0.0",
    );
    let alias = "semver-invalid-metadata-update";
    let installed = install_from_source(&temp, &agent_dir, &source, &["--install-id", alias]);
    assert_success(&installed);

    let target_before = file_snapshot(&agent_dir);
    let manifest_path = manifest_path(&temp, &agent_dir);
    let manifest_before = fs::read(&manifest_path).expect("manifest is missing");
    let registry_path = temp.state_home().join("omp-topology/registry.json");
    let registry_before = fs::read(&registry_path).expect("registry is missing");
    let topology_file = source
        .join("topologies")
        .join(TOPOLOGY)
        .join("topology.yml");
    fs::write(
        &topology_file,
        format!(
            "name: {TOPOLOGY}\nversion: 1.2\ndescription: installer fixture\ninstallable: true\n"
        ),
    )
    .expect("failed to set invalid local topology version");

    let updated = run_cli_without_agent_dir(&temp, &["update", "--install-id", alias]);
    let message = output_text(&updated);
    assert!(
        !updated.status.success(),
        "update unexpectedly accepted invalid declared version 1.2:\n{message}"
    );
    let diagnostic = message.to_ascii_lowercase();
    assert!(
        diagnostic.contains("1.2")
            && (diagnostic.contains("invalid") || diagnostic.contains("semver")),
        "invalid declared version diagnostic must identify 1.2 as invalid:\n{message}"
    );
    assert_eq!(file_snapshot(&agent_dir), target_before);
    assert_eq!(
        fs::read(&manifest_path).expect("manifest is missing"),
        manifest_before,
        "invalid declared version changed the installed manifest"
    );
    assert_eq!(
        fs::read(&registry_path).expect("registry is missing"),
        registry_before,
        "invalid declared version changed the alias registry"
    );
}

#[test]
fn update_with_mismatched_version_fails_before_mutation() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let source = local_topology_source_with_version(
        &temp,
        "fixture: true\n",
        &[("payload.txt", b"source payload")],
        "1.0.0",
    );
    let alias = "semver-update-mismatch";
    let installed = install_from_source(&temp, &agent_dir, &source, &["--install-id", alias]);
    assert_success(&installed);

    let target_before = file_snapshot(&agent_dir);
    let manifest_path = manifest_path(&temp, &agent_dir);
    let manifest_before = fs::read(&manifest_path).expect("manifest is missing");
    let registry_path = temp.state_home().join("omp-topology/registry.json");
    let registry_before = fs::read(&registry_path).expect("registry is missing");

    let updated = run_cli_without_agent_dir(
        &temp,
        &["update", "--install-id", alias, "--version", "2.0.0"],
    );
    let message = output_text(&updated);
    assert!(
        !updated.status.success(),
        "update unexpectedly accepted requested version 2.0.0 for source version 1.0.0:\n{message}"
    );
    assert!(
        message.contains("2.0.0") && message.contains("1.0.0"),
        "version mismatch diagnostic must identify requested and declared versions:\n{message}"
    );
    assert_eq!(file_snapshot(&agent_dir), target_before);
    assert_eq!(
        fs::read(&manifest_path).expect("manifest is missing"),
        manifest_before,
        "version mismatch changed the installed manifest"
    );
    assert_eq!(
        fs::read(&registry_path).expect("registry is missing"),
        registry_before,
        "version mismatch changed the alias registry"
    );
}

#[test]
fn update_rejects_invalid_declared_source_without_mutation() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let source = local_topology_source_with_version(
        &temp,
        "fixture: true\n",
        &[("payload.txt", b"original payload")],
        "1.0.0",
    );
    let alias = "semver-invalid-source-update";
    let installed = install_from_source(&temp, &agent_dir, &source, &["--install-id", alias]);
    assert_success(&installed);

    let target_before = file_snapshot(&agent_dir);
    let manifest_path = manifest_path(&temp, &agent_dir);
    let manifest_before = fs::read(&manifest_path).expect("manifest is missing");
    let registry_path = temp.state_home().join("omp-topology/registry.json");
    let registry_before = fs::read(&registry_path).expect("registry is missing");
    let topology_dir = source.join("topologies").join(TOPOLOGY);
    fs::write(
        topology_dir.join("topology.yml"),
        format!(
            "name: {TOPOLOGY}\nversion: 1.2\ndescription: installer fixture\ninstallable: true\n"
        ),
    )
    .expect("failed to set invalid local topology version");
    fs::write(
        topology_dir.join("agent/payload.txt"),
        b"changed source payload",
    )
    .expect("failed to change local topology payload");

    let updated = run_cli_without_agent_dir(&temp, &["update", "--install-id", alias]);
    let message = output_text(&updated);
    assert!(
        !updated.status.success(),
        "update unexpectedly accepted invalid declared source version 1.2:\n{message}"
    );
    let diagnostic = message.to_ascii_lowercase();
    assert!(
        diagnostic.contains("1.2")
            && (diagnostic.contains("invalid") || diagnostic.contains("semver")),
        "invalid declared source diagnostic must identify 1.2 as invalid:\n{message}"
    );
    assert_eq!(file_snapshot(&agent_dir), target_before);
    assert_eq!(
        fs::read(&manifest_path).expect("manifest is missing"),
        manifest_before,
        "invalid declared source changed the installed manifest"
    );
    assert_eq!(
        fs::read(&registry_path).expect("registry is missing"),
        registry_before,
        "invalid declared source changed the alias registry"
    );
}

#[test]
fn update_rejects_invalid_installed_version_without_mutation() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let source = local_topology_source_with_version(
        &temp,
        "fixture: true\n",
        &[("payload.txt", b"original payload")],
        "1.0.0",
    );
    let alias = "semver-invalid-installed-version";
    let installed = install_from_source(&temp, &agent_dir, &source, &["--install-id", alias]);
    assert_success(&installed);

    let manifest_path = manifest_path(&temp, &agent_dir);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("manifest is missing"))
            .expect("manifest is not valid JSON");
    manifest["topology_version"] = serde_json::json!("1.2");
    fs::write(
        &manifest_path,
        serde_json::to_vec(&manifest).expect("failed to encode malformed installed version"),
    )
    .expect("failed to set malformed installed version");
    let manifest_before = fs::read(&manifest_path).expect("manifest is missing");
    let registry_path = temp.state_home().join("omp-topology/registry.json");
    let registry_before = fs::read(&registry_path).expect("registry is missing");
    let target_before = file_snapshot(&agent_dir);

    let topology_dir = source.join("topologies").join(TOPOLOGY);
    fs::write(
        topology_dir.join("topology.yml"),
        format!(
            "name: {TOPOLOGY}\nversion: 1.1.0\ndescription: installer fixture\ninstallable: true\n"
        ),
    )
    .expect("failed to set source upgrade version");
    fs::write(
        topology_dir.join("agent/payload.txt"),
        b"upgraded source payload",
    )
    .expect("failed to change source upgrade payload");

    let updated = run_cli_without_agent_dir(&temp, &["update", "--install-id", alias]);
    let message = output_text(&updated);
    assert!(
        !updated.status.success(),
        "update unexpectedly accepted malformed installed topology version 1.2:\n{message}"
    );
    let diagnostic = message.to_ascii_lowercase();
    assert!(
        diagnostic.contains("1.2")
            && diagnostic.contains("installed")
            && (diagnostic.contains("invalid") || diagnostic.contains("semver")),
        "diagnostic must identify the invalid installed SemVer and its source:\n{message}"
    );
    assert_eq!(file_snapshot(&agent_dir), target_before);
    assert_eq!(
        fs::read(&manifest_path).expect("manifest is missing"),
        manifest_before,
        "invalid installed version changed the manifest"
    );
    assert_eq!(
        fs::read(&registry_path).expect("registry is missing"),
        registry_before,
        "invalid installed version changed the alias registry"
    );
}
fn local_topology_source_with_version(
    temp: &TestDir,
    config: &str,
    files: &[(&str, &[u8])],
    version: &str,
) -> PathBuf {
    let source = local_topology_source(temp, config, files);
    let topology_file = source
        .join("topologies")
        .join(TOPOLOGY)
        .join("topology.yml");
    fs::write(
        topology_file,
        format!(
            "name: {TOPOLOGY}\nversion: {version}\ndescription: installer fixture\ninstallable: true\n"
        ),
    )
    .expect("failed to set local topology version");
    source
}

#[test]
fn install_with_matching_version_writes_payload_and_manifest_version() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let payload = b"versioned topology payload";
    let source = local_topology_source_with_version(
        &temp,
        "fixture: true\n",
        &[("payload.txt", payload)],
        "1.2.3",
    );

    let installed = install_from_source(&temp, &agent_dir, &source, &["--version", "1.2.3"]);
    assert_success(&installed);

    assert_eq!(
        fs::read(agent_dir.join("payload.txt")).expect("installed payload is missing"),
        payload
    );
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(manifest_path(&temp, &agent_dir)).expect("manifest is missing"),
    )
    .expect("manifest is not valid JSON");
    assert_eq!(manifest["topology_version"], "1.2.3");
}

#[test]
fn install_with_mismatched_version_fails_before_mutation() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let existing = agent_dir.join("existing.txt");
    fs::write(&existing, b"pre-existing target file").expect("failed to seed target file");
    let source = local_topology_source_with_version(
        &temp,
        "fixture: true\n",
        &[("payload.txt", b"source topology payload")],
        "1.2.3",
    );
    let source_topology = source.join("topologies").join(TOPOLOGY);
    let source_metadata_before =
        fs::read(source_topology.join("topology.yml")).expect("source metadata is missing");
    let source_payload_before =
        fs::read(source_topology.join("agent/payload.txt")).expect("source payload is missing");
    let target_files_before = files_under(&agent_dir);
    let state_before = state_snapshot_without_manifest(&temp.state_home());
    let registry = temp.state_home().join("omp-topology/registry.json");

    let installed = install_from_source(&temp, &agent_dir, &source, &["--version", "2.0.0"]);
    let message = output_text(&installed);
    assert!(
        !installed.status.success(),
        "install unexpectedly accepted requested version 2.0.0 for topology version 1.2.3:\n{message}"
    );
    assert!(
        message.contains("2.0.0") && message.contains("1.2.3"),
        "version mismatch diagnostic must include requested version 2.0.0 and actual version 1.2.3:\n{message}"
    );

    assert_eq!(
        fs::read(source_topology.join("topology.yml")).expect("source metadata disappeared"),
        source_metadata_before,
        "version mismatch changed source metadata"
    );
    assert_eq!(
        fs::read(source_topology.join("agent/payload.txt")).expect("source payload disappeared"),
        source_payload_before,
        "version mismatch changed source payload"
    );
    assert_eq!(files_under(&agent_dir), target_files_before);
    assert_eq!(
        fs::read(&existing).expect("target file disappeared"),
        b"pre-existing target file",
        "version mismatch changed target file"
    );
    assert!(
        !manifest_path(&temp, &agent_dir).exists(),
        "version mismatch wrote an install manifest"
    );
    assert!(!registry.exists(), "version mismatch wrote the registry");
    assert_eq!(
        state_snapshot_without_manifest(&temp.state_home()),
        state_before,
        "version mismatch changed installer state"
    );
    assert!(
        !temp.state_home().exists(),
        "rejected install created the previously absent state root"
    );
}

#[test]
fn install_with_invalid_requested_version_fails_before_mutation() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let existing = agent_dir.join("existing.txt");
    fs::write(&existing, b"pre-existing target file").expect("failed to seed target file");
    let source = local_topology_source_with_version(
        &temp,
        "fixture: true\n",
        &[("payload.txt", b"source topology payload")],
        "1.2.3",
    );
    let source_topology = source.join("topologies").join(TOPOLOGY);
    let source_metadata_before =
        fs::read(source_topology.join("topology.yml")).expect("source metadata is missing");
    let source_payload_before =
        fs::read(source_topology.join("agent/payload.txt")).expect("source payload is missing");
    let target_files_before = files_under(&agent_dir);
    let state_before = state_snapshot_without_manifest(&temp.state_home());
    let registry = temp.state_home().join("omp-topology/registry.json");

    let installed = install_from_source(&temp, &agent_dir, &source, &["--version", "not-semver"]);
    let message = output_text(&installed);
    assert!(
        !installed.status.success(),
        "install unexpectedly accepted invalid requested version not-semver:\n{message}"
    );
    let diagnostic = message.to_ascii_lowercase();
    assert!(
        diagnostic.contains("not-semver")
            && (diagnostic.contains("semver") || diagnostic.contains("semantic version")),
        "invalid requested version diagnostic must identify not-semver as invalid SemVer:\n{message}"
    );

    assert_eq!(
        fs::read(source_topology.join("topology.yml")).expect("source metadata disappeared"),
        source_metadata_before,
        "invalid requested version changed source metadata"
    );
    assert_eq!(
        fs::read(source_topology.join("agent/payload.txt")).expect("source payload disappeared"),
        source_payload_before,
        "invalid requested version changed source payload"
    );
    assert_eq!(files_under(&agent_dir), target_files_before);
    assert_eq!(
        fs::read(&existing).expect("target file disappeared"),
        b"pre-existing target file",
        "invalid requested version changed target file"
    );
    assert!(
        !manifest_path(&temp, &agent_dir).exists(),
        "invalid requested version wrote an install manifest"
    );
    assert!(
        !registry.exists(),
        "invalid requested version wrote the registry"
    );
    assert_eq!(
        state_snapshot_without_manifest(&temp.state_home()),
        state_before,
        "invalid requested version changed installer state"
    );
}

#[test]
fn install_without_version_rejects_invalid_topology_metadata() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let existing = agent_dir.join("existing.txt");
    fs::write(&existing, b"pre-existing target file").expect("failed to seed target file");
    let source = local_topology_source_with_version(
        &temp,
        "fixture: true\n",
        &[("payload.txt", b"source topology payload")],
        "1.2",
    );
    let source_topology = source.join("topologies").join(TOPOLOGY);
    let source_metadata_before =
        fs::read(source_topology.join("topology.yml")).expect("source metadata is missing");
    let source_payload_before =
        fs::read(source_topology.join("agent/payload.txt")).expect("source payload is missing");
    let target_files_before = files_under(&agent_dir);
    let state_before = state_snapshot_without_manifest(&temp.state_home());
    let registry = temp.state_home().join("omp-topology/registry.json");

    let installed = install_from_source(&temp, &agent_dir, &source, &[]);
    let message = output_text(&installed);
    assert!(
        !installed.status.success(),
        "install unexpectedly accepted invalid declared topology version 1.2:\n{message}"
    );
    let diagnostic = message.to_ascii_lowercase();
    assert!(
        diagnostic.contains("1.2")
            && (diagnostic.contains("invalid") || diagnostic.contains("semver")),
        "invalid topology version diagnostic must identify declared version 1.2 as invalid:\n{message}"
    );

    assert_eq!(
        fs::read(source_topology.join("topology.yml")).expect("source metadata disappeared"),
        source_metadata_before,
        "invalid topology metadata was changed"
    );
    assert_eq!(
        fs::read(source_topology.join("agent/payload.txt")).expect("source payload disappeared"),
        source_payload_before,
        "invalid topology metadata changed source payload"
    );
    assert_eq!(files_under(&agent_dir), target_files_before);
    assert_eq!(
        fs::read(&existing).expect("target file disappeared"),
        b"pre-existing target file",
        "invalid topology metadata changed target file"
    );
    assert!(
        !manifest_path(&temp, &agent_dir).exists(),
        "invalid topology metadata wrote an install manifest"
    );
    assert!(
        !registry.exists(),
        "invalid topology metadata wrote the registry"
    );
    assert_eq!(
        state_snapshot_without_manifest(&temp.state_home()),
        state_before,
        "invalid topology metadata changed installer state"
    );
    assert!(
        !temp.state_home().exists(),
        "rejected install created the previously absent state root"
    );
}

#[test]
fn config_yaml_default_update_preserves_deleted_config_and_candidate_ownership() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let source = local_topology_source(&temp, "version: first\n", &[]);
    let installed = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &["--install-id", "deleted-config"],
    );
    assert_success(&installed);
    let active = agent_dir.join("config.yml");
    assert!(
        active.is_file(),
        "initial install did not create config.yml"
    );

    fs::remove_file(&active).expect("failed to simulate user deleting config.yml");
    let source_config = source
        .join("topologies")
        .join(TOPOLOGY)
        .join("agent/config.yml");
    fs::write(&source_config, "version: second\n").expect("failed to update topology config");

    let first_update =
        run_cli_without_agent_dir(&temp, &["update", "--install-id", "deleted-config"]);
    assert_success(&first_update);
    assert!(
        !active.exists(),
        "update recreated a config.yml the user had deleted"
    );
    assert_eq!(
        fs::read(agent_dir.join("config.yml.new")).expect("update omitted config candidate"),
        b"version: second\n"
    );
    assert!(
        output_text(&first_update).contains("config.yml.new"),
        "update did not report the candidate chosen after active config deletion:\n{}",
        output_text(&first_update)
    );

    fs::write(&source_config, "version: third\n").expect("failed to update topology config");
    let second_update =
        run_cli_without_agent_dir(&temp, &["update", "--install-id", "deleted-config"]);
    assert_success(&second_update);
    assert_eq!(
        fs::read(agent_dir.join("config.yml.new")).expect("candidate disappeared"),
        b"version: third\n",
        "update did not refresh its unchanged candidate"
    );

    let edited_candidate = b"version: chosen-locally\n";
    fs::write(agent_dir.join("config.yml.new"), edited_candidate)
        .expect("failed to edit config candidate");
    fs::write(&source_config, "version: fourth\n").expect("failed to update topology config");
    let collision = run_cli_without_agent_dir(&temp, &["update", "--install-id", "deleted-config"]);
    assert!(
        !collision.status.success(),
        "update overwrote a user-edited config candidate"
    );
    assert_eq!(
        fs::read(agent_dir.join("config.yml.new")).expect("edited candidate disappeared"),
        edited_candidate,
        "update changed the user's candidate after reporting a collision"
    );

    let removed = run_cli_without_agent_dir(&temp, &["remove", "--install-id", "deleted-config"]);
    assert_success(&removed);
    assert_eq!(
        fs::read(agent_dir.join("config.yml.new")).expect("remove deleted an edited candidate"),
        edited_candidate,
        "remove did not preserve a user-edited candidate"
    );
}

#[test]
fn config_yaml_merge_reconciles_topology_changes_and_preserves_profile_edits() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let config = agent_dir.join("config.yml");
    fs::write(
        &config,
        "# profile note\nsettings:\n  eager: never\n  editable: source-value\nunrelated: keep-me\n",
    )
    .expect("failed to seed profile config");
    let source = local_topology_source(
        &temp,
        "settings:\n  eager: always\n  added: first\n  removedBySource: source-value\n  editable: source-value\n",
        &[],
    );

    let installed = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &["--install-id", "merge-lifecycle", "--config-merge"],
    );
    assert_success(&installed);
    let merged = fs::read_to_string(&config).expect("failed to read merged config");
    for preserved in ["# profile note", "eager: never", "unrelated: keep-me"] {
        assert!(
            merged.contains(preserved),
            "merge did not preserve {preserved:?}:\n{merged}"
        );
    }
    assert!(
        merged.contains("added: first"),
        "merge omitted a topology setting:\n{merged}"
    );

    let local_edit = merged.replace("editable: source-value", "editable: changed-locally");
    assert_ne!(
        local_edit, merged,
        "test fixture did not contain the expected setting"
    );
    fs::write(&config, local_edit).expect("failed to make a local edit to a topology setting");
    let source_config = source
        .join("topologies")
        .join(TOPOLOGY)
        .join("agent/config.yml");
    fs::write(
        &source_config,
        "settings:\n  eager: updated-topology\n  added: second\n  newSetting: introduced\n",
    )
    .expect("failed to update topology config");

    let updated = run_cli_without_agent_dir(
        &temp,
        &[
            "update",
            "--install-id",
            "merge-lifecycle",
            "--config-merge",
        ],
    );
    assert_success(&updated);
    let reconciled = fs::read_to_string(&config).expect("failed to read reconciled config");
    for preserved in [
        "# profile note",
        "eager: never",
        "added: second",
        "newSetting: introduced",
        "editable: changed-locally",
        "unrelated: keep-me",
    ] {
        assert!(
            reconciled.contains(preserved),
            "merge update omitted {preserved:?}:\n{reconciled}"
        );
    }
    assert!(
        !reconciled.contains("removedBySource:"),
        "merge retained an unchanged key dropped by the topology:\n{reconciled}"
    );
}

#[test]
fn config_yaml_merge_precedence_is_selected_for_that_command() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let config = agent_dir.join("config.yml");
    fs::write(&config, "# profile comment\nsettings:\n  eager: profile\n")
        .expect("failed to seed profile config");
    let source = local_topology_source(&temp, "settings:\n  eager: topology\n", &[]);

    let installed = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &[
            "--install-id",
            "topology-precedence",
            "--config-merge",
            "--config-precedence",
            "topology",
        ],
    );
    assert_success(&installed);
    let merged = fs::read_to_string(&config).expect("failed to read merged config");
    assert!(
        merged.contains("eager: topology"),
        "topology precedence did not win the shared key:\n{merged}"
    );
    assert!(
        merged.contains("# profile comment"),
        "merge discarded a profile comment:\n{merged}"
    );

    fs::write(
        source
            .join("topologies")
            .join(TOPOLOGY)
            .join("agent/config.yml"),
        "settings:\n  eager: next-topology\n",
    )
    .expect("failed to update topology config");
    let default_mode_update =
        run_cli_without_agent_dir(&temp, &["update", "--install-id", "topology-precedence"]);
    assert_success(&default_mode_update);
    assert_eq!(
        fs::read(&config).expect("active config disappeared"),
        merged.as_bytes(),
        "update silently remembered the prior merge/precedence mode"
    );
    assert_eq!(
        fs::read(agent_dir.join("config.yml.new")).expect("default update omitted candidate"),
        b"settings:\n  eager: next-topology\n"
    );
}

#[test]
fn config_yaml_replace_restores_unchanged_replacement_and_preserves_later_edits() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let config = agent_dir.join("config.yml");
    let original = b"# prior profile config\nprofileOnly: keep\n";
    fs::write(&config, original).expect("failed to seed profile config");
    let source = local_topology_source(&temp, "topologyOnly: installed\n", &[]);

    let plan = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &[
            "--install-id",
            "replace-lifecycle",
            "--config-replace",
            "--dry-run",
        ],
    );
    assert_success(&plan);
    let plan_text = output_text(&plan);
    assert!(
        plan_text.contains("topologyOnly"),
        "dry-run did not show the planned config replacement:\n{plan_text}"
    );
    assert_eq!(
        fs::read(&config).expect("dry-run changed the active config"),
        original,
        "replacement dry-run modified config.yml"
    );

    let replaced = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &["--install-id", "replace-lifecycle", "--config-replace"],
    );
    assert_success(&replaced);
    let replacement = fs::read_to_string(&config).expect("replacement config disappeared");
    assert!(
        replacement.contains("topologyOnly: installed"),
        "replace did not publish the topology config:\n{replacement}"
    );
    assert!(
        !replacement.contains("profileOnly"),
        "replace retained unrelated profile config:\n{replacement}"
    );

    let removed =
        run_cli_without_agent_dir(&temp, &["remove", "--install-id", "replace-lifecycle"]);
    assert_success(&removed);
    assert_eq!(
        fs::read(&config).expect("remove did not restore the prior config"),
        original,
        "remove failed to restore the unchanged replacement's previous config"
    );

    let edited_temp = TestDir::new();
    let edited_agent = edited_temp.0.join("agent");
    fs::create_dir_all(&edited_agent).expect("failed to create isolated agent directory");
    let edited_config = edited_agent.join("config.yml");
    fs::write(&edited_config, original).expect("failed to seed profile config");
    let edited_source = local_topology_source(&edited_temp, "topologyOnly: installed\n", &[]);
    let edited_install = install_from_source(
        &edited_temp,
        &edited_agent,
        &edited_source,
        &["--install-id", "edited-replacement", "--config-replace"],
    );
    assert_success(&edited_install);

    let local_config = b"topologyOnly: edited-after-install\n";
    fs::write(&edited_config, local_config).expect("failed to edit replacement");
    let edited_remove = run_cli_without_agent_dir(
        &edited_temp,
        &["remove", "--install-id", "edited-replacement"],
    );
    assert_success(&edited_remove);
    assert_eq!(
        fs::read(&edited_config).expect("remove deleted a locally edited replacement"),
        local_config,
        "remove overwrote the user's post-replacement config edit"
    );
    assert!(
        output_text(&edited_remove)
            .to_lowercase()
            .contains("backup"),
        "remove did not report how to recover the prior config:\n{}",
        output_text(&edited_remove)
    );
}

#[cfg(unix)]
#[test]
fn main_to_topology_directory_symlink_updates_only_declared_files_and_survives_remove() {
    let temp = TestDir::new();
    let home = temp.0.join("home");
    let omp_home = home.join(".omp");
    let agent_dir = omp_home.join("agent");
    let topology_agent = omp_home.join("profiles").join(TOPOLOGY).join("agent");
    let linked_target = topology_agent.join("sessions");
    fs::create_dir_all(&agent_dir).expect("failed to create main agent directory");
    fs::create_dir_all(&linked_target).expect("failed to create topology target directory");
    let user_file = linked_target.join("user-notes.md");
    fs::write(&user_file, b"leave this user data alone\n").expect("failed to seed user data");
    let link = agent_dir.join("sessions");
    symlink(&linked_target, &link).expect("failed to create main-to-topology directory link");

    let source = local_topology_source(
        &temp,
        "setting: value\n",
        &[("sessions/managed.md", b"source version one\n")],
    );
    let installed = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &["--install-id", "main-topology-link"],
    );
    assert_success(&installed);
    assert!(link.is_symlink(), "install replaced the main-profile link");
    assert_eq!(
        fs::read(linked_target.join("managed.md")).expect("managed file was not installed"),
        b"source version one\n"
    );
    assert_eq!(
        fs::read(&user_file).expect("unowned target file disappeared"),
        b"leave this user data alone\n",
        "install changed an undeclared file in the symlink target"
    );

    let source_agent = source.join("topologies").join(TOPOLOGY).join("agent");
    fs::write(
        source_agent.join("sessions/managed.md"),
        b"source version two\n",
    )
    .expect("failed to update the topology file");
    fs::write(source_agent.join("sessions/added.md"), b"newly declared\n")
        .expect("failed to add a topology file");
    let update =
        run_cli_without_agent_dir(&temp, &["update", "--install-id", "main-topology-link"]);
    assert_success(&update);
    assert!(link.is_symlink(), "update replaced the main-profile link");
    assert_eq!(
        fs::read(linked_target.join("managed.md")).expect("managed file disappeared"),
        b"source version two\n",
        "update did not write the declared file at the symlink target"
    );
    assert_eq!(
        fs::read(linked_target.join("added.md")).expect("new declared file was not installed"),
        b"newly declared\n"
    );
    assert_eq!(
        fs::read(&user_file).expect("unowned target file disappeared"),
        b"leave this user data alone\n"
    );

    fs::write(linked_target.join("managed.md"), b"user edit\n")
        .expect("failed to edit the installed target file");
    fs::remove_file(source_agent.join("sessions/managed.md"))
        .expect("failed to drop the managed source file");
    fs::remove_file(source_agent.join("sessions/added.md"))
        .expect("failed to drop the other managed source file");
    let dropped =
        run_cli_without_agent_dir(&temp, &["update", "--install-id", "main-topology-link"]);
    assert_success(&dropped);
    assert_eq!(
        fs::read(linked_target.join("managed.md")).expect("update deleted a user-edited file"),
        b"user edit\n",
        "update deleted a modified topology-owned file"
    );
    assert!(
        !linked_target.join("added.md").exists(),
        "update retained an unchanged topology-owned file dropped by the source"
    );
    assert_eq!(
        fs::read(&user_file).expect("unowned target file disappeared"),
        b"leave this user data alone\n"
    );

    let removed =
        run_cli_without_agent_dir(&temp, &["remove", "--install-id", "main-topology-link"]);
    assert_success(&removed);
    assert!(link.is_symlink(), "remove deleted the main-profile symlink");
    assert!(
        linked_target.is_dir(),
        "remove deleted the symlink's target directory"
    );
    assert_eq!(
        fs::read(linked_target.join("managed.md")).expect("remove deleted a user edit"),
        b"user edit\n"
    );
    assert_eq!(
        fs::read(&user_file).expect("remove deleted unowned target data"),
        b"leave this user data alone\n"
    );
}
#[cfg(unix)]
#[test]
fn update_dry_run_reports_resolved_directory_symlink_target_for_dropped_file() {
    let temp = TestDir::new();
    let home = temp.0.join("home");
    let omp_home = home.join(".omp");
    let agent_dir = omp_home.join("agent");
    let topology_agent = omp_home.join("profiles").join(TOPOLOGY).join("agent");
    let linked_target = topology_agent.join("sessions");
    fs::create_dir_all(&agent_dir).expect("failed to create main agent directory");
    fs::create_dir_all(&linked_target).expect("failed to create selected-profile target");
    let link = agent_dir.join("sessions");
    symlink(&linked_target, &link).expect("failed to create main-to-topology directory link");

    let source = local_topology_source(
        &temp,
        "setting: value\n",
        &[("sessions/dropped.md", b"source file\n")],
    );
    let installed = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &["--install-id", "dry-run-linked-update"],
    );
    assert_success(&installed);

    let target_file = linked_target.join("dropped.md");
    let target_before = fs::read(&target_file).expect("installed target file is missing");
    let config = agent_dir.join("config.yml");
    let config_before = fs::read(&config).expect("installed config is missing");
    let manifest = manifest_path(&temp, &agent_dir);
    let manifest_before = fs::read(&manifest).expect("ownership manifest is missing");
    let source_agent = source.join("topologies").join(TOPOLOGY).join("agent");
    fs::remove_file(source_agent.join("sessions/dropped.md"))
        .expect("failed to drop the declared source file");

    let dry_run = run_cli_without_agent_dir(
        &temp,
        &[
            "update",
            "--install-id",
            "dry-run-linked-update",
            "--dry-run",
        ],
    );
    assert_success(&dry_run);
    let report = output_text(&dry_run);
    assert!(
        link.is_symlink(),
        "update dry-run replaced the directory symlink"
    );
    assert_eq!(
        fs::read(&target_file).expect("update dry-run deleted the target file"),
        target_before,
        "update dry-run mutated the target file"
    );
    assert_eq!(
        fs::read(&config).expect("update dry-run deleted config.yml"),
        config_before,
        "update dry-run mutated config.yml"
    );
    assert_eq!(
        fs::read(&manifest).expect("update dry-run deleted the ownership manifest"),
        manifest_before,
        "update dry-run mutated ownership state"
    );
    assert!(
        report.contains(&target_file.display().to_string()),
        "update dry-run did not report the resolved selected-profile deletion target {}:\n{report}",
        target_file.display()
    );
}

#[cfg(unix)]
#[test]
fn remove_dry_run_reports_resolved_directory_symlink_target() {
    let temp = TestDir::new();
    let home = temp.0.join("home");
    let omp_home = home.join(".omp");
    let agent_dir = omp_home.join("agent");
    let topology_agent = omp_home.join("profiles").join(TOPOLOGY).join("agent");
    let linked_target = topology_agent.join("sessions");
    fs::create_dir_all(&agent_dir).expect("failed to create main agent directory");
    fs::create_dir_all(&linked_target).expect("failed to create selected-profile target");
    let link = agent_dir.join("sessions");
    symlink(&linked_target, &link).expect("failed to create main-to-topology directory link");

    let source = local_topology_source(
        &temp,
        "setting: value\n",
        &[("sessions/managed.md", b"managed file\n")],
    );
    let installed = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &["--install-id", "dry-run-linked-remove"],
    );
    assert_success(&installed);

    let target_file = linked_target.join("managed.md");
    let target_before = fs::read(&target_file).expect("installed target file is missing");
    let config = agent_dir.join("config.yml");
    let config_before = fs::read(&config).expect("installed config is missing");
    let manifest = manifest_path(&temp, &agent_dir);
    let manifest_before = fs::read(&manifest).expect("ownership manifest is missing");

    let dry_run = run_cli_without_agent_dir(
        &temp,
        &[
            "remove",
            "--install-id",
            "dry-run-linked-remove",
            "--dry-run",
        ],
    );
    assert_success(&dry_run);
    let report = output_text(&dry_run);
    assert!(
        link.is_symlink(),
        "remove dry-run replaced the directory symlink"
    );
    assert_eq!(
        fs::read(&target_file).expect("remove dry-run deleted the target file"),
        target_before,
        "remove dry-run mutated the target file"
    );
    assert_eq!(
        fs::read(&config).expect("remove dry-run deleted config.yml"),
        config_before,
        "remove dry-run mutated config.yml"
    );
    assert_eq!(
        fs::read(&manifest).expect("remove dry-run deleted the ownership manifest"),
        manifest_before,
        "remove dry-run mutated ownership state"
    );
    assert!(
        report.contains(&target_file.display().to_string()),
        "remove dry-run did not report the resolved selected-profile file target {}:\n{report}",
        target_file.display()
    );
}

#[cfg(unix)]
#[test]
fn main_to_topology_file_symlink_updates_declared_target_and_preserves_sibling() {
    let temp = TestDir::new();
    let home = temp.0.join("home");
    let omp_home = home.join(".omp");
    let agent_dir = omp_home.join("agent");
    let topology_agent = omp_home.join("profiles").join(TOPOLOGY).join("agent");
    let linked_target = topology_agent.join("RULES.md");
    let initial_contents = b"source version one\n";
    fs::create_dir_all(&agent_dir).expect("failed to create main agent directory");
    fs::create_dir_all(&topology_agent).expect("failed to create topology agent directory");
    fs::write(&linked_target, initial_contents).expect("failed to seed topology target file");
    let user_file = topology_agent.join("user-notes.md");
    let user_contents = b"leave this user data alone\n";
    fs::write(&user_file, user_contents).expect("failed to seed target sibling data");
    let link = agent_dir.join("RULES.md");
    symlink(&linked_target, &link).expect("failed to create main-to-topology file link");

    let source =
        local_topology_source(&temp, "setting: value\n", &[("RULES.md", initial_contents)]);
    let source_file = source
        .join("topologies")
        .join(TOPOLOGY)
        .join("agent/RULES.md");
    assert_eq!(
        fs::read(&linked_target).expect("topology target file disappeared"),
        fs::read(&source_file).expect("topology source file disappeared"),
        "target file must exist and match the topology source before install"
    );

    let installed = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &["--install-id", "main-topology-file-link"],
    );
    assert_success(&installed);
    assert!(
        link.is_symlink(),
        "install replaced the main-profile file link"
    );
    assert_eq!(
        fs::read_link(&link).expect("install removed the main-profile file link"),
        linked_target
    );
    assert_eq!(
        fs::read(&linked_target).expect("install removed the topology target file"),
        initial_contents
    );
    assert_eq!(
        fs::read(&user_file).expect("install removed unowned target sibling data"),
        user_contents,
        "install changed an undeclared sibling file in the symlink target"
    );

    let updated_contents = b"source version two\n";
    fs::write(&source_file, updated_contents).expect("failed to update the topology source");
    let update = run_cli_without_agent_dir(
        &temp,
        &["update", "--install-id", "main-topology-file-link"],
    );
    assert_success(&update);
    assert!(
        link.is_symlink(),
        "update replaced the main-profile file link"
    );
    assert_eq!(
        fs::read_link(&link).expect("update removed the main-profile file link"),
        linked_target
    );
    assert_eq!(
        fs::read(&linked_target).expect("topology target file disappeared"),
        updated_contents,
        "update did not write the declared file at the symlink target"
    );
    assert_eq!(
        fs::read(&user_file).expect("update removed unowned target sibling data"),
        user_contents,
        "update changed an undeclared sibling file in the symlink target"
    );
    let removed = run_cli_without_agent_dir(
        &temp,
        &["remove", "--install-id", "main-topology-file-link"],
    );
    assert_success(&removed);
    assert!(
        link.is_symlink(),
        "remove deleted the main-profile file link"
    );
    assert_eq!(
        fs::read_link(&link).expect("remove deleted the main-profile file link"),
        linked_target
    );
    assert_eq!(
        fs::read(&linked_target).expect("remove deleted the pre-existing topology target file"),
        updated_contents,
        "remove changed the pre-existing target file from the latest topology version"
    );
    assert_eq!(
        fs::read(&user_file).expect("remove deleted unowned target sibling data"),
        user_contents,
        "remove changed an undeclared sibling file in the symlink target"
    );
    let manifest = manifest_path(&temp, &agent_dir);
    assert!(
        !manifest.exists(),
        "remove left ownership state after releasing all pre-existing files"
    );
}

#[cfg(unix)]
#[test]
fn remove_rejects_retargeted_adopted_file_symlink_before_mutation() {
    let temp = TestDir::new();
    let home = temp.0.join("home");
    let omp_home = home.join(".omp");
    let agent_dir = omp_home.join("agent");
    let topology_agent = omp_home.join("profiles").join(TOPOLOGY).join("agent");
    let old_target = topology_agent.join("RULES.md");
    let new_target = topology_agent.join("alternate-RULES.md");
    let old_contents = b"matching pre-existing topology file\n";
    let new_contents = b"replacement profile file\n";
    fs::create_dir_all(&agent_dir).expect("failed to create main agent directory");
    fs::create_dir_all(&topology_agent).expect("failed to create topology agent directory");
    fs::write(&old_target, old_contents).expect("failed to seed adopted target file");
    fs::write(&new_target, new_contents).expect("failed to seed replacement target file");
    let link = agent_dir.join("RULES.md");
    symlink(&old_target, &link).expect("failed to create main-to-topology file link");

    let source = local_topology_source(&temp, "setting: value\n", &[("RULES.md", old_contents)]);
    let installed = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &["--install-id", "remove-retargeted-adopted-file-link"],
    );
    assert_success(&installed);
    assert!(
        link.is_symlink(),
        "install replaced the main-profile file link"
    );
    assert_eq!(
        fs::read(&old_target).expect("install removed the adopted target file"),
        old_contents
    );
    let manifest = manifest_path(&temp, &agent_dir);
    let manifest_before = fs::read(&manifest).expect("ownership manifest is missing");
    let manifest_json: serde_json::Value =
        serde_json::from_slice(&manifest_before).expect("ownership manifest is not valid JSON");
    assert!(
        manifest_json
            .get("preexisting")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| items.iter().any(|item| item.as_str() == Some("RULES.md"))),
        "install did not record the matching file symlink target as pre-existing"
    );
    let config = agent_dir.join("config.yml");
    let config_before = fs::read(&config).expect("installed config is missing");
    let old_target_before = fs::read(&old_target).expect("adopted target file is missing");
    let new_target_before = fs::read(&new_target).expect("replacement target file is missing");

    fs::remove_file(&link).expect("failed to remove original link");
    symlink(&new_target, &link).expect("failed to retarget main-profile link");

    let removed = run_cli_without_agent_dir(
        &temp,
        &[
            "remove",
            "--install-id",
            "remove-retargeted-adopted-file-link",
        ],
    );
    assert!(
        !removed.status.success(),
        "remove accepted a retargeted adopted file symlink:\n{}",
        output_text(&removed)
    );
    assert_eq!(
        fs::read(&manifest).expect("remove deleted the ownership manifest"),
        manifest_before,
        "remove changed the ownership manifest before rejecting the retargeted link"
    );
    assert_eq!(
        fs::read(&config).expect("remove deleted the installed config"),
        config_before,
        "remove changed config before rejecting the retargeted link"
    );
    assert_eq!(
        fs::read(&old_target).expect("remove deleted the adopted target file"),
        old_target_before,
        "remove changed the previously recorded target"
    );
    assert_eq!(
        fs::read(&new_target).expect("remove deleted the replacement target file"),
        new_target_before,
        "remove changed the newly selected target"
    );
    assert!(link.is_symlink(), "remove deleted the retargeted symlink");
    assert_eq!(
        fs::read_link(&link).expect("remove removed the retargeted symlink"),
        new_target,
        "remove changed the retargeted symlink destination"
    );
}

#[cfg(unix)]
#[test]
fn remove_rejects_malformed_preexisting_manifest_without_mutation() {
    let temp = TestDir::new();
    let home = temp.0.join("home");
    let omp_home = home.join(".omp");
    let agent_dir = omp_home.join("agent");
    let topology_agent = omp_home.join("profiles").join(TOPOLOGY).join("agent");
    let linked_target = topology_agent.join("RULES.md");
    let initial_contents = b"matching pre-existing topology file\n";
    fs::create_dir_all(&agent_dir).expect("failed to create main agent directory");
    fs::create_dir_all(&topology_agent).expect("failed to create topology agent directory");
    fs::write(&linked_target, initial_contents).expect("failed to seed topology target file");
    let link = agent_dir.join("RULES.md");
    symlink(&linked_target, &link).expect("failed to create main-to-topology file link");

    let source =
        local_topology_source(&temp, "setting: value\n", &[("RULES.md", initial_contents)]);
    let installed = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &["--install-id", "malformed-preexisting-file-link"],
    );
    assert_success(&installed);
    assert!(
        link.is_symlink(),
        "install replaced the main-profile file link"
    );
    assert_eq!(
        fs::read(&linked_target).expect("install removed the topology target file"),
        initial_contents
    );

    let manifest = manifest_path(&temp, &agent_dir);
    let mut manifest_json: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).expect("ownership manifest is missing"))
            .expect("ownership manifest is not valid JSON");
    assert!(
        manifest_json
            .get("preexisting")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| items.iter().any(|item| item.as_str() == Some("RULES.md"))),
        "install did not record the adopted target as pre-existing"
    );
    manifest_json["preexisting"] = serde_json::json!({"RULES.md": true});
    fs::write(
        &manifest,
        serde_json::to_vec_pretty(&manifest_json).expect("failed to serialize malformed manifest"),
    )
    .expect("failed to corrupt preexisting manifest metadata");

    let target_before = fs::read(&linked_target).expect("topology target file is missing");
    let config = agent_dir.join("config.yml");
    let config_before = fs::read(&config).expect("installed config is missing");
    let manifest_before = fs::read(&manifest).expect("malformed ownership manifest is missing");
    let removed = run_cli_without_agent_dir(
        &temp,
        &["remove", "--install-id", "malformed-preexisting-file-link"],
    );
    assert!(
        !removed.status.success(),
        "remove accepted malformed preexisting metadata:\n{}",
        output_text(&removed)
    );
    assert_eq!(
        fs::read(&linked_target).expect("remove deleted the pre-existing topology target file"),
        target_before,
        "remove mutated the pre-existing target file"
    );
    assert_eq!(
        fs::read(&config).expect("remove deleted config.yml"),
        config_before,
        "remove mutated config after rejecting malformed metadata"
    );
    assert_eq!(
        fs::read(&manifest).expect("remove deleted the malformed ownership manifest"),
        manifest_before,
        "remove changed the malformed ownership manifest"
    );
}

#[cfg(unix)]
#[test]
fn main_to_different_profile_symlink_fails_before_config_or_target_mutation() {
    let temp = TestDir::new();
    let home = temp.0.join("home");
    let omp_home = home.join(".omp");
    let agent_dir = omp_home.join("agent");
    let selected_target = omp_home
        .join("profiles")
        .join(TOPOLOGY)
        .join("agent/sessions");
    let other_target = omp_home
        .join("profiles")
        .join("another-profile")
        .join("agent/sessions");
    fs::create_dir_all(&agent_dir).expect("failed to create main agent directory");
    fs::create_dir_all(&selected_target).expect("failed to create selected profile target");
    fs::create_dir_all(&other_target).expect("failed to create other profile target");

    let config = agent_dir.join("config.yml");
    let original_config = b"existing: user config\n";
    fs::write(&config, original_config).expect("failed to seed main profile config");
    let selected_data = selected_target.join("user-data.md");
    let other_data = other_target.join("user-data.md");
    fs::write(&selected_data, b"selected profile data\n")
        .expect("failed to seed selected profile data");
    fs::write(&other_data, b"other profile data\n").expect("failed to seed other profile data");

    let link = agent_dir.join("sessions");
    symlink(&other_target, &link).expect("failed to link main agent path to other profile");
    let source = local_topology_source(
        &temp,
        "topology: config\n",
        &[("sessions/managed.md", b"must not be written\n")],
    );
    // Mirror the adjacent fixture: `-t TOPOLOGY` selects the installed
    // topology, while `--agent-dir` keeps the target at the main agent tree.
    let installed = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &["--install-id", "wrong-profile-link"],
    );

    assert!(
        !installed.status.success(),
        "install accepted a main-agent symlink into a different profile:\n{}",
        output_text(&installed)
    );
    assert!(
        link.is_symlink(),
        "failed install replaced the main-agent link"
    );
    assert_eq!(
        fs::read_link(&link).expect("failed install removed the link"),
        other_target
    );
    assert_eq!(
        fs::read(&config).expect("failed install removed the active config"),
        original_config,
        "failed install changed the active config"
    );
    assert!(
        !agent_dir.join("config.yml.new").exists(),
        "failed install wrote a config candidate before rejecting the link"
    );
    assert!(
        !selected_target.join("managed.md").exists(),
        "failed install wrote a managed file into the selected profile target"
    );
    assert_eq!(
        fs::read(&selected_data).expect("selected profile data disappeared"),
        b"selected profile data\n",
        "failed install changed data in the selected profile target"
    );
    assert!(
        !other_target.join("managed.md").exists(),
        "failed install wrote a managed file into a different profile"
    );
    assert_eq!(
        fs::read(&other_data).expect("other profile data disappeared"),
        b"other profile data\n",
        "failed install changed existing data in a different profile"
    );
}

#[cfg(unix)]
#[test]
fn install_rejects_selected_profile_agent_symlink_to_other_profile() {
    let temp = TestDir::new();
    let omp_home = temp.0.join("home/.omp");
    let agent_dir = omp_home.join("agent");
    let selected_profile_agent = omp_home.join("profiles").join(TOPOLOGY).join("agent");
    let other_profile_agent = omp_home.join("profiles/other/agent");
    let other_sessions = other_profile_agent.join("sessions");
    fs::create_dir_all(&agent_dir).expect("failed to create main agent directory");
    fs::create_dir_all(
        selected_profile_agent
            .parent()
            .expect("selected profile has no parent"),
    )
    .expect("failed to create selected profile directory");
    fs::create_dir_all(&other_sessions).expect("failed to create other profile sessions");
    symlink(&other_profile_agent, &selected_profile_agent)
        .expect("failed to link selected profile agent to other profile");

    let selected_target = selected_profile_agent.join("sessions");
    let existing_data = other_sessions.join("user-data.md");
    fs::write(&existing_data, b"other profile data\n")
        .expect("failed to seed other profile target data");
    let sessions_link = agent_dir.join("sessions");
    symlink(&selected_target, &sessions_link)
        .expect("failed to link main agent path into selected profile");
    let source = local_topology_source(
        &temp,
        "topology: config\n",
        &[("sessions/managed.md", b"must not be written\n")],
    );

    let installed = install_from_source(&temp, &agent_dir, &source, &[]);

    assert!(
        !installed.status.success(),
        "install accepted a selected-profile path redirected into another profile:\n{}",
        output_text(&installed)
    );
    assert!(
        selected_profile_agent.is_symlink(),
        "failed install replaced the selected profile agent symlink"
    );
    assert_eq!(
        fs::read_link(&selected_profile_agent).expect("selected profile agent link disappeared"),
        other_profile_agent
    );
    assert!(
        sessions_link.is_symlink(),
        "failed install replaced the main agent sessions symlink"
    );
    assert_eq!(
        fs::read_link(&sessions_link).expect("main agent sessions link disappeared"),
        selected_target
    );
    assert!(
        !agent_dir.join("config.yml").exists(),
        "failed install wrote the active config"
    );
    assert!(
        !agent_dir.join("config.yml.new").exists(),
        "failed install wrote a config candidate before rejecting the link"
    );
    assert!(
        !other_sessions.join("managed.md").exists(),
        "failed install wrote a managed file through the redirected profile symlink"
    );
    assert_eq!(
        fs::read(&existing_data).expect("other profile target data disappeared"),
        b"other profile data\n",
        "failed install changed existing data in the other profile"
    );
}

#[cfg(unix)]
#[test]
fn main_to_topology_symlink_retarget_fails_before_writing_new_destination() {
    let temp = TestDir::new();
    let omp_home = temp.0.join("home/.omp");
    let agent_dir = omp_home.join("agent");
    let old_target = omp_home
        .join("profiles")
        .join(TOPOLOGY)
        .join("agent/sessions");
    let new_target = omp_home.join("profiles/other/agent/sessions");
    fs::create_dir_all(&agent_dir).expect("failed to create main agent directory");
    fs::create_dir_all(&old_target).expect("failed to create original topology target");
    fs::create_dir_all(&new_target).expect("failed to create replacement target");
    fs::write(old_target.join("old-data"), b"original target\n")
        .expect("failed to seed original target");
    fs::write(new_target.join("new-data"), b"replacement target\n")
        .expect("failed to seed replacement target");
    let link = agent_dir.join("sessions");
    symlink(&old_target, &link).expect("failed to create initial main-to-topology link");
    let source = local_topology_source(
        &temp,
        "setting: value\n",
        &[("sessions/managed.md", b"source version one\n")],
    );
    let installed = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &["--install-id", "retargeted-link"],
    );
    assert_success(&installed);

    fs::write(
        source
            .join("topologies")
            .join(TOPOLOGY)
            .join("agent/sessions/managed.md"),
        b"source version two\n",
    )
    .expect("failed to change topology source");
    fs::remove_file(&link).expect("failed to remove original link");
    symlink(&new_target, &link).expect("failed to retarget main-profile link");
    let update = run_cli_without_agent_dir(&temp, &["update", "--install-id", "retargeted-link"]);
    assert!(
        !update.status.success(),
        "update followed a link retargeted after installation"
    );
    assert_eq!(
        fs::read(old_target.join("managed.md")).expect("original owned file disappeared"),
        b"source version one\n",
        "retargeted update changed the previously recorded destination"
    );
    assert_eq!(
        fs::read(new_target.join("new-data")).expect("replacement target data disappeared"),
        b"replacement target\n",
        "retargeted update changed the new destination"
    );
    assert!(
        !new_target.join("managed.md").exists(),
        "retargeted update wrote a managed file into the new destination"
    );
    assert!(
        link.is_symlink(),
        "failed update replaced the retargeted link"
    );
}

#[cfg(unix)]
#[test]
fn remove_rejects_retargeted_main_to_selected_profile_symlink_before_mutation() {
    let temp = TestDir::new();
    let home = temp.0.join("home");
    let omp_home = home.join(".omp");
    let agent_dir = omp_home.join("agent");
    let selected_profile = omp_home.join("profiles").join(TOPOLOGY);
    let old_target = selected_profile.join("agent/sessions");
    let new_target = selected_profile.join("agent/alternate-sessions");
    fs::create_dir_all(&agent_dir).expect("failed to create main agent directory");
    fs::create_dir_all(&old_target).expect("failed to create original selected-profile target");
    fs::create_dir_all(&new_target).expect("failed to create replacement selected-profile target");

    let old_unowned_data = old_target.join("user-data.md");
    let new_unowned_data = new_target.join("user-data.md");
    fs::write(&old_unowned_data, b"original target data\n")
        .expect("failed to seed original target data");
    fs::write(&new_unowned_data, b"replacement target data\n")
        .expect("failed to seed replacement target data");
    let link = agent_dir.join("sessions");
    symlink(&old_target, &link).expect("failed to create main-to-selected-profile link");

    let source = local_topology_source(
        &temp,
        "setting: value\n",
        &[
            ("sessions/managed.md", b"linked managed file\n"),
            ("agents/plain.md", b"plain managed file\n"),
        ],
    );
    let installed = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &["--install-id", "remove-retargeted-link"],
    );
    assert_success(&installed);
    assert_eq!(
        fs::read(old_target.join("managed.md")).expect("linked managed file was not installed"),
        b"linked managed file\n"
    );
    let plain_managed_file = agent_dir.join("agents/plain.md");
    assert_eq!(
        fs::read(&plain_managed_file).expect("plain managed file was not installed"),
        b"plain managed file\n"
    );
    let config = agent_dir.join("config.yml");
    let active_config = fs::read(&config).expect("active config was not installed");

    fs::remove_file(&link).expect("failed to remove original link");
    symlink(&new_target, &link).expect("failed to retarget main-profile link");

    let removed =
        run_cli_without_agent_dir(&temp, &["remove", "--install-id", "remove-retargeted-link"]);

    assert!(
        !removed.status.success(),
        "remove followed a main-profile link retargeted within the selected profile:\n{}",
        output_text(&removed)
    );
    assert!(
        old_target.is_dir(),
        "remove deleted the original target directory"
    );
    assert!(
        new_target.is_dir(),
        "remove deleted the replacement target directory"
    );
    assert_eq!(
        fs::read(old_target.join("managed.md")).expect("remove deleted the linked managed file"),
        b"linked managed file\n",
        "remove changed the previously recorded linked file"
    );
    assert_eq!(
        fs::read(&old_unowned_data).expect("remove deleted original target data"),
        b"original target data\n",
        "remove changed unowned data in the original target"
    );
    assert_eq!(
        fs::read(&new_unowned_data).expect("remove deleted replacement target data"),
        b"replacement target data\n",
        "remove changed unowned data in the replacement target"
    );
    assert!(
        !new_target.join("managed.md").exists(),
        "remove touched the replacement target by deleting the linked managed file there"
    );
    assert_eq!(
        fs::read(&plain_managed_file).expect("remove deleted the plain managed file"),
        b"plain managed file\n",
        "remove changed a topology-owned file outside the retargeted link"
    );
    assert_eq!(
        fs::read(&config).expect("remove deleted the active config"),
        active_config,
        "remove changed the active config before rejecting the retargeted link"
    );
    assert!(link.is_symlink(), "remove deleted the retargeted symlink");
    assert_eq!(
        fs::read_link(&link).expect("remove removed the retargeted symlink"),
        new_target,
        "remove changed the retargeted symlink destination"
    );
}

#[cfg(unix)]
#[test]
fn main_to_topology_symlink_rejects_dangling_and_external_destinations() {
    for external in [false, true] {
        let temp = TestDir::new();
        let omp_home = temp.0.join("home/.omp");
        let agent_dir = omp_home.join("agent");
        fs::create_dir_all(&agent_dir).expect("failed to create main agent directory");
        let outside = temp.0.join("outside");
        if external {
            fs::create_dir_all(&outside).expect("failed to create external target");
            fs::write(outside.join("user-data"), b"outside data\n")
                .expect("failed to seed external target");
        }
        let target = if external {
            outside.clone()
        } else {
            omp_home
                .join("profiles")
                .join(TOPOLOGY)
                .join("agent/missing")
        };
        let link = agent_dir.join("sessions");
        symlink(&target, &link).expect("failed to create unsafe target link");
        let source = local_topology_source(
            &temp,
            "setting: value\n",
            &[("sessions/managed.md", b"must not be written\n")],
        );

        let installed = install_from_source(&temp, &agent_dir, &source, &[]);
        assert!(
            !installed.status.success(),
            "install accepted a {} symlink target",
            if external { "external" } else { "dangling" }
        );
        assert!(
            link.is_symlink(),
            "failed install replaced the user symlink"
        );
        assert!(
            !agent_dir.join("config.yml").exists(),
            "failed install partially changed the active config"
        );
        if external {
            assert_eq!(
                fs::read(outside.join("user-data")).expect("external user data disappeared"),
                b"outside data\n",
                "failed install modified external user data"
            );
            assert!(
                !outside.join("managed.md").exists(),
                "failed install wrote the topology file outside ~/.omp"
            );
        }
    }
}
#[cfg(unix)]
#[test]
fn explicit_agent_dir_rejects_sibling_profiles_symlink_outside_omp_home() {
    let temp = TestDir::new();
    let home = temp.0.join("home");
    fs::create_dir_all(&home).expect("failed to create isolated HOME");

    let agent_parent = temp.0.join("custom-agent-layout");
    let agent_dir = agent_parent.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create explicit agent directory");
    assert!(
        !agent_dir.starts_with(home.join(".omp")),
        "explicit agent directory unexpectedly lies under HOME/.omp"
    );
    let target_sessions = agent_parent
        .join("profiles")
        .join(TOPOLOGY)
        .join("agent/sessions");
    fs::create_dir_all(&target_sessions).expect("failed to create sibling profiles target");
    let unowned = target_sessions.join("unowned.md");
    let unowned_contents = b"user-owned data\n";
    fs::write(&unowned, unowned_contents).expect("failed to seed unowned target data");
    let sessions_link = agent_dir.join("sessions");
    symlink(&target_sessions, &sessions_link).expect("failed to link sessions to sibling profiles");

    let source = local_topology_source(
        &temp,
        "setting: value\n",
        &[("sessions/managed.md", b"topology-managed content\n")],
    );
    let installed = install_from_source(&temp, &agent_dir, &source, &[]);

    assert!(
        !installed.status.success(),
        "install accepted a sessions symlink to sibling profiles outside HOME/.omp:\n{}",
        output_text(&installed)
    );
    assert!(
        !agent_dir.join("config.yml").exists(),
        "failed install partially changed the active config"
    );
    assert!(
        !target_sessions.join("managed.md").exists(),
        "failed install wrote the topology-managed file through the symlink"
    );
    assert!(
        sessions_link.is_symlink(),
        "failed install replaced the sessions symlink"
    );
    assert_eq!(
        fs::read(&unowned)
            .expect("unowned target data disappeared")
            .as_slice(),
        unowned_contents,
        "failed install modified unowned target data"
    );
    assert_eq!(
        files_under(&target_sessions),
        vec![unowned],
        "failed install changed the sibling profiles target contents"
    );
}

#[cfg(unix)]
#[test]
fn install_rejects_profiles_root_symlink_outside_omp_home_before_mutation() {
    let temp = TestDir::new();
    let home = temp.0.join("home");
    let omp_home = home.join(".omp");
    let agent_dir = omp_home.join("agent");
    let external_profiles = temp.0.join("outside/profiles");
    let profiles_link = omp_home.join("profiles");
    let target_sessions = external_profiles.join(TOPOLOGY).join("agent/sessions");
    let linked_sessions = profiles_link.join(TOPOLOGY).join("agent/sessions");
    fs::create_dir_all(&agent_dir).expect("failed to create real main agent directory");
    fs::create_dir_all(&target_sessions).expect("failed to create external profile target");
    symlink(&external_profiles, &profiles_link).expect("failed to link profiles root outside");

    let original_config = b"existing: user config\n";
    let config = agent_dir.join("config.yml");
    fs::write(&config, original_config).expect("failed to seed active config");
    let sessions_link = agent_dir.join("sessions");
    symlink(&linked_sessions, &sessions_link)
        .expect("failed to link main-agent sessions through profiles root");
    let unowned = target_sessions.join("unowned.md");
    let unowned_contents = b"user-owned external data\n";
    fs::write(&unowned, unowned_contents).expect("failed to seed external profile data");

    let source = local_topology_source(
        &temp,
        "setting: topology\n",
        &[("sessions/managed.md", b"must not be written\n")],
    );
    let installed = install_from_source(&temp, &agent_dir, &source, &[]);

    assert!(
        !installed.status.success(),
        "install accepted a profiles root symlink outside HOME/.omp:\n{}",
        output_text(&installed)
    );
    assert!(
        profiles_link.is_symlink(),
        "failed install replaced profiles link"
    );
    assert_eq!(
        fs::read_link(&profiles_link).expect("failed install removed profiles link"),
        external_profiles
    );
    assert!(
        sessions_link.is_symlink(),
        "failed install replaced main-agent link"
    );
    assert_eq!(
        fs::read_link(&sessions_link).expect("failed install removed main-agent link"),
        linked_sessions
    );
    assert_eq!(
        fs::read(&config).expect("failed install removed active config"),
        original_config,
        "failed install changed active config"
    );
    assert!(
        !agent_dir.join("config.yml.new").exists(),
        "failed install wrote a config candidate before rejecting the external profiles root"
    );
    assert!(
        !target_sessions.join("managed.md").exists(),
        "failed install wrote a managed file outside HOME/.omp"
    );
    assert_eq!(
        fs::read(&unowned).expect("external profile data disappeared"),
        unowned_contents,
        "failed install changed external profile data"
    );
    assert_eq!(
        files_under(&target_sessions),
        vec![unowned],
        "failed install changed external profile target contents"
    );
}

#[cfg(unix)]
#[test]
fn install_and_update_merge_through_preexisting_selected_profile_config_link() {
    let temp = TestDir::new();
    let omp_home = temp.0.join("home/.omp");
    let agent_dir = omp_home.join("agent");
    let selected_agent = omp_home.join("profiles").join(TOPOLOGY).join("agent");
    let config = agent_dir.join("config.yml");
    let linked_target = selected_agent.join("config.yml");
    let user_only = selected_agent.join("user-only.txt");
    let other_profile_sentinel = omp_home.join("profiles/other/agent/sentinel.txt");
    let outside_sentinel = temp.0.join("outside/config.yml");
    fs::create_dir_all(&agent_dir).expect("failed to create main agent directory");
    fs::create_dir_all(&selected_agent).expect("failed to create selected profile agent directory");
    fs::create_dir_all(
        other_profile_sentinel
            .parent()
            .expect("other-profile sentinel has no parent"),
    )
    .expect("failed to create other profile agent directory");
    fs::create_dir_all(
        outside_sentinel
            .parent()
            .expect("outside sentinel has no parent"),
    )
    .expect("failed to create outside sentinel directory");

    fs::write(
        &linked_target,
        "task:\n  eager: never\nprofileOnly: preserve\n",
    )
    .expect("failed to seed selected profile config");
    let user_only_before = b"user-owned selected-profile data\n";
    fs::write(&user_only, user_only_before).expect("failed to seed user-only sentinel");
    let other_profile_before = b"other profile must remain untouched\n";
    fs::write(&other_profile_sentinel, other_profile_before)
        .expect("failed to seed other-profile sentinel");
    let outside_before = b"outside .omp must remain untouched\n";
    fs::write(&outside_sentinel, outside_before).expect("failed to seed outside sentinel");
    symlink(&linked_target, &config).expect("failed to create pre-install config symlink");

    let source =
        local_topology_source(&temp, "task:\n  eager: always\ninstallOnly: present\n", &[]);
    let alias = "preexisting-selected-profile-config-link";
    let installed = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &["--install-id", alias, "--config-merge"],
    );
    assert_success(&installed);
    assert!(
        config.is_symlink(),
        "install replaced the preexisting config link"
    );
    assert!(
        !agent_dir.join("config.yml.new").exists(),
        "install published a config candidate instead of writing through the declared config link"
    );
    assert_eq!(
        fs::read_link(&config).expect("install removed the config link"),
        linked_target
    );
    let installed_config =
        fs::read_to_string(&linked_target).expect("install did not retain selected profile config");
    for expected in [
        "eager: never",
        "profileOnly: preserve",
        "installOnly: present",
    ] {
        assert!(
            installed_config.contains(expected),
            "install merge did not preserve/add {expected:?} through the selected profile link:\n{installed_config}"
        );
    }
    assert_eq!(
        fs::read(&user_only).expect("user-only selected-profile sentinel disappeared"),
        user_only_before,
        "install changed user-only selected-profile data"
    );
    assert_eq!(
        fs::read(&other_profile_sentinel).expect("other-profile sentinel disappeared"),
        other_profile_before,
        "install changed data in another profile"
    );
    assert_eq!(
        fs::read(&outside_sentinel).expect("outside sentinel disappeared"),
        outside_before,
        "install changed data outside .omp"
    );

    let source_config = source
        .join("topologies")
        .join(TOPOLOGY)
        .join("agent/config.yml");
    fs::write(
        &source_config,
        "task:\n  eager: always\ninstallOnly: present\nupdateOnly: added\n",
    )
    .expect("failed to update topology config");
    let updated =
        run_cli_without_agent_dir(&temp, &["update", "--install-id", alias, "--config-merge"]);
    assert_success(&updated);
    assert!(
        config.is_symlink(),
        "update replaced the preexisting config link"
    );
    assert!(
        !agent_dir.join("config.yml.new").exists(),
        "update published a config candidate instead of writing through the declared config link"
    );
    assert_eq!(
        fs::read_link(&config).expect("update removed the config link"),
        linked_target
    );
    let updated_config =
        fs::read_to_string(&linked_target).expect("update lost selected profile config");
    for expected in [
        "eager: never",
        "profileOnly: preserve",
        "installOnly: present",
        "updateOnly: added",
    ] {
        assert!(
            updated_config.contains(expected),
            "update merge did not preserve/add {expected:?} through the selected profile link:\n{updated_config}"
        );
    }
    assert_eq!(
        fs::read(&user_only).expect("user-only selected-profile sentinel disappeared"),
        user_only_before,
        "update changed user-only selected-profile data"
    );
    assert_eq!(
        fs::read(&other_profile_sentinel).expect("other-profile sentinel disappeared"),
        other_profile_before,
        "update changed data in another profile"
    );
    assert_eq!(
        fs::read(&outside_sentinel).expect("outside sentinel disappeared"),
        outside_before,
        "update changed data outside .omp"
    );
}

#[cfg(unix)]
#[test]
fn config_yaml_merge_update_after_default_update_preserves_profile_value_through_symlink() {
    let temp = TestDir::new();
    let home = temp.0.join("home");
    let omp_home = home.join(".omp");
    let agent_dir = omp_home.join("agent");
    let config = agent_dir.join("config.yml");
    let linked_target = omp_home
        .join("profiles")
        .join(TOPOLOGY)
        .join("agent/config.yml");
    fs::create_dir_all(&agent_dir).expect("failed to create main agent directory");
    fs::create_dir_all(
        linked_target
            .parent()
            .expect("selected profile config has no parent"),
    )
    .expect("failed to create selected profile agent directory");

    let source = local_topology_source(&temp, "setting: initial\n", &[]);
    let installed = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &["--install-id", "config-link-default-then-merge"],
    );
    assert_success(&installed);
    let active_bytes = b"setting: initial\nprofileOnly: preserve\n";
    fs::write(&config, active_bytes).expect("failed to seed active profile config");
    fs::rename(&config, &linked_target).expect("failed to move config into selected profile");
    symlink(&linked_target, &config).expect("failed to link main config to selected profile");

    let source_config = source
        .join("topologies")
        .join(TOPOLOGY)
        .join("agent/config.yml");
    fs::write(&source_config, "setting: updated\n")
        .expect("failed to change topology config for safe-default update");
    let default_update = run_cli_without_agent_dir(
        &temp,
        &["update", "--install-id", "config-link-default-then-merge"],
    );
    assert_success(&default_update);
    assert!(
        config.is_symlink(),
        "safe-default update replaced config link"
    );
    assert_eq!(
        fs::read(&linked_target).expect("safe-default update lost active config"),
        active_bytes,
        "safe-default update changed the active profile config"
    );

    fs::write(&source_config, "setting: updated\nnewTopology: added\n")
        .expect("failed to add topology config key for merge update");
    let merge_update = run_cli_without_agent_dir(
        &temp,
        &[
            "update",
            "--install-id",
            "config-link-default-then-merge",
            "--config-merge",
        ],
    );
    assert_success(&merge_update);
    assert!(config.is_symlink(), "merge update replaced config link");
    assert_eq!(
        fs::read_link(&config).expect("merge update removed config link"),
        linked_target
    );
    let merged = fs::read_to_string(&linked_target).expect("failed to read merged target config");
    for expected in [
        "setting: initial",
        "profileOnly: preserve",
        "newTopology: added",
    ] {
        assert!(
            merged.contains(expected),
            "merge update did not preserve or add {expected:?} after safe-default update:\n{merged}"
        );
    }
}

#[test]
fn replace_config_backup_survives_non_replace_update_and_remove() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("home/.omp/agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let config = agent_dir.join("config.yml");
    let original_config = b"# original user config\nprofileOnly: preserve\n";
    fs::write(&config, original_config).expect("failed to seed original config");
    let source = local_topology_source(&temp, "topologyOnly: initial\n", &[]);
    let alias = "replace-backup-non-replace-update";
    let installed = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &["--install-id", alias, "--config-replace"],
    );
    assert_success(&installed);

    let source_config = source
        .join("topologies")
        .join(TOPOLOGY)
        .join("agent/config.yml");
    fs::write(
        &source_config,
        "topologyOnly: updated\nnewTopology: added\n",
    )
    .expect("failed to change topology config before update");
    let update = run_cli_without_agent_dir(&temp, &["update", "--install-id", alias]);
    assert_success(&update);

    let remove = run_cli_without_agent_dir(&temp, &["remove", "--install-id", alias]);
    assert_success(&remove);
    assert_eq!(
        fs::read(&config).expect("remove did not restore the pre-replacement config"),
        original_config,
        "non-replace update lost the config-replace backup"
    );
    assert!(
        !manifest_path(&temp, &agent_dir).exists(),
        "remove left the ownership manifest after restoring original config"
    );
}
#[cfg(unix)]
#[test]
fn config_yaml_default_update_preserves_main_to_profile_symlink_and_publishes_candidate() {
    let temp = TestDir::new();
    let home = temp.0.join("home");
    let omp_home = home.join(".omp");
    let agent_dir = omp_home.join("agent");
    let config = agent_dir.join("config.yml");
    let linked_target = omp_home
        .join("profiles")
        .join(TOPOLOGY)
        .join("agent/config.yml");
    fs::create_dir_all(&agent_dir).expect("failed to create main agent directory");
    fs::create_dir_all(
        linked_target
            .parent()
            .expect("selected profile config has no parent"),
    )
    .expect("failed to create selected profile agent directory");

    let topology_config = b"topology: updated candidate\n";
    let source = local_topology_source(&temp, "topology: initial\n", &[]);
    let installed = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &["--install-id", "config-link-default"],
    );
    assert_success(&installed);

    let active_bytes = b"# active profile config\nlocal: keep-this\n";
    fs::write(&config, active_bytes).expect("failed to seed active config bytes");
    fs::rename(&config, &linked_target).expect("failed to move config into selected profile");
    symlink(&linked_target, &config).expect("failed to link main config to selected profile");
    let source_config = source
        .join("topologies")
        .join(TOPOLOGY)
        .join("agent/config.yml");
    fs::write(&source_config, topology_config).expect("failed to update topology config");

    let update =
        run_cli_without_agent_dir(&temp, &["update", "--install-id", "config-link-default"]);
    assert_success(&update);
    assert!(
        config.is_symlink(),
        "update replaced the main-profile config link"
    );
    assert_eq!(
        fs::read_link(&config).expect("update removed the main-profile config link"),
        linked_target
    );
    assert_eq!(
        fs::read(&linked_target).expect("active config target disappeared"),
        active_bytes,
        "safe-default update changed active config bytes through the link"
    );
    assert_eq!(
        fs::read(agent_dir.join("config.yml.new"))
            .expect("safe-default update did not publish a config candidate"),
        topology_config,
        "candidate does not contain the topology config"
    );
}

#[cfg(unix)]
#[test]
fn config_yaml_merge_update_writes_through_main_to_profile_symlink() {
    let temp = TestDir::new();
    let home = temp.0.join("home");
    let omp_home = home.join(".omp");
    let agent_dir = omp_home.join("agent");
    let config = agent_dir.join("config.yml");
    let linked_target = omp_home
        .join("profiles")
        .join(TOPOLOGY)
        .join("agent/config.yml");
    fs::create_dir_all(&agent_dir).expect("failed to create main agent directory");
    fs::create_dir_all(
        linked_target
            .parent()
            .expect("selected profile config has no parent"),
    )
    .expect("failed to create selected profile agent directory");

    let source = local_topology_source(&temp, "setting: initial\n", &[]);
    let installed = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &["--install-id", "config-link-merge"],
    );
    assert_success(&installed);
    let active_bytes = b"setting: initial\nprofileOnly: preserve\n";
    fs::write(&config, active_bytes).expect("failed to seed profile config");
    fs::rename(&config, &linked_target).expect("failed to move config into selected profile");
    symlink(&linked_target, &config).expect("failed to link main config to selected profile");
    let source_config = source
        .join("topologies")
        .join(TOPOLOGY)
        .join("agent/config.yml");
    fs::write(&source_config, "setting: updated\nnewTopology: added\n")
        .expect("failed to update topology config");

    let update = run_cli_without_agent_dir(
        &temp,
        &[
            "update",
            "--install-id",
            "config-link-merge",
            "--config-merge",
        ],
    );
    assert_success(&update);
    assert!(
        config.is_symlink(),
        "merge update replaced the main-profile config link"
    );
    assert_eq!(
        fs::read_link(&config).expect("merge update removed the config link"),
        linked_target
    );
    let merged = fs::read_to_string(&linked_target).expect("failed to read merged target config");
    for preserved in [
        "setting: initial",
        "profileOnly: preserve",
        "newTopology: added",
    ] {
        assert!(
            merged.contains(preserved),
            "merge did not preserve or add {preserved:?} at the resolved target:\n{merged}"
        );
    }
}

#[cfg(unix)]
#[test]
fn config_yaml_replace_update_writes_through_main_to_profile_symlink() {
    let temp = TestDir::new();
    let home = temp.0.join("home");
    let omp_home = home.join(".omp");
    let agent_dir = omp_home.join("agent");
    let config = agent_dir.join("config.yml");
    let linked_target = omp_home
        .join("profiles")
        .join(TOPOLOGY)
        .join("agent/config.yml");
    fs::create_dir_all(&agent_dir).expect("failed to create main agent directory");
    fs::create_dir_all(
        linked_target
            .parent()
            .expect("selected profile config has no parent"),
    )
    .expect("failed to create selected profile agent directory");

    let source = local_topology_source(&temp, "setting: initial\n", &[]);
    let installed = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &["--install-id", "config-link-replace"],
    );
    assert_success(&installed);
    let active_bytes = b"# profile config before replacement\nprofileOnly: keep\n";
    fs::write(&config, active_bytes).expect("failed to seed profile config");
    fs::rename(&config, &linked_target).expect("failed to move config into selected profile");
    symlink(&linked_target, &config).expect("failed to link main config to selected profile");
    let replacement = b"setting: replaced\n";
    let source_config = source
        .join("topologies")
        .join(TOPOLOGY)
        .join("agent/config.yml");
    fs::write(&source_config, replacement).expect("failed to update topology config");

    let update = run_cli_without_agent_dir(
        &temp,
        &[
            "update",
            "--install-id",
            "config-link-replace",
            "--config-replace",
        ],
    );
    assert_success(&update);
    assert!(
        config.is_symlink(),
        "replace update replaced the main-profile config link"
    );
    assert_eq!(
        fs::read_link(&config).expect("replace update removed the config link"),
        linked_target
    );
    assert_eq!(
        fs::read(&linked_target).expect("replacement target config disappeared"),
        replacement,
        "replace update did not write topology bytes at the resolved target"
    );
}

#[test]
fn replace_remove_retry_clears_restored_config_bookkeeping() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let config = agent_dir.join("config.yml");
    let prior_config = b"# prior profile config\nprofileOnly: keep\n";
    fs::write(&config, prior_config).expect("failed to seed prior config");
    let managed_bytes = b"topology-managed file\n";
    let source = local_topology_source(
        &temp,
        "topologyOnly: installed\n",
        &[("managed.txt", managed_bytes)],
    );
    let installed = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &["--install-id", "replace-retry", "--config-replace"],
    );
    assert_success(&installed);

    let managed = agent_dir.join("managed.txt");
    let local_edit = b"user-edited managed file\n";
    fs::write(&managed, local_edit).expect("failed to edit managed file");
    let first_remove =
        run_cli_without_agent_dir(&temp, &["remove", "--install-id", "replace-retry"]);
    assert_success(&first_remove);
    let first_report = output_text(&first_remove);
    assert!(
        first_report.contains("partially removed"),
        "first remove did not report its partial state:\n{first_report}"
    );
    assert_eq!(
        fs::read(&config).expect("first remove deleted the restored config"),
        prior_config,
        "first remove did not restore the pre-replacement config"
    );
    assert_eq!(
        fs::read(&managed).expect("first remove deleted the modified managed file"),
        local_edit,
        "first remove overwrote the modified managed file"
    );

    let manifest = manifest_path(&temp, &agent_dir);
    let manifest_json: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).expect("partial manifest is missing"))
            .expect("partial manifest is not valid JSON");
    assert!(
        manifest_json.get("config_replace").is_none(),
        "partial manifest retained replacement metadata after restoring the prior config"
    );

    fs::write(&managed, managed_bytes).expect("failed to resolve managed-file conflict");
    let retry = run_cli_without_agent_dir(&temp, &["remove", "--install-id", "replace-retry"]);
    assert_success(&retry);
    assert!(
        !managed.exists(),
        "retry did not remove the now-unchanged managed file"
    );
    assert!(
        !manifest.exists(),
        "retry left the ownership manifest after all conflicts were resolved"
    );
    assert_eq!(
        fs::read(&config).expect("retry removed the restored config"),
        prior_config,
        "retry changed the already-restored prior config"
    );
}
#[cfg(unix)]
#[test]
fn install_alias_requires_profile_and_rejects_agent_dir_without_mutation() {
    let temp = TestDir::new();
    let bin_dir = temp.0.join("validation-bin");
    fs::create_dir_all(&bin_dir).expect("failed to create fake PATH");
    let omp_marker = temp.0.join("omp-invoked");
    let fake_omp = bin_dir.join("omp");
    fs::write(&fake_omp, "#!/bin/sh\n: > \"$OMP_TEST_MARKER\"\nexit 0\n")
        .expect("failed to write fake omp");
    fs::set_permissions(&fake_omp, fs::Permissions::from_mode(0o755))
        .expect("failed to make fake omp executable");
    let source = local_topology_source(&temp, "fixture: true\n", &[("payload.txt", b"payload")]);
    let source = source.to_str().expect("source path is not UTF-8");
    let target = temp.0.join("target");
    let target_text = target.to_str().expect("target path is not UTF-8");

    for (label, extra_args) in [
        (
            "profile with agent-dir",
            vec![
                "--profile",
                "demo",
                "--alias",
                "omp-demo",
                "--agent-dir",
                target_text,
            ],
        ),
        (
            "missing profile",
            vec!["--alias", "omp-demo", "--agent-dir", target_text],
        ),
    ] {
        let mut args = vec!["install", "-t", TOPOLOGY, "--source", source];
        args.extend(extra_args);
        let output = cli_command(&temp, Path::new(env!("CARGO_MANIFEST_DIR")))
            .args(&args)
            .env("PATH", &bin_dir)
            .env("OMP_TEST_MARKER", &omp_marker)
            .output()
            .expect("failed to execute invalid install");
        assert_eq!(
            output.status.code(),
            Some(2),
            "{label} unexpectedly proceeded: {output:?}"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("--profile"),
            "{label} did not report its profile requirement/conflict:\n{stderr}"
        );
        if label == "profile with agent-dir" {
            assert!(
                stderr.contains("--agent-dir"),
                "profile/agent-dir conflict was not identified:\n{stderr}"
            );
        }
        assert!(!omp_marker.exists(), "{label} invoked OMP");
        assert!(!target.exists(), "{label} created the target");
        assert!(
            !temp.state_home().exists(),
            "{label} created installer state"
        );
    }
}

#[cfg(unix)]
#[test]
fn installer_install_id_may_match_path_executable_without_shell_alias() {
    let temp = TestDir::new();
    let path_dir = create_path_dir(&temp);
    add_path_executable(&path_dir, "teddy");
    let profile = "path-install-id";
    let agent_dir = temp
        .0
        .join("home/.omp/profiles")
        .join(profile)
        .join("agent");
    let source = local_topology_source(&temp, "fixture: true\n", &[("payload.txt", b"payload")]);
    let source = source.to_str().expect("source path is not UTF-8");
    let omp_log = temp.0.join("omp-invocations.log");
    let alias_marker = temp.0.join("omp-alias-created");
    let fake_omp = path_dir.join("omp");
    fs::write(
        &fake_omp,
        "#!/bin/sh\n\
         printf '%s\\n' \"$*\" >> \"$OMP_TEST_OMP_LOG\"\n\
         if [ \"$#\" -eq 4 ] && [ \"$1\" = \"--profile\" ] && [ \"$2\" = \"$OMP_TEST_PROFILE\" ] && [ \"$3\" = \"config\" ] && [ \"$4\" = \"path\" ]; then\n\
         \tprintf '%s\\n' \"$OMP_TEST_PROFILE_AGENT_DIR\"\n\
         \texit 0\n\
         fi\n\
         exit 64\n",
    )
    .expect("failed to write fake omp");
    fs::set_permissions(&fake_omp, fs::Permissions::from_mode(0o755))
        .expect("failed to make fake omp executable");
    let output = cli_command(&temp, Path::new(env!("CARGO_MANIFEST_DIR")))
        .args([
            "install",
            "-t",
            TOPOLOGY,
            "--source",
            source,
            "--profile",
            profile,
            "--install-id",
            "teddy",
        ])
        .env("PATH", &path_dir)
        .env("OMP_TEST_PROFILE", profile)
        .env("OMP_TEST_PROFILE_AGENT_DIR", &agent_dir)
        .env("OMP_TEST_OMP_LOG", &omp_log)
        .env("OMP_TEST_ALIAS_MARKER", &alias_marker)
        .output()
        .expect("failed to execute install with PATH-matching install ID");
    assert_success(&output);
    assert_eq!(
        fs::read(agent_dir.join("payload.txt")).expect("install omitted the payload"),
        b"payload"
    );
    let registry: serde_json::Value = serde_json::from_slice(
        &fs::read(temp.state_home().join("omp-topology/registry.json"))
            .expect("install omitted registry"),
    )
    .expect("registry is not valid JSON");
    assert!(
        registry["aliases"]["teddy"].is_object(),
        "install ID matching the PATH executable was not registered"
    );
    assert_eq!(
        fs::read_to_string(&omp_log).expect("OMP config path resolution was not recorded"),
        format!("--profile {profile} config path\n"),
        "install ID incorrectly invoked OMP shell-alias creation"
    );
    assert!(
        !alias_marker.exists(),
        "install ID matching a PATH executable created a shell alias"
    );
}

#[cfg(unix)]
#[test]
fn lifecycle_commands_reject_old_alias_flag_without_state_mutation() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("agent");
    let source = local_topology_source(&temp, "fixture: true\n", &[("payload.txt", b"payload")]);
    let installed = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &["--install-id", "legacy-flag-check"],
    );
    assert_success(&installed);
    let registry_path = temp.state_home().join("omp-topology/registry.json");
    let registry_before = fs::read(&registry_path).expect("registry is missing");
    let state_before = file_snapshot(&temp.state_home());
    let target_before = file_snapshot(&agent_dir);

    for args in [
        vec!["update", "--alias", "legacy-flag-check"],
        vec!["remove", "--alias", "legacy-flag-check"],
        vec!["status", "--alias", "legacy-flag-check"],
    ] {
        let output = run_cli_without_agent_dir(&temp, &args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "legacy lifecycle option unexpectedly succeeded for {args:?}: {output:?}"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("--alias"),
            "clap error did not identify the removed lifecycle option for {args:?}:\n{stderr}"
        );
        assert_eq!(
            fs::read(&registry_path).expect("registry disappeared"),
            registry_before,
            "{args:?} changed the registry"
        );
        assert_eq!(
            file_snapshot(&temp.state_home()),
            state_before,
            "{args:?} changed installer state"
        );
        assert_eq!(
            file_snapshot(&agent_dir),
            target_before,
            "{args:?} changed the install target"
        );
    }
}

#[cfg(unix)]
#[test]
fn shell_alias_dry_run_never_invokes_omp_alias_creation_or_mutates_state() {
    let temp = TestDir::new();
    let path_dir = create_path_dir(&temp);
    let profile = "dry-run-alias";
    let agent_dir = temp
        .0
        .join("home/.omp/profiles")
        .join(profile)
        .join("agent");
    let source = local_topology_source(&temp, "fixture: true\n", &[("payload.txt", b"payload")]);

    let output = run_install_from_source_with_path(
        &temp,
        &agent_dir,
        &source,
        "omp-dry-run",
        true,
        &path_dir,
    );
    assert_success(&output);
    assert!(
        !temp.0.join("omp-alias-created").exists(),
        "dry-run created the OMP shell alias"
    );
    let invocation_log = temp.0.join("omp-invocations.log");
    assert!(
        !invocation_log.exists()
            || fs::read_to_string(&invocation_log)
                .expect("failed to read OMP invocation log")
                .lines()
                .all(|line| line.ends_with("config path")),
        "dry-run invoked OMP shell-alias creation"
    );
    assert!(!agent_dir.exists(), "dry-run created the profile target");
    assert!(
        !temp.state_home().exists(),
        "dry-run created installer state"
    );
}

#[cfg(unix)]
#[test]
fn failed_omp_alias_creation_reports_partial_install_without_corrupting_json() {
    let temp = TestDir::new();
    let profile = "partial-alias";
    let shell_alias = "omp-recovery";
    let install_id = "retained-install";
    let agent_dir = temp
        .0
        .join("home/.omp/profiles")
        .join(profile)
        .join("agent");
    let source = local_named_topology_source(
        &temp,
        "source",
        "alpha",
        "fixture: true\n",
        &[("payload.txt", b"committed payload")],
    );
    let source = source.to_str().expect("source path is not UTF-8");
    let bin_dir = temp.0.join("failure-omp-bin");
    fs::create_dir_all(&bin_dir).expect("failed to create fake PATH");
    let invocation_log = temp.0.join("omp-invocations.log");
    let fake_omp = bin_dir.join("omp");
    fs::write(
        &fake_omp,
        "#!/bin/sh\n\
         printf '%s\\n' \"$*\" >> \"$OMP_TEST_OMP_LOG\"\n\
         if [ \"$#\" -eq 4 ] && [ \"$1\" = \"--profile\" ] && [ \"$2\" = \"$OMP_TEST_PROFILE\" ] && [ \"$3\" = \"config\" ] && [ \"$4\" = \"path\" ]; then\n\
         \tprintf '%s\\n' \"$OMP_TEST_PROFILE_AGENT_DIR\"\n\
         \texit 0\n\
         fi\n\
         if [ \"$#\" -eq 4 ] && [ \"$1\" = \"--profile\" ] && [ \"$2\" = \"$OMP_TEST_PROFILE\" ] && [ \"$3\" = \"--alias\" ] && [ \"$4\" = \"$OMP_TEST_ALIAS\" ]; then\n\
         \tprintf 'FAKE_OMP_STDOUT_MUST_NOT_LEAK\\n'\n\
         \tprintf 'FAKE_OMP_STDERR_MUST_NOT_LEAK\\n' >&2\n\
         \texit 23\n\
         fi\n\
         exit 64\n",
    )
    .expect("failed to write fake omp");
    fs::set_permissions(&fake_omp, fs::Permissions::from_mode(0o755))
        .expect("failed to make fake omp executable");

    let output = cli_command(&temp, Path::new(env!("CARGO_MANIFEST_DIR")))
        .args([
            "install",
            "-t",
            "alpha",
            "--source",
            source,
            "--profile",
            profile,
            "--install-id",
            install_id,
            "--alias",
            shell_alias,
            "--json",
        ])
        .env("PATH", &bin_dir)
        .env("OMP_TEST_PROFILE", profile)
        .env("OMP_TEST_ALIAS", shell_alias)
        .env("OMP_TEST_PROFILE_AGENT_DIR", &agent_dir)
        .env("OMP_TEST_OMP_LOG", &invocation_log)
        .output()
        .expect("failed to execute installer with failing OMP alias");
    assert!(
        !output.status.success(),
        "install reported success despite OMP alias failure: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.is_empty(),
        "failed JSON install wrote non-JSON or partial JSON to stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("FAKE_OMP_STDOUT_MUST_NOT_LEAK")
            && !stderr.contains("FAKE_OMP_STDOUT_MUST_NOT_LEAK")
            && !stderr.contains("FAKE_OMP_STDERR_MUST_NOT_LEAK"),
        "captured OMP output escaped into installer output:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("topology install is complete")
            && stderr.contains("shell alias could not be created"),
        "failure omitted explicit partial-completion diagnostics:\n{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "recover by running 'omp --profile {profile} --alias {shell_alias}' manually"
        )),
        "failure omitted the exact manual recovery command:\n{stderr}"
    );
    assert!(
        fs::read(agent_dir.join("payload.txt"))
            .is_ok_and(|payload| payload == b"committed payload"),
        "failed shell-alias creation rolled back the committed topology payload"
    );
    let registry: serde_json::Value = serde_json::from_slice(
        &fs::read(temp.state_home().join("omp-topology/registry.json"))
            .expect("committed install registry is missing"),
    )
    .expect("committed registry is not valid JSON");
    assert!(
        registry["aliases"][install_id].is_object(),
        "failed shell-alias creation removed the committed install registry entry"
    );
    assert_eq!(
        fs::read_to_string(&invocation_log).expect("fake OMP invocation log is missing"),
        format!("--profile {profile} config path\n--profile {profile} --alias {shell_alias}\n"),
        "OMP alias creation did not follow profile resolution after install commit"
    );
}

#[test]
fn public_install_status_and_profiles_json_use_install_id_terminology() {
    let temp = TestDir::new();
    let agent_dir = temp.0.join("custom-agent");
    let source = local_topology_source(&temp, "fixture: true\n", &[("payload.txt", b"payload")]);

    let installed = install_from_source(
        &temp,
        &agent_dir,
        &source,
        &["--install-id", "json-install-id", "--json"],
    );
    assert_success(&installed);
    let install_json: serde_json::Value =
        serde_json::from_slice(&installed.stdout).expect("install JSON is invalid");
    assert_eq!(install_json["command"], "install");
    assert!(
        install_json["lines"]
            .as_array()
            .is_some_and(|lines| lines.iter().any(|line| {
                line.as_str()
                    .is_some_and(|line| line.contains("install ID 'json-install-id'"))
            })),
        "install JSON did not report the public install ID: {install_json}"
    );
    assert!(
        install_json.get("alias").is_none(),
        "install JSON exposed the legacy registry alias property: {install_json}"
    );

    let status = run_cli_without_agent_dir(
        &temp,
        &["status", "--install-id", "json-install-id", "--json"],
    );
    assert_success(&status);
    let status_json: serde_json::Value =
        serde_json::from_slice(&status.stdout).expect("status JSON is invalid");
    assert_eq!(status_json["install_id"], "json-install-id");
    assert!(
        status_json.get("alias").is_none(),
        "status JSON exposed the legacy registry alias property: {status_json}"
    );

    let profiles = run_cli_without_agent_dir(&temp, &["profiles", "list", "--json"]);
    assert_success(&profiles);
    let profiles_json: serde_json::Value =
        serde_json::from_slice(&profiles.stdout).expect("profiles JSON is invalid");
    let managed = profiles_json["profiles"]
        .as_array()
        .expect("profiles JSON omitted its profiles array")
        .iter()
        .flat_map(|profile| profile["managed"].as_array().into_iter().flatten())
        .collect::<Vec<_>>();
    assert!(
        managed
            .iter()
            .any(|install| install["install_id"] == "json-install-id"),
        "profiles JSON omitted the install ID: {profiles_json}"
    );
    assert!(
        managed.iter().all(|install| install.get("alias").is_none()),
        "profiles JSON exposed the legacy registry alias property: {profiles_json}"
    );
}
