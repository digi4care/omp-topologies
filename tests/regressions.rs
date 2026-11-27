use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

const TOPOLOGY: &str = "regression-topology";

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before UNIX epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "omp-topology-regressions-{}-{unique}",
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

fn command(temp: &TestDir) -> Command {
    let home = temp.0.join("home");
    let xdg_config = temp.0.join("xdg-config");
    fs::create_dir_all(&home).expect("failed to create isolated HOME");
    fs::create_dir_all(&xdg_config).expect("failed to create isolated XDG_CONFIG_HOME");
    let mut command = Command::new(env!("CARGO_BIN_EXE_omp-profile"));
    command
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", xdg_config)
        .env("XDG_STATE_HOME", temp.state_home());
    command
}

fn run(temp: &TestDir, args: &[&str]) -> Output {
    command(temp)
        .args(args)
        .output()
        .expect("failed to execute omp-profile")
}

fn run_for_agent(temp: &TestDir, agent_dir: &Path, args: &[&str]) -> Output {
    command(temp)
        .args(args)
        .arg("--agent-dir")
        .arg(agent_dir)
        .output()
        .expect("failed to execute omp-profile")
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

fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn json_document(output: &Output) -> serde_json::Value {
    assert_success(output);
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|err| panic!("stdout was not exactly one JSON document: {err}"))
}

fn listed_topology_names(temp: &TestDir, source_root: &Path) -> Vec<String> {
    let source = source_root.to_string_lossy().into_owned();
    let report = json_document(&run(temp, &["list", "--source", &source, "--json"]));
    report["topologies"]
        .as_array()
        .expect("list JSON has no topologies array")
        .iter()
        .map(|topology| {
            topology["name"]
                .as_str()
                .expect("listed topology has no string name")
                .to_string()
        })
        .collect()
}

fn write_topology(
    source_root: &Path,
    name: &str,
    config: &str,
    files: &[(&str, &[u8])],
) -> PathBuf {
    let topology_dir = source_root.join("topologies").join(name);
    let agent_root = topology_dir.join("agent");
    fs::create_dir_all(&agent_root).expect("failed to create fixture agent overlay");
    fs::write(
        topology_dir.join("topology.yml"),
        format!(
            "name: {name}\nversion: 1.0.0\ndescription: regression fixture\ninstallable: true\n"
        ),
    )
    .expect("failed to write fixture metadata");
    fs::write(agent_root.join("config.yml"), config).expect("failed to write fixture config");
    for (relative, contents) in files {
        let path = agent_root.join(relative);
        fs::create_dir_all(path.parent().expect("fixture file has no parent"))
            .expect("failed to create fixture file parent");
        fs::write(path, contents).expect("failed to write fixture overlay file");
    }
    topology_dir
}

fn install_source(
    temp: &TestDir,
    agent_dir: &Path,
    source_root: &Path,
    name: &str,
    install_id: Option<&str>,
    json: bool,
) -> Output {
    let source = source_root.to_string_lossy().into_owned();
    let mut args = vec![
        "install".to_string(),
        "--topology".to_string(),
        name.to_string(),
        "--source".to_string(),
        source,
    ];
    if let Some(install_id) = install_id {
        args.extend(["--install-id".to_string(), install_id.to_string()]);
    }
    if json {
        args.push("--json".to_string());
    }
    args.push("--agent-dir".to_string());
    args.push(agent_dir.to_string_lossy().into_owned());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run(temp, &args)
}

fn files_under(root: &Path) -> Vec<PathBuf> {
    fn visit(directory: &Path, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(directory).expect("failed to read test directory") {
            let entry = entry.expect("failed to read test directory entry");
            let kind = entry.file_type().expect("failed to inspect test entry");
            if kind.is_dir() {
                visit(&entry.path(), files);
            } else if kind.is_file() {
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

fn manifest_path(temp: &TestDir) -> PathBuf {
    files_under(&temp.state_home().join("omp-topology"))
        .into_iter()
        .find(|path| path.file_name().is_some_and(|name| name == "manifest.json"))
        .expect("ownership manifest is missing")
}
fn manifest_exists(temp: &TestDir) -> bool {
    files_under(&temp.state_home().join("omp-topology"))
        .iter()
        .any(|path| path.file_name().is_some_and(|name| name == "manifest.json"))
}
fn file_snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    files_under(root)
        .into_iter()
        .map(|path| {
            let contents = fs::read(&path)
                .unwrap_or_else(|err| panic!("failed to snapshot {}: {err}", path.display()));
            (path, contents)
        })
        .collect()
}

fn is_snapshot_timestamp(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 26
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 | 13 | 16 | 19 => *byte == b'-',
            10 => *byte == b'T',
            _ => byte.is_ascii_digit(),
        })
}
fn snapshot_transaction_dirs(state_root: &Path) -> BTreeSet<PathBuf> {
    files_under(state_root)
        .into_iter()
        .filter_map(|file| {
            file.ancestors()
                .find(|ancestor| {
                    ancestor.file_name().is_some_and(|name| {
                        let name = name.to_string_lossy();
                        name.strip_prefix("install-")
                            .or_else(|| name.strip_prefix("remove-"))
                            .is_some_and(is_snapshot_timestamp)
                    })
                })
                .map(Path::to_path_buf)
        })
        .collect()
}

fn run_cycles(
    temp: &TestDir,
    source_root: &Path,
    agent_dir: &Path,
    install_id: &str,
    cycles: usize,
) {
    for _ in 0..cycles {
        let installed = install_source(
            temp,
            agent_dir,
            source_root,
            TOPOLOGY,
            Some(install_id),
            false,
        );
        assert_success(&installed);
        let removed = run_for_agent(temp, agent_dir, &["remove", "--topology", TOPOLOGY]);
        assert_success(&removed);
    }
}

#[test]
fn remove_preserves_user_comment_in_installer_created_config() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("agent");
    write_topology(&source_root, TOPOLOGY, "task:\n  eager: true\n", &[]);
    let config = agent_dir.join("config.yml");
    assert!(
        !config.exists(),
        "test config unexpectedly exists before install"
    );

    assert_success(&install_source(
        &temp,
        &agent_dir,
        &source_root,
        TOPOLOGY,
        Some("comment"),
        false,
    ));
    let mut contents = fs::read_to_string(&config).expect("installed config is missing");
    contents.push_str("# user-owned comment\n");
    fs::write(&config, contents).expect("failed to add user-owned config comment");

    let removed = run_for_agent(&temp, &agent_dir, &["remove", "--topology", TOPOLOGY]);
    assert_success(&removed);
    assert!(
        config.is_file(),
        "remove deleted the config containing a user-owned comment"
    );
    let after = fs::read_to_string(&config).expect("preserved config is unreadable");
    assert!(
        after.contains("# user-owned comment"),
        "remove discarded the user-owned config comment:\n{after}"
    );
    assert!(
        !after.contains("task:") && !after.contains("eager:"),
        "remove left managed topology keys in config:\n{after}"
    );
}

#[test]
fn remove_preserves_user_empty_mapping_in_installer_created_config() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("agent");
    write_topology(&source_root, TOPOLOGY, "task:\n  eager: true\n", &[]);
    let config = agent_dir.join("config.yml");
    assert!(
        !config.exists(),
        "test config unexpectedly exists before install"
    );

    assert_success(&install_source(
        &temp,
        &agent_dir,
        &source_root,
        TOPOLOGY,
        Some("empty-mapping"),
        false,
    ));
    let mut contents = fs::read_to_string(&config).expect("installed config is missing");
    contents.push_str("local: {}\n");
    fs::write(&config, contents).expect("failed to add user-owned empty mapping");

    let removed = run_for_agent(&temp, &agent_dir, &["remove", "--topology", TOPOLOGY]);
    assert_success(&removed);
    assert!(
        config.is_file(),
        "remove deleted the config containing a user-owned empty mapping"
    );
    let after = fs::read_to_string(&config).expect("preserved config is unreadable");
    assert!(
        after.contains("local: {}"),
        "remove discarded the user-owned empty mapping:\n{after}"
    );
    assert!(
        !after.contains("task:") && !after.contains("eager:"),
        "remove left managed topology keys in config:\n{after}"
    );
}

#[test]
fn updates_and_reinstalls_preserve_user_edits_and_deletions() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("agent");
    let topology_dir = write_topology(
        &source_root,
        TOPOLOGY,
        "task:\n  eager: true\n  maxConcurrency: 4\n",
        &[
            ("RULES.md", b"rules v1\n"),
            ("agents/guide.md", b"guide v1\n"),
            ("agents/second.md", b"second v1\n"),
        ],
    );
    assert_success(&install_source(
        &temp,
        &agent_dir,
        &source_root,
        TOPOLOGY,
        Some("preserve"),
        false,
    ));

    let guide = agent_dir.join("agents/guide.md");
    let rules = agent_dir.join("RULES.md");
    let second = agent_dir.join("agents/second.md");
    let user_guide = b"user-edited guide\n";
    fs::write(&guide, user_guide).expect("failed to edit installed guide");
    fs::remove_file(&rules).expect("failed to delete installed rules");
    let config = agent_dir.join("config.yml");
    let edited_config = fs::read_to_string(&config)
        .expect("installed config is missing")
        .replace("maxConcurrency: 4", "maxConcurrency: 99");
    fs::write(&config, edited_config).expect("failed to edit installed config leaf");

    fs::write(
        topology_dir.join("agent/config.yml"),
        "task:\n  eager: false\n  maxConcurrency: 8\n",
    )
    .expect("failed to update fixture config");
    fs::write(topology_dir.join("agent/RULES.md"), b"rules v2\n")
        .expect("failed to update fixture rules");
    fs::write(topology_dir.join("agent/agents/guide.md"), b"guide v2\n")
        .expect("failed to update fixture guide");
    let second_v2 = b"second v2\n";
    fs::write(topology_dir.join("agent/agents/second.md"), second_v2)
        .expect("failed to update fixture second file");

    let update = run(&temp, &["update", "--install-id", "preserve", "--json"]);
    assert_eq!(json_document(&update)["command"], "update");
    assert_eq!(fs::read(&guide).unwrap().as_slice(), user_guide);
    assert!(
        !rules.exists(),
        "update recreated a previously deleted owned file"
    );
    assert_eq!(fs::read(&second).unwrap().as_slice(), second_v2);
    let config_after_update = fs::read_to_string(&config).expect("update removed config");
    assert!(
        config_after_update.contains("maxConcurrency: 99"),
        "update overwrote a user-edited owned config leaf:\n{config_after_update}"
    );

    let user_second = b"user-edited second file\n";
    fs::write(&second, user_second).expect("failed to edit second owned file");
    fs::remove_file(&guide).expect("failed to delete installed guide");
    fs::write(
        topology_dir.join("agent/config.yml"),
        "task:\n  eager: true\n  maxConcurrency: 12\n",
    )
    .expect("failed to update fixture config for reinstall");
    fs::write(topology_dir.join("agent/agents/guide.md"), b"guide v3\n")
        .expect("failed to update fixture guide for reinstall");
    fs::write(topology_dir.join("agent/agents/second.md"), b"second v3\n")
        .expect("failed to update fixture second file for reinstall");

    let reinstall = install_source(
        &temp,
        &agent_dir,
        &source_root,
        TOPOLOGY,
        Some("preserve"),
        false,
    );
    assert_success(&reinstall);
    assert_eq!(fs::read(&second).unwrap().as_slice(), user_second);
    assert!(
        !guide.exists(),
        "reinstall recreated a previously deleted owned file"
    );
    let config_after_reinstall = fs::read_to_string(&config).expect("reinstall removed config");
    assert!(
        config_after_reinstall.contains("maxConcurrency: 99"),
        "reinstall overwrote a user-edited owned config leaf:\n{config_after_reinstall}"
    );
}

#[cfg(unix)]
#[test]
fn remove_rolls_back_deleted_files_when_config_rewrite_fails() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("agent");
    write_topology(
        &source_root,
        TOPOLOGY,
        "task:\n  eager: true\n",
        &[("nested/RULES.md", b"managed rules\n")],
    );
    fs::create_dir_all(&agent_dir).expect("failed to create target agent directory");
    let original_config = format!("task:\n  eager: {}\n", "x".repeat(4096));
    fs::write(agent_dir.join("config.yml"), &original_config)
        .expect("failed to seed large prior config value");
    let installed = install_source(
        &temp,
        &agent_dir,
        &source_root,
        TOPOLOGY,
        Some("rollback"),
        false,
    );
    assert_success(&installed);

    let rules = agent_dir.join("nested/RULES.md");
    let rules_before = fs::read(&rules).expect("installed rules are missing");
    let config = agent_dir.join("config.yml");
    let config_before = fs::read(&config).expect("installed config is missing");
    let manifest = manifest_path(&temp);
    let manifest_before = fs::read(&manifest).expect("failed to read ownership manifest");

    let wrapper = temp.0.join("remove-with-file-limit");
    fs::write(
        &wrapper,
        b"#!/bin/sh\ntrap '' XFSZ\nulimit -f 2 || exit 91\nexec \"$OMP_TOPOLOGY_BINARY\" \"$@\"\n",
    )
    .expect("failed to write file-limited command wrapper");
    let home = temp.0.join("home");
    let xdg_config = temp.0.join("xdg-config");
    fs::create_dir_all(&home).expect("failed to create isolated HOME");
    fs::create_dir_all(&xdg_config).expect("failed to create isolated XDG_CONFIG_HOME");
    // Reading the just-written wrapper through the shell avoids ETXTBSY
    // from directly executing it on Docker overlayfs.
    let removed = Command::new("/bin/sh")
        .arg(&wrapper)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", xdg_config)
        .env("XDG_STATE_HOME", temp.state_home())
        .env("OMP_TOPOLOGY_BINARY", env!("CARGO_BIN_EXE_omp-profile"))
        .args(["remove", "--topology", TOPOLOGY, "--agent-dir"])
        .arg(&agent_dir)
        .output()
        .expect("failed to execute file-limited remove");

    assert!(
        !removed.status.success(),
        "remove unexpectedly succeeded despite the deterministic config-write limit"
    );
    let report = output_text(&removed);
    assert!(
        report.contains("remove failed and changes were rolled back"),
        "remove did not report a completed rollback:\n{report}"
    );
    assert_eq!(
        fs::read(&rules).expect("rollback did not restore deleted rules"),
        rules_before,
        "rollback did not preserve the deleted managed file"
    );
    assert_eq!(
        fs::read(&config).expect("rollback lost the pre-remove config"),
        config_before,
        "rollback changed config after the rewrite failed"
    );
    assert_eq!(
        fs::read(&manifest).expect("rollback lost ownership state"),
        manifest_before,
        "rollback changed ownership state"
    );
}

#[test]
fn local_config_only_drift_is_visible_and_updatable() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("agent");
    let topology_dir = write_topology(
        &source_root,
        TOPOLOGY,
        "task:\n  eager: false\n  maxConcurrency: 4\n",
        &[("RULES.md", b"unchanged overlay file\n")],
    );
    let installed = install_source(
        &temp,
        &agent_dir,
        &source_root,
        TOPOLOGY,
        Some("digest"),
        true,
    );
    assert_eq!(json_document(&installed)["command"], "install");
    let before = json_document(&run(&temp, &["status", "--install-id", "digest", "--json"]));
    assert_eq!(before["freshness"]["status"], "up-to-date");

    fs::write(
        topology_dir.join("agent/config.yml"),
        "task:\n  eager: true\n  maxConcurrency: 4\n",
    )
    .expect("failed to make config-only source change");
    let after = json_document(&run(&temp, &["status", "--install-id", "digest", "--json"]));
    assert_eq!(
        after["freshness"]["status"], "update-available",
        "a source config-only change was not reported as drift: {after}"
    );

    let updated = json_document(&run(&temp, &["update", "--install-id", "digest", "--json"]));
    assert_eq!(updated["command"], "update");
    let current = json_document(&run(&temp, &["status", "--install-id", "digest", "--json"]));
    assert_eq!(current["freshness"]["status"], "up-to-date");
    assert!(
        fs::read_to_string(agent_dir.join("config.yml"))
            .expect("updated config is missing")
            .contains("eager: true"),
        "update did not apply the changed source config"
    );
}

#[test]
fn update_source_switches_local_source_and_failed_override_keeps_previous_source() {
    let temp = TestDir::new();
    let source_a = temp.0.join("source-a");
    let source_b = temp.0.join("source-b");
    let missing_topology_source = temp.0.join("source-without-topology");
    let agent_dir = temp.0.join("agent");
    write_topology(
        &source_a,
        TOPOLOGY,
        "task:\n  eager: true\n",
        &[("RULES.md", b"source A\n")],
    );
    let topology_b = write_topology(
        &source_b,
        TOPOLOGY,
        "task:\n  eager: true\n",
        &[("RULES.md", b"source B\n")],
    );
    write_topology(
        &missing_topology_source,
        "different-topology",
        "task:\n  eager: true\n",
        &[("RULES.md", b"unselected topology\n")],
    );
    assert_success(&install_source(
        &temp,
        &agent_dir,
        &source_a,
        TOPOLOGY,
        Some("source-switch"),
        false,
    ));

    let source_b_arg = source_b.to_string_lossy().into_owned();
    let switched = run(
        &temp,
        &[
            "update",
            "--install-id",
            "source-switch",
            "--source",
            &source_b_arg,
            "--json",
        ],
    );
    assert_success(&switched);
    assert_eq!(
        fs::read(agent_dir.join("RULES.md")).expect("updated source file is missing"),
        b"source B\n",
        "update --source did not apply the newly selected local source"
    );

    let registry_path = temp.state_home().join("omp-topology/registry.json");
    let canonical_source_b = fs::canonicalize(&source_b)
        .expect("failed to canonicalize selected source")
        .to_string_lossy()
        .into_owned();
    let recorded_source_path = || {
        let registry: serde_json::Value = serde_json::from_slice(
            &fs::read(&registry_path).expect("failed to read install registry"),
        )
        .expect("install registry was not valid JSON");
        registry["aliases"]["source-switch"]["source"]["path"]
            .as_str()
            .expect("registry omitted the local source path")
            .to_string()
    };
    assert_eq!(
        recorded_source_path(),
        canonical_source_b,
        "successful source override was not recorded"
    );

    let missing_topology_arg = missing_topology_source.to_string_lossy().into_owned();
    let failed_override = run(
        &temp,
        &[
            "update",
            "--install-id",
            "source-switch",
            "--source",
            &missing_topology_arg,
            "--json",
        ],
    );
    assert!(
        !failed_override.status.success(),
        "update unexpectedly accepted a source without the installed topology"
    );
    assert_eq!(
        fs::read(agent_dir.join("RULES.md")).expect("failed override removed managed file"),
        b"source B\n",
        "failed source override mutated the installed files"
    );
    assert_eq!(
        recorded_source_path(),
        canonical_source_b,
        "failed source override replaced the previously recorded source"
    );

    fs::write(
        topology_b.join("agent/RULES.md"),
        b"source B after failure\n",
    )
    .expect("failed to change the previously selected source");
    let update_saved_source = run(
        &temp,
        &["update", "--install-id", "source-switch", "--json"],
    );
    assert_success(&update_saved_source);
    assert_eq!(
        fs::read(agent_dir.join("RULES.md")).expect("saved-source update omitted managed file"),
        b"source B after failure\n",
        "update without --source did not use the source retained after the failed override"
    );
    assert_eq!(
        recorded_source_path(),
        canonical_source_b,
        "update without --source changed the recorded source"
    );
}

#[cfg(unix)]
fn fake_github_git(temp: &TestDir, github_url: &str, commit: &str) -> std::ffi::OsString {
    let fake_bin = temp.0.join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("failed to create fake git directory");
    let fake_git = fake_bin.join("git");
    fs::write(
        &fake_git,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = ls-remote ] && [ \"$2\" = --symref ] && \
             [ \"$3\" = {github_url} ] && [ \"$4\" = HEAD ]; then\n\
               printf 'ref: refs/heads/main\\tHEAD\\n%s\\tHEAD\\n' '{commit}'\n\
               exit 0\n\
             fi\n\
             if [ \"$1\" = ls-remote ] && [ \"$2\" = {github_url} ]; then\n\
               printf '{commit}\\trefs/heads/%s\\n' \"$3\"\n\
               exit 0\n\
             fi\n\
             if [ \"$1\" = init ] && [ \"$2\" = --quiet ]; then\n\
               mkdir -p \"$3\"\n\
               exit $?\n\
             fi\n\
             if [ \"$1\" = -C ]; then\n\
               checkout=\"$2\"\n\
               shift 2\n\
               case \"$1\" in\n\
                 remote) [ \"$2\" = add ] && [ \"$3\" = origin ] && \
                         [ \"$4\" = {github_url} ] ;;\n\
                 fetch) cp -R \"$OMP_FAKE_GIT_TREE\"/. \"$checkout\"/ ;;\n\
                 checkout) exit 0 ;;\n\
                 rev-parse) [ \"$2\" = HEAD ] && printf '{commit}\\n' ;;\n\
                 *) exit 42 ;;\n\
               esac\n\
               exit $?\n\
             fi\n\
             exit 42\n"
        ),
    )
    .expect("failed to write fake git executable");
    let mut permissions = fs::metadata(&fake_git)
        .expect("failed to inspect fake git executable")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_git, permissions).expect("failed to make fake git executable");
    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![fake_bin];
    paths.extend(std::env::split_paths(&inherited_path));
    std::env::join_paths(paths).expect("failed to add fake git to PATH")
}

#[cfg(unix)]
#[test]
fn update_clears_github_ref_when_switching_to_local_source() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source-tree");
    let local_topology = write_topology(
        &source_root,
        TOPOLOGY,
        "task:\n  eager: true\n",
        &[("RULES.md", b"initial source\n")],
    );
    let agent_dir = temp.0.join("agent");
    let github_url = "https://github.com/owner/repo.git";
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let fake_path = fake_github_git(&temp, github_url, commit);
    let installed = command(&temp)
        .env("PATH", &fake_path)
        .env("OMP_FAKE_GIT_TREE", &source_root)
        .args([
            "install",
            "--topology",
            TOPOLOGY,
            "--source",
            "https://github.com/owner/repo",
            "--ref",
            "release-1",
            "--install-id",
            "source-switch",
            "--agent-dir",
        ])
        .arg(&agent_dir)
        .arg("--json")
        .output()
        .expect("failed to execute GitHub install with fake git");
    assert_success(&installed);

    let registry_path = temp.state_home().join("omp-topology/registry.json");
    let read_entry = || {
        let registry: serde_json::Value = serde_json::from_slice(
            &fs::read(&registry_path).expect("failed to read install registry"),
        )
        .expect("install registry was not valid JSON");
        registry["aliases"]["source-switch"].clone()
    };
    let initial_entry = read_entry();
    assert_eq!(
        initial_entry["source"],
        serde_json::json!({
            "kind": "github",
            "owner": "owner",
            "repo": "repo"
        }),
        "test setup did not install the expected GitHub source"
    );
    assert_eq!(
        initial_entry["ref"], "release-1",
        "test setup did not save the GitHub ref"
    );

    let local_source = source_root.to_string_lossy().into_owned();
    let switched = command(&temp)
        .env("PATH", &fake_path)
        .env("OMP_FAKE_GIT_TREE", &source_root)
        .args(["update", "--install-id", "source-switch", "--source"])
        .arg(&local_source)
        .arg("--json")
        .output()
        .expect("failed to execute update with local source override");
    assert_success(&switched);

    let canonical_local_source = fs::canonicalize(&source_root)
        .expect("failed to canonicalize local source")
        .to_string_lossy()
        .into_owned();
    let switched_entry = read_entry();
    assert_eq!(
        switched_entry["source"]["kind"], "local",
        "registry did not record the local source kind after switching"
    );
    assert_eq!(
        switched_entry["source"]["path"], canonical_local_source,
        "registry did not record the canonical local source path"
    );
    assert!(
        switched_entry["ref"].is_null(),
        "registry retained the old GitHub ref after switching to a local source"
    );

    fs::write(
        local_topology.join("agent/RULES.md"),
        b"updated local source\n",
    )
    .expect("failed to change the local source after switching");
    let updated_local = command(&temp)
        .env("PATH", &fake_path)
        .env("OMP_FAKE_GIT_TREE", &source_root)
        .args(["update", "--install-id", "source-switch", "--json"])
        .output()
        .expect("failed to execute update from the recorded local source");
    assert_success(&updated_local);
    assert_eq!(
        fs::read(agent_dir.join("RULES.md")).expect("updated local file is missing"),
        b"updated local source\n",
        "update without --source did not apply the recorded local source"
    );
    let updated_entry = read_entry();
    assert_eq!(
        updated_entry["source"]["path"], canonical_local_source,
        "update without --source changed the recorded local source"
    );
    assert!(
        updated_entry["ref"].is_null(),
        "a later local update restored the stale GitHub ref"
    );
}

#[cfg(unix)]
#[test]
fn install_accepts_full_https_github_source_offline_and_normalizes_identity() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source-tree");
    let agent_dir = temp.0.join("agent");
    let shorthand_agent_dir = temp.0.join("agent-shorthand");
    write_topology(
        &source_root,
        TOPOLOGY,
        "task:\n  eager: true\n",
        &[("RULES.md", b"offline GitHub fixture\n")],
    );

    let commit = "0123456789abcdef0123456789abcdef01234567";
    let fake_path = fake_github_git(&temp, "https://github.com/owner/repo.git", commit);
    let output = command(&temp)
        .env("PATH", &fake_path)
        .env("OMP_FAKE_GIT_TREE", &source_root)
        .args([
            "install",
            "--topology",
            TOPOLOGY,
            "--source",
            "https://github.com/OWNER/REPO",
            "--install-id",
            "https",
            "--agent-dir",
        ])
        .arg(&agent_dir)
        .arg("--json")
        .output()
        .expect("failed to execute install with fake git");
    assert_success(&output);
    assert_eq!(
        fs::read(agent_dir.join("RULES.md")).expect("offline GitHub install omitted file"),
        b"offline GitHub fixture\n"
    );

    let registry: serde_json::Value = serde_json::from_slice(
        &fs::read(temp.state_home().join("omp-topology/registry.json"))
            .expect("failed to read install registry"),
    )
    .expect("install registry was not valid JSON");
    assert_eq!(
        registry["aliases"]["https"]["source"],
        serde_json::json!({
            "kind": "github",
            "owner": "owner",
            "repo": "repo"
        }),
        "full HTTPS GitHub URL was not normalized to its canonical source identity"
    );
    let shorthand_output = command(&temp)
        .env("PATH", &fake_path)
        .env("OMP_FAKE_GIT_TREE", &source_root)
        .args([
            "install",
            "--topology",
            TOPOLOGY,
            "--source",
            "github:OWNER/REPO",
            "--install-id",
            "shorthand",
            "--agent-dir",
        ])
        .arg(&shorthand_agent_dir)
        .arg("--json")
        .output()
        .expect("failed to execute shorthand install with fake git");
    assert_success(&shorthand_output);
    assert_eq!(
        fs::read(shorthand_agent_dir.join("RULES.md"))
            .expect("offline shorthand GitHub install omitted file"),
        b"offline GitHub fixture\n"
    );

    let registry: serde_json::Value = serde_json::from_slice(
        &fs::read(temp.state_home().join("omp-topology/registry.json"))
            .expect("failed to read install registry"),
    )
    .expect("install registry was not valid JSON");
    assert_eq!(
        registry["aliases"]["shorthand"]["source"],
        serde_json::json!({
            "kind": "github",
            "owner": "owner",
            "repo": "repo"
        }),
        "github:OWNER/REPO was not normalized to its canonical source identity"
    );
}

#[test]
fn update_preserves_installed_at_for_unchanged_local_source() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("agent");
    write_topology(
        &source_root,
        TOPOLOGY,
        "task:\n  eager: true\n",
        &[("RULES.md", b"managed rules\n")],
    );
    assert_success(&install_source(
        &temp,
        &agent_dir,
        &source_root,
        TOPOLOGY,
        Some("stable"),
        false,
    ));

    let before = json_document(&run(&temp, &["status", "--install-id", "stable", "--json"]));
    let installed_at = before["installed_at"]
        .as_str()
        .expect("status omitted installed_at")
        .to_string();

    // An unchanged source normally takes update's no-op path. Make its
    // recorded digest stale so this exercises a refresh without changing
    // the local source itself.
    let registry_path = temp.state_home().join("omp-topology/registry.json");
    let mut registry: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("failed to read install registry"))
            .expect("failed to parse install registry");
    let recorded_digest = registry["aliases"]["stable"]["overlay_digest"]
        .as_str()
        .expect("install registry omitted overlay digest");
    assert_eq!(recorded_digest.len(), 64, "overlay digest was not SHA-256");
    assert!(
        recorded_digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "overlay digest was not hexadecimal"
    );
    let stale_prefix = if recorded_digest.starts_with('0') {
        "1"
    } else {
        "0"
    };
    let mut stale_digest = recorded_digest.to_owned();
    stale_digest.replace_range(..1, stale_prefix);
    registry["aliases"]["stable"]["overlay_digest"] = serde_json::json!(stale_digest);
    fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&registry).expect("failed to encode install registry"),
    )
    .expect("failed to stage stale digest");

    let drifted = json_document(&run(&temp, &["status", "--install-id", "stable", "--json"]));
    assert_eq!(drifted["freshness"]["status"], "update-available");

    let updated = json_document(&run(&temp, &["update", "--install-id", "stable", "--json"]));
    assert_eq!(updated["command"], "update");

    let after = json_document(&run(&temp, &["status", "--install-id", "stable", "--json"]));
    assert_eq!(
        after["installed_at"], installed_at,
        "updating an existing install ID reset its original installation time"
    );
    assert_eq!(after["freshness"]["status"], "up-to-date");
}

#[test]
fn lifecycle_json_install_id_and_profile_discovery_are_consumer_visible() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("MyAgent");
    write_topology(
        &source_root,
        TOPOLOGY,
        "task:\n  eager: true\n",
        &[("RULES.md", b"managed rules\n")],
    );

    let installed = install_source(&temp, &agent_dir, &source_root, TOPOLOGY, None, true);
    let install_doc = json_document(&installed);
    assert_eq!(install_doc["command"], "install");
    assert!(
        install_doc["lines"].as_array().is_some_and(|lines| {
            lines.iter().any(|line| {
                line.as_str()
                    .is_some_and(|line| line.contains("recorded install ID 'myagent'"))
            })
        }),
        "install did not expose its lowercased default install ID: {install_doc}"
    );
    assert!(
        install_doc.get("alias").is_none(),
        "install JSON exposed a registry alias field: {install_doc}"
    );

    let status = json_document(&run(
        &temp,
        &["status", "--install-id", "myagent", "--json"],
    ));
    assert_eq!(status["command"], "status");
    assert_eq!(status["managed"], true);
    assert_eq!(status["install_id"], "myagent");
    assert!(
        status.get("alias").is_none(),
        "status JSON exposed a registry alias field: {status}"
    );

    let profiles = json_document(&run(&temp, &["profiles", "list", "--json"]));
    assert_eq!(profiles["command"], "profiles list");
    let resolved_agent = fs::canonicalize(&agent_dir)
        .expect("failed to resolve installed agent directory")
        .display()
        .to_string();
    assert!(
        profiles["profiles"].as_array().is_some_and(|profiles| {
            profiles.iter().any(|profile| {
                profile["agent_dir"] == resolved_agent
                    && profile["managed"].as_array().is_some_and(|managed| {
                        managed.iter().any(|item| {
                            item["install_id"] == "myagent" && item.get("alias").is_none()
                        })
                    })
            })
        }),
        "profile discovery omitted the registry-managed custom target: {profiles}"
    );

    let removed = json_document(&run(
        &temp,
        &["remove", "--install-id", "myagent", "--json"],
    ));
    assert_eq!(removed["command"], "remove");
    assert!(!agent_dir.join("RULES.md").exists());
    let missing_install_id = run(&temp, &["status", "--install-id", "myagent", "--json"]);
    assert!(
        !missing_install_id.status.success(),
        "status accepted an install ID after its install was removed"
    );
    assert!(
        missing_install_id.stdout.is_empty(),
        "failed JSON command wrote non-JSON stdout"
    );
}

#[test]
fn second_install_id_dry_run_and_lifecycle_are_isolated_from_default_install() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("omp-dps");
    let shared_config = "task:\n  eager: true\n";
    write_topology(
        &source_root,
        "omp-dps",
        shared_config,
        &[("agents/dps.md", b"dps rules\n")],
    );
    let second_topology = write_topology(
        &source_root,
        "omp-dps-plus",
        shared_config,
        &[("agents/dps-plus.md", b"dps-plus v1\n")],
    );
    fs::remove_file(second_topology.join("agent/config.yml"))
        .expect("failed to make second topology config-free");

    let first_install = install_source(&temp, &agent_dir, &source_root, "omp-dps", None, false);
    assert_success(&first_install);
    let first_manifest = manifest_path(&temp);
    let first_manifest_before =
        fs::read(&first_manifest).expect("failed to read first alias manifest");
    let first_file = agent_dir.join("agents/dps.md");
    let first_file_before = fs::read(&first_file).expect("first alias file is missing");
    let target_before_dry_run = file_snapshot(&agent_dir);
    let state_root = temp.state_home().join("omp-topology");
    let state_before_dry_run = file_snapshot(&state_root);

    let source_arg = source_root.to_string_lossy().into_owned();
    let agent_dir_arg = agent_dir.to_string_lossy().into_owned();
    let dry_run = run(
        &temp,
        &[
            "install",
            "--topology",
            "omp-dps-plus",
            "--source",
            &source_arg,
            "--install-id",
            "omp-dps-plus",
            "--dry-run",
            "--agent-dir",
            &agent_dir_arg,
        ],
    );
    assert_success(&dry_run);
    assert_eq!(
        file_snapshot(&agent_dir),
        target_before_dry_run,
        "second-alias dry-run changed target files"
    );
    assert_eq!(
        file_snapshot(&state_root),
        state_before_dry_run,
        "second-alias dry-run changed state"
    );

    let second_install = install_source(
        &temp,
        &agent_dir,
        &source_root,
        "omp-dps-plus",
        Some("omp-dps-plus"),
        false,
    );
    assert_success(&second_install);
    assert_eq!(
        fs::read(&first_manifest).expect("second install removed first alias manifest"),
        first_manifest_before,
        "second install changed first alias ownership"
    );
    assert_eq!(
        fs::read(&first_file).expect("second install removed first alias file"),
        first_file_before
    );
    let manifests = files_under(&state_root)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name == "manifest.json" || name == "omp-dps-plus.json")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        manifests.len(),
        2,
        "second alias did not receive independent ownership state: {manifests:?}"
    );
    let second_manifest = manifests
        .into_iter()
        .find(|path| path != &first_manifest)
        .expect("second alias reused the first alias manifest path");
    let second_manifest_doc: serde_json::Value =
        serde_json::from_slice(&fs::read(&second_manifest).expect("second manifest is missing"))
            .expect("second alias manifest is not valid JSON");
    assert_eq!(second_manifest_doc["topology"], "omp-dps-plus");

    let first_status = json_document(&run(
        &temp,
        &["status", "--install-id", "omp-dps", "--json"],
    ));
    assert_eq!(first_status["manifest"], "healthy");
    let second_status = json_document(&run(
        &temp,
        &["status", "--install-id", "omp-dps-plus", "--json"],
    ));
    assert_eq!(second_status["manifest"], "healthy");
    assert_eq!(second_status["topology"], "omp-dps-plus");

    let lifecycle_target_before_dry_run = file_snapshot(&agent_dir);
    let lifecycle_state_before_dry_run = file_snapshot(&state_root);
    let update_dry_run = run(
        &temp,
        &["update", "--install-id", "omp-dps-plus", "--dry-run"],
    );
    assert_success(&update_dry_run);
    assert_eq!(
        file_snapshot(&agent_dir),
        lifecycle_target_before_dry_run,
        "multi-alias update dry-run changed target files"
    );
    assert_eq!(
        file_snapshot(&state_root),
        lifecycle_state_before_dry_run,
        "multi-alias update dry-run changed registry or ownership manifests"
    );
    let remove_dry_run = run(
        &temp,
        &["remove", "--install-id", "omp-dps-plus", "--dry-run"],
    );
    assert_success(&remove_dry_run);
    assert_eq!(
        file_snapshot(&agent_dir),
        lifecycle_target_before_dry_run,
        "multi-alias remove dry-run changed target files"
    );
    assert_eq!(
        file_snapshot(&state_root),
        lifecycle_state_before_dry_run,
        "multi-alias remove dry-run changed registry or ownership manifests"
    );

    let second_file = agent_dir.join("agents/dps-plus.md");
    fs::write(
        second_topology.join("agent/agents/dps-plus.md"),
        b"dps-plus v2\n",
    )
    .expect("failed to update second topology fixture");
    let update = run(&temp, &["update", "--install-id", "omp-dps-plus", "--json"]);
    assert_eq!(json_document(&update)["command"], "update");
    assert_eq!(
        fs::read(&second_file).expect("second alias update removed its file"),
        b"dps-plus v2\n"
    );
    assert_eq!(
        fs::read(&first_file).expect("second alias update removed first alias file"),
        first_file_before,
        "second alias update changed the first alias file"
    );
    assert_eq!(
        fs::read(&first_manifest).expect("second alias update removed first manifest"),
        first_manifest_before,
        "second alias update changed first alias ownership"
    );
    let second_status = json_document(&run(
        &temp,
        &["status", "--install-id", "omp-dps-plus", "--json"],
    ));
    assert_eq!(second_status["freshness"]["status"], "up-to-date");

    let removed = json_document(&run(
        &temp,
        &["remove", "--install-id", "omp-dps-plus", "--json"],
    ));
    assert_eq!(removed["command"], "remove");
    assert!(!second_file.exists(), "remove retained second alias file");
    assert_eq!(
        fs::read(&first_file).expect("removing second alias removed first alias file"),
        first_file_before
    );
    assert_eq!(
        fs::read(&first_manifest).expect("removing second alias removed first manifest"),
        first_manifest_before,
        "removing second alias changed first alias ownership"
    );
    assert!(
        !second_manifest.exists(),
        "removing second alias retained its ownership manifest"
    );
    assert_eq!(
        json_document(&run(
            &temp,
            &["status", "--install-id", "omp-dps", "--json"]
        ))["manifest"],
        "healthy"
    );
    let removed_status = run(&temp, &["status", "--install-id", "omp-dps-plus", "--json"]);
    assert!(
        !removed_status.status.success(),
        "status still found the removed alias:\n{}",
        output_text(&removed_status)
    );
}

#[test]

fn second_alias_config_merge_cannot_rewrite_another_alias_owned_key() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("omp-dps");
    write_topology(
        &source_root,
        "omp-dps",
        "task:\n  eager: true\n",
        &[("agents/dps.md", b"dps rules\n")],
    );
    write_topology(
        &source_root,
        "omp-dps-plus",
        "task:\n  eager: false\n",
        &[("agents/dps-plus.md", b"dps-plus rules\n")],
    );

    let first_install = install_source(&temp, &agent_dir, &source_root, "omp-dps", None, false);
    assert_success(&first_install);
    let first_manifest = manifest_path(&temp);
    let first_manifest_before =
        fs::read(&first_manifest).expect("failed to read first alias manifest");
    let target_before = file_snapshot(&agent_dir);
    let state_root = temp.state_home().join("omp-topology");
    let state_before = file_snapshot(&state_root);

    let source_arg = source_root.to_string_lossy().into_owned();
    let agent_dir_arg = agent_dir.to_string_lossy().into_owned();
    let rejected = run(
        &temp,
        &[
            "install",
            "--topology",
            "omp-dps-plus",
            "--source",
            &source_arg,
            "--install-id",
            "omp-dps-plus",
            "--config-merge",
            "--config-precedence",
            "topology",
            "--agent-dir",
            &agent_dir_arg,
        ],
    );
    assert!(
        !rejected.status.success(),
        "second alias rewrote a config key owned by the first alias"
    );
    let report = output_text(&rejected);
    assert!(
        report.contains("omp-dps") && report.contains("config"),
        "config ownership conflict was not identified:\n{report}"
    );
    assert_eq!(
        file_snapshot(&agent_dir),
        target_before,
        "rejected config-key collision changed target files"
    );
    assert_eq!(
        file_snapshot(&state_root),
        state_before,
        "rejected config-key collision changed registry or ownership state"
    );
    assert_eq!(
        fs::read(&first_manifest).expect("rejected install removed first manifest"),
        first_manifest_before
    );
    assert_eq!(
        json_document(&run(
            &temp,
            &["status", "--install-id", "omp-dps", "--json"]
        ))["manifest"],
        "healthy"
    );
    let second_status = run(&temp, &["status", "--install-id", "omp-dps-plus", "--json"]);
    assert!(
        !second_status.status.success(),
        "rejected config-key collision registered the second alias"
    );
}

#[test]

fn second_alias_rejects_shared_file_collision_without_mutation() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("omp-dps");
    let shared_config = "task:\n  eager: true\n";
    write_topology(
        &source_root,
        "omp-dps",
        shared_config,
        &[("RULES.md", b"first alias rules\n")],
    );
    let second_topology = write_topology(
        &source_root,
        "omp-dps-plus",
        shared_config,
        &[("RULES.md", b"second alias rules\n")],
    );
    fs::remove_file(second_topology.join("agent/config.yml"))
        .expect("failed to make colliding topology config-free");

    let first_install = install_source(&temp, &agent_dir, &source_root, "omp-dps", None, false);
    assert_success(&first_install);
    let first_manifest = manifest_path(&temp);
    let target_before = file_snapshot(&agent_dir);
    let state_root = temp.state_home().join("omp-topology");
    let state_before = file_snapshot(&state_root);

    let rejected = install_source(
        &temp,
        &agent_dir,
        &source_root,
        "omp-dps-plus",
        Some("omp-dps-plus"),
        false,
    );
    assert!(
        !rejected.status.success(),
        "second alias install accepted a conflicting target file"
    );
    assert_eq!(
        file_snapshot(&agent_dir),
        target_before,
        "failed second-alias install mutated target files"
    );
    assert_eq!(
        file_snapshot(&state_root),
        state_before,
        "failed second-alias install mutated registry or ownership state"
    );
    assert!(
        first_manifest.exists(),
        "failed second-alias install removed first alias manifest"
    );
    assert_eq!(
        json_document(&run(
            &temp,
            &["status", "--install-id", "omp-dps", "--json"]
        ))["manifest"],
        "healthy"
    );
    let second_status = run(&temp, &["status", "--install-id", "omp-dps-plus", "--json"]);
    assert!(
        !second_status.status.success(),
        "failed second-alias install registered a new alias:\n{}",
        output_text(&second_status)
    );
}

#[test]
fn second_alias_rejects_byte_identical_shared_file_without_mutation() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("shared-agent");
    let shared_config = "task:\n  eager: true\n";
    write_topology(
        &source_root,
        "shared-first",
        shared_config,
        &[("RULES.md", b"shared rules\n")],
    );
    let second_topology = write_topology(
        &source_root,
        "shared-second",
        shared_config,
        &[("RULES.md", b"shared rules\n")],
    );
    fs::remove_file(second_topology.join("agent/config.yml"))
        .expect("failed to make second topology config-free");

    let first_install = install_source(
        &temp,
        &agent_dir,
        &source_root,
        "shared-first",
        Some("first-alias"),
        false,
    );
    assert_success(&first_install);
    let first_manifest = manifest_path(&temp);
    let first_manifest_before = fs::read(&first_manifest).expect("first alias manifest is missing");
    let shared_file = agent_dir.join("RULES.md");
    let shared_file_before = fs::read(&shared_file).expect("first alias file is missing");
    let target_before = file_snapshot(&agent_dir);
    let state_root = temp.state_home().join("omp-topology");
    let state_before = file_snapshot(&state_root);

    let rejected = install_source(
        &temp,
        &agent_dir,
        &source_root,
        "shared-second",
        Some("second-alias"),
        false,
    );
    assert!(
        !rejected.status.success(),
        "second alias accepted a byte-identical file already owned by the first alias"
    );
    assert_eq!(
        file_snapshot(&agent_dir),
        target_before,
        "rejected second-alias install changed target files"
    );
    assert_eq!(
        file_snapshot(&state_root),
        state_before,
        "rejected second-alias install changed registry or ownership state"
    );
    assert_eq!(
        fs::read(&shared_file).expect("rejected install removed the first alias file"),
        shared_file_before
    );
    assert_eq!(
        fs::read(&first_manifest).expect("rejected install removed first alias manifest"),
        first_manifest_before
    );
    assert_eq!(
        json_document(&run(
            &temp,
            &["status", "--install-id", "first-alias", "--json"]
        ))["manifest"],
        "healthy"
    );
    let second_status = run(&temp, &["status", "--install-id", "second-alias", "--json"]);
    assert!(
        !second_status.status.success(),
        "rejected install registered the second alias:\n{}",
        output_text(&second_status)
    );
}

#[test]
fn checked_in_digital_product_studio_alias_collisions_fail_closed_in_both_orders() {
    let temp = TestDir::new();
    let source_root = temp.0.join("collision-source");
    write_topology(
        &source_root,
        "collision-a",
        "task:\n  eager: true\n",
        &[("RULES.md", b"collision fixture rules\n")],
    );
    write_topology(
        &source_root,
        "collision-b",
        "task:\n  eager: true\n",
        &[("RULES.md", b"collision fixture rules\n")],
    );
    for (first_topology, first_alias, second_topology, second_alias) in [
        ("collision-a", "collision-a-owner", "collision-b", "collision-b-owner"),
        ("collision-b", "collision-b-owner", "collision-a", "collision-a-owner"),
    ] {
        let temp = TestDir::new();
        let agent_dir = temp.0.join("shared-agent");
        let first_install = install_source(
            &temp,
            &agent_dir,
            &source_root,
            first_topology,
            Some(first_alias),
            false,
        );
        assert_success(&first_install);

        let first_manifest = manifest_path(&temp);
        let first_manifest_before =
            fs::read(&first_manifest).expect("first alias manifest is missing");
        let target_before = file_snapshot(&agent_dir);
        let state_root = temp.state_home().join("omp-topology");
        let state_before = file_snapshot(&state_root);
        let source_arg = source_root.to_string_lossy().into_owned();

        let dry_run = run_for_agent(
            &temp,
            &agent_dir,
            &[
                "install",
                "--topology",
                second_topology,
                "--source",
                &source_arg,
                "--install-id",
                second_alias,
                "--dry-run",
            ],
        );
        assert!(
            !dry_run.status.success(),
            "dry-run accepted {second_topology} over {first_topology} in one agent directory:\n{}",
            output_text(&dry_run)
        );
        let dry_run_report = output_text(&dry_run);
        assert!(
            dry_run_report.contains(first_alias) && dry_run_report.contains("RULES.md"),
            "dry-run collision did not identify the first alias and shared RULES.md path:\n{dry_run_report}"
        );
        assert_eq!(
            file_snapshot(&agent_dir),
            target_before,
            "rejected synthetic-topology dry-run changed target files"
        );
        assert_eq!(
            file_snapshot(&state_root),
            state_before,
            "rejected synthetic-topology dry-run changed registry or manifests"
        );

        let rejected = install_source(
            &temp,
            &agent_dir,
            &source_root,
            second_topology,
            Some(second_alias),
            false,
        );
        assert!(
            !rejected.status.success(),
            "install accepted {second_topology} over {first_topology} in one agent directory:\n{}",
            output_text(&rejected)
        );
        let report = output_text(&rejected);
        assert!(
            report.contains(first_alias) && report.contains("RULES.md"),
            "collision did not identify the first alias and shared RULES.md path:\n{report}"
        );
        assert_eq!(
            file_snapshot(&agent_dir),
            target_before,
            "rejected synthetic-topology install changed target files"
        );
        assert_eq!(
            file_snapshot(&state_root),
            state_before,
            "rejected synthetic-topology install changed registry or manifests"
        );
        assert_eq!(
            fs::read(&first_manifest).expect("rejected install changed first alias manifest"),
            first_manifest_before
        );
        assert_eq!(
            json_document(&run(
                &temp,
                &["status", "--install-id", first_alias, "--json"]
            ))["manifest"],
            "healthy",
            "first alias became unhealthy after the rejected collision"
        );
        let second_status = run(&temp, &["status", "--install-id", second_alias, "--json"]);
        assert!(
            !second_status.status.success(),
            "rejected collision registered the second alias:\n{}",
            output_text(&second_status)
        );
    }
}

#[test]
fn second_alias_cannot_claim_first_alias_config_candidate() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("candidate-agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    let config = agent_dir.join("config.yml");
    let original_config = b"local:\n  value: keep\n";
    fs::write(&config, original_config).expect("failed to seed active config");

    let first_config = "first:\n  value: owned\n";
    write_topology(
        &source_root,
        "candidate-first",
        first_config,
        &[("agents/candidate-first.md", b"first alias\n")],
    );
    write_topology(
        &source_root,
        "candidate-second",
        "second:\n  value: disjoint\n",
        &[("agents/candidate-second.md", b"second alias\n")],
    );

    let source_arg = source_root.to_string_lossy().into_owned();
    let first_install = run_for_agent(
        &temp,
        &agent_dir,
        &[
            "install",
            "--topology",
            "candidate-first",
            "--source",
            &source_arg,
            "--install-id",
            "candidate-owner",
        ],
    );
    assert_success(&first_install);
    let candidate = agent_dir.join("config.yml.new");
    assert_eq!(
        fs::read(&candidate).expect("first alias did not publish its config candidate"),
        first_config.as_bytes()
    );
    let first_manifest = manifest_path(&temp);
    let first_manifest_before = fs::read(&first_manifest).expect("first alias manifest is missing");
    let target_before = file_snapshot(&agent_dir);
    let state_root = temp.state_home().join("omp-topology");
    let state_before = file_snapshot(&state_root);

    let rejected = run_for_agent(
        &temp,
        &agent_dir,
        &[
            "install",
            "--topology",
            "candidate-second",
            "--source",
            &source_arg,
            "--install-id",
            "candidate-contender",
        ],
    );
    assert!(
        !rejected.status.success(),
        "second alias claimed the first alias's config.yml.new candidate"
    );
    let report = output_text(&rejected);
    assert!(
        report.contains("candidate-owner") && report.contains("config.yml.new"),
        "candidate collision did not identify its owner and path:\n{report}"
    );
    assert_eq!(
        file_snapshot(&agent_dir),
        target_before,
        "rejected candidate collision changed active config, candidate, or target files"
    );
    assert_eq!(
        fs::read(&config).expect("rejected candidate install changed active config"),
        original_config
    );
    assert_eq!(
        fs::read(&candidate).expect("rejected candidate install removed the candidate"),
        first_config.as_bytes()
    );
    assert_eq!(
        file_snapshot(&state_root),
        state_before,
        "rejected candidate collision changed registry or manifests"
    );
    assert_eq!(
        fs::read(&first_manifest).expect("rejected install changed first alias manifest"),
        first_manifest_before
    );
    assert_eq!(
        json_document(&run(
            &temp,
            &["status", "--install-id", "candidate-owner", "--json"]
        ))["manifest"],
        "healthy"
    );
}

#[test]
fn remove_scopes_same_relative_skills_to_each_recorded_root() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("shared-agent");
    let first_topology = write_topology(
        &source_root,
        "skills-first",
        "",
        &[
            ("agents/first.md", b"first alias\n"),
            ("skills/shared/SKILL.md", b"first root skill\n"),
        ],
    );
    let second_topology = write_topology(
        &source_root,
        "skills-second",
        "",
        &[
            ("agents/second.md", b"second alias\n"),
            ("skills/shared/SKILL.md", b"second root skill\n"),
        ],
    );
    fs::remove_file(first_topology.join("agent/config.yml"))
        .expect("failed to make first topology config-free");
    fs::remove_file(second_topology.join("agent/config.yml"))
        .expect("failed to make second topology config-free");

    let source_arg = source_root.to_string_lossy().into_owned();
    let first_skills_root = temp.0.join("skills-first-root");
    let first_skills_arg = first_skills_root.to_string_lossy().into_owned();
    let first_install = run_for_agent(
        &temp,
        &agent_dir,
        &[
            "install",
            "--topology",
            "skills-first",
            "--source",
            &source_arg,
            "--install-id",
            "skills-first-owner",
            "--skills-dir",
            &first_skills_arg,
        ],
    );
    assert_success(&first_install);

    let second_skills_root = temp.0.join("skills-second-root");
    let second_skills_arg = second_skills_root.to_string_lossy().into_owned();
    let second_install = run_for_agent(
        &temp,
        &agent_dir,
        &[
            "install",
            "--topology",
            "skills-second",
            "--source",
            &source_arg,
            "--install-id",
            "skills-second-owner",
            "--skills-dir",
            &second_skills_arg,
        ],
    );
    assert_success(&second_install);

    let first_skill = first_skills_root.join("shared/SKILL.md");
    let second_skill = second_skills_root.join("shared/SKILL.md");
    assert_eq!(
        fs::read(&first_skill).expect("first alias skill is missing"),
        b"first root skill\n"
    );
    assert_eq!(
        fs::read(&second_skill).expect("second alias skill is missing"),
        b"second root skill\n"
    );

    let removed = run(&temp, &["remove", "--install-id", "skills-first-owner"]);
    assert_success(&removed);
    assert!(
        !first_skill.exists(),
        "removing the first alias retained its skill under its recorded root"
    );
    assert_eq!(
        fs::read(&second_skill).expect("removing the first alias deleted the sibling skill"),
        b"second root skill\n"
    );
    assert!(
        !agent_dir.join("agents/first.md").exists(),
        "removing the first alias retained its agent file"
    );
    assert!(
        agent_dir.join("agents/second.md").is_file(),
        "removing the first alias deleted the sibling agent file"
    );
    assert_eq!(
        json_document(&run(
            &temp,
            &["status", "--install-id", "skills-second-owner", "--json"]
        ))["manifest"],
        "healthy"
    );
}

#[test]
fn replace_update_cannot_remove_another_alias_merge_owned_config_key() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("replace-agent");
    fs::create_dir_all(&agent_dir).expect("failed to create isolated agent directory");
    fs::write(agent_dir.join("config.yml"), b"local:\n  value: original\n")
        .expect("failed to seed active config");

    let first_topology = write_topology(
        &source_root,
        "replace-first",
        "replace_owned:\n  value: first\n",
        &[("agents/replace-first.md", b"first alias\n")],
    );
    write_topology(
        &source_root,
        "merge-second",
        "merge_owned:\n  value: sibling\n",
        &[("agents/merge-second.md", b"second alias\n")],
    );
    let source_arg = source_root.to_string_lossy().into_owned();
    let first_install = run_for_agent(
        &temp,
        &agent_dir,
        &[
            "install",
            "--topology",
            "replace-first",
            "--source",
            &source_arg,
            "--install-id",
            "replace-owner",
            "--config-replace",
        ],
    );
    assert_success(&first_install);
    let second_install = run_for_agent(
        &temp,
        &agent_dir,
        &[
            "install",
            "--topology",
            "merge-second",
            "--source",
            &source_arg,
            "--install-id",
            "merge-owner",
            "--config-merge",
            "--config-precedence",
            "topology",
        ],
    );
    assert_success(&second_install);
    let config = agent_dir.join("config.yml");
    let config_before_update = fs::read(&config).expect("merged active config is missing");
    let config_text = String::from_utf8_lossy(&config_before_update);
    assert!(
        config_text.contains("replace_owned") && config_text.contains("merge_owned"),
        "setup did not establish both aliases' config keys:\n{config_text}"
    );

    fs::write(
        first_topology.join("agent/config.yml"),
        "replace_owned:\n  value: updated\n",
    )
    .expect("failed to update replace-owned topology config");
    let target_before = file_snapshot(&agent_dir);
    let state_root = temp.state_home().join("omp-topology");
    let state_before = file_snapshot(&state_root);

    let rejected = run(
        &temp,
        &[
            "update",
            "--install-id",
            "replace-owner",
            "--config-replace",
        ],
    );
    assert!(
        !rejected.status.success(),
        "replace update removed a disjoint config key owned by alias 'merge-owner':\n{}",
        output_text(&rejected)
    );
    let report = output_text(&rejected);
    assert!(
        report.contains("merge-owner") && report.contains("merge_owned"),
        "replace collision did not identify the sibling alias and owned key:\n{report}"
    );
    assert_eq!(
        file_snapshot(&agent_dir),
        target_before,
        "rejected replace update changed target files or config"
    );
    assert_eq!(
        fs::read(&config).expect("rejected replace update removed active config"),
        config_before_update
    );
    assert_eq!(
        file_snapshot(&state_root),
        state_before,
        "rejected replace update changed registry or ownership manifests"
    );
    let dry_run = run(
        &temp,
        &[
            "update",
            "--install-id",
            "replace-owner",
            "--config-replace",
            "--dry-run",
        ],
    );
    assert!(!dry_run.status.success());
    assert!(output_text(&dry_run).contains("merge-owner"));
    assert_eq!(
        file_snapshot(&agent_dir),
        target_before,
        "dry-run replace refusal changed target files or config"
    );
    assert_eq!(
        file_snapshot(&state_root),
        state_before,
        "dry-run replace refusal changed registry or ownership manifests"
    );
    assert_eq!(
        json_document(&run(
            &temp,
            &["status", "--install-id", "merge-owner", "--json"]
        ))["manifest"],
        "healthy"
    );
}

#[test]
fn second_alias_install_and_remove_preserve_first_alias_recovery_snapshot() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("recovery-alias-agent");
    let shared_config = "task:\n  eager: true\n";
    write_topology(
        &source_root,
        "recovery-first",
        shared_config,
        &[("FIRST.md", b"first alias rules\n")],
    );
    let second_topology = write_topology(
        &source_root,
        "recovery-second",
        shared_config,
        &[("SECOND.md", b"second alias rules\n")],
    );
    fs::remove_file(second_topology.join("agent/config.yml"))
        .expect("failed to make second recovery topology config-free");

    let first_install = install_source(
        &temp,
        &agent_dir,
        &source_root,
        "recovery-first",
        Some("first-alias"),
        false,
    );
    assert_success(&first_install);
    let state_root = manifest_path(&temp)
        .parent()
        .expect("first manifest has no agent state directory")
        .to_path_buf();
    let first_snapshot = snapshot_transaction_dirs(&state_root)
        .into_iter()
        .find(|snapshot| {
            snapshot
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("install-"))
        })
        .expect("first install produced no transaction snapshot");
    let snapshot_payload = files_under(&first_snapshot)
        .into_iter()
        .next()
        .expect("first install snapshot has no payload");
    let snapshot_payload_before =
        fs::read(&snapshot_payload).expect("failed to read first snapshot payload");
    let first_marker = first_snapshot.join(".recovery-required");
    let first_marker_bytes =
        b"retained: recovery required after incomplete rollback (alias 'first-alias')\n";
    fs::write(&first_marker, first_marker_bytes)
        .expect("failed to seed first alias recovery marker");

    let snapshots_before_second_install = snapshot_transaction_dirs(&state_root);
    let second_install = install_source(
        &temp,
        &agent_dir,
        &source_root,
        "recovery-second",
        Some("second-alias"),
        false,
    );
    assert_success(&second_install);
    assert!(
        first_snapshot.is_dir(),
        "second alias install deleted the first alias recovery snapshot"
    );
    assert_eq!(
        fs::read(&first_marker).expect("second alias install cleared first alias marker"),
        first_marker_bytes,
        "second alias install changed the first alias recovery marker"
    );
    assert_eq!(
        fs::read(&snapshot_payload).expect("second alias install deleted snapshot payload"),
        snapshot_payload_before
    );

    let second_snapshot = snapshot_transaction_dirs(&state_root)
        .difference(&snapshots_before_second_install)
        .find(|snapshot| {
            snapshot
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("install-"))
        })
        .expect("second install produced no new transaction snapshot")
        .to_path_buf();
    let second_marker = second_snapshot.join(".recovery-required");
    let second_marker_bytes =
        b"retained: recovery required after incomplete rollback (alias 'second-alias')\n";
    fs::write(&second_marker, second_marker_bytes)
        .expect("failed to seed second alias recovery marker");

    let removed = run(&temp, &["remove", "--install-id", "second-alias", "--json"]);
    assert_eq!(json_document(&removed)["command"], "remove");
    assert!(
        first_snapshot.is_dir(),
        "second alias remove deleted the first alias recovery snapshot"
    );
    assert_eq!(
        fs::read(&first_marker).expect("second alias remove cleared first alias marker"),
        first_marker_bytes,
        "second alias remove changed the first alias recovery marker"
    );
    assert_eq!(
        fs::read(&snapshot_payload).expect("second alias remove deleted snapshot payload"),
        snapshot_payload_before
    );
    assert!(
        !second_marker.exists(),
        "successful second alias remove retained its own recovery marker"
    );
    assert_eq!(
        json_document(&run(
            &temp,
            &["status", "--install-id", "first-alias", "--json"]
        ))["manifest"],
        "healthy"
    );
}

#[test]

fn aliases_remain_per_alias_after_legacy_owner_is_removed() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("omp-dps");
    write_topology(
        &source_root,
        "omp-dps",
        "task:\n  eager: true\n",
        &[("agents/dps.md", b"dps rules\n")],
    );
    let b_topology = write_topology(
        &source_root,
        "omp-dps-plus",
        "task:\n  eager: true\n",
        &[("agents/dps-plus.md", b"dps-plus v1\n")],
    );
    fs::remove_file(b_topology.join("agent/config.yml"))
        .expect("failed to make second topology config-free");
    write_topology(
        &source_root,
        "omp-dps-ci",
        "task:\n  eager: true\n",
        &[("agents/dps-ci.md", b"dps-ci v1\n")],
    );
    let c_topology = source_root.join("topologies/omp-dps-ci");
    fs::remove_file(c_topology.join("agent/config.yml"))
        .expect("failed to make third topology config-free");

    let first_install = install_source(&temp, &agent_dir, &source_root, "omp-dps", None, false);
    assert_success(&first_install);
    let legacy_manifest = manifest_path(&temp);

    let b_install = install_source(
        &temp,
        &agent_dir,
        &source_root,
        "omp-dps-plus",
        Some("omp-dps-plus"),
        false,
    );
    assert_success(&b_install);
    let state_root = temp.state_home().join("omp-topology");
    let manifests_after_b = files_under(&state_root)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name == "manifest.json" || name == "omp-dps-plus.json")
        })
        .collect::<Vec<_>>();
    assert_eq!(manifests_after_b.len(), 2);
    let b_manifest = manifests_after_b
        .iter()
        .find(|path| path.as_path() != legacy_manifest.as_path())
        .expect("second alias did not get a per-alias manifest")
        .clone();
    let b_manifest_before = fs::read(&b_manifest).expect("second alias manifest is missing");
    let b_file = agent_dir.join("agents/dps-plus.md");

    let removed_a = run(&temp, &["remove", "--install-id", "omp-dps", "--json"]);
    assert_eq!(json_document(&removed_a)["command"], "remove");
    assert!(
        !legacy_manifest.exists(),
        "removing the first alias retained its manifest"
    );
    assert!(
        b_file.exists(),
        "removing the legacy owner removed the remaining alias file"
    );
    assert_eq!(
        fs::read(&b_manifest).expect("removing the legacy owner removed B's manifest"),
        b_manifest_before
    );
    assert_eq!(
        json_document(&run(
            &temp,
            &["status", "--install-id", "omp-dps-plus", "--json"]
        ))["manifest"],
        "healthy"
    );

    let c_install = install_source(
        &temp,
        &agent_dir,
        &source_root,
        "omp-dps-ci",
        Some("omp-dps-ci"),
        false,
    );
    assert_success(&c_install);
    assert!(
        !legacy_manifest.exists(),
        "new alias after legacy removal reused or recreated the legacy manifest"
    );
    let manifests_after_c = files_under(&state_root)
        .into_iter()
        .filter(|path| {
            path.file_name().is_some_and(|name| {
                name == "manifest.json" || name == "omp-dps-plus.json" || name == "omp-dps-ci.json"
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        manifests_after_c.len(),
        2,
        "installing C did not preserve independent per-alias manifests: {manifests_after_c:?}"
    );
    let c_manifest = manifests_after_c
        .iter()
        .find(|path| path.as_path() != b_manifest.as_path())
        .expect("third alias reused B's manifest")
        .clone();
    assert!(
        c_manifest
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "manifests"),
        "third alias was not assigned a per-alias manifest: {}",
        c_manifest.display()
    );
    let c_manifest_before = fs::read(&c_manifest).expect("third alias manifest is missing");
    let c_file = agent_dir.join("agents/dps-ci.md");
    assert_eq!(fs::read(&c_file).unwrap(), b"dps-ci v1\n");

    let direct_status = run_for_agent(&temp, &agent_dir, &["status", "--json"]);
    assert!(
        !direct_status.status.success(),
        "status accepted an ambiguous target without --install-id"
    );
    assert!(
        output_text(&direct_status).contains("pass --install-id NAME explicitly"),
        "ambiguous status did not require an explicit install ID:\n{}",
        output_text(&direct_status)
    );

    fs::write(
        b_topology.join("agent/agents/dps-plus.md"),
        b"dps-plus v2\n",
    )
    .expect("failed to update B's topology fixture");
    let update_b = run(&temp, &["update", "--install-id", "omp-dps-plus", "--json"]);
    assert_eq!(json_document(&update_b)["command"], "update");
    assert_eq!(fs::read(&b_file).unwrap(), b"dps-plus v2\n");
    assert_eq!(
        fs::read(&c_file).expect("updating B removed C's file"),
        b"dps-ci v1\n"
    );
    assert_eq!(
        fs::read(&c_manifest).expect("updating B removed C's manifest"),
        c_manifest_before,
        "updating B changed C's ownership"
    );

    let removed_b = run(&temp, &["remove", "--install-id", "omp-dps-plus", "--json"]);
    assert_eq!(json_document(&removed_b)["command"], "remove");
    assert!(!b_file.exists(), "removing B retained B's file");
    assert!(
        !b_manifest.exists(),
        "removing B retained B's per-alias manifest"
    );
    assert_eq!(
        fs::read(&c_file).expect("removing B removed C's file"),
        b"dps-ci v1\n"
    );
    assert_eq!(
        fs::read(&c_manifest).expect("removing B removed C's manifest"),
        c_manifest_before,
        "removing B changed C's ownership"
    );
    assert!(
        !legacy_manifest.exists(),
        "removing B recreated the legacy manifest"
    );
    assert_eq!(
        json_document(&run(
            &temp,
            &["status", "--install-id", "omp-dps-ci", "--json"]
        ))["manifest"],
        "healthy"
    );
}

#[test]

fn malformed_registry_fails_closed_before_install_mutates_target() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    write_topology(
        &source_root,
        TOPOLOGY,
        "task:\n  eager: true\n",
        &[("RULES.md", b"managed rules\n")],
    );
    let registry = temp.state_home().join("omp-topology/registry.json");
    fs::create_dir_all(registry.parent().unwrap()).expect("failed to create registry directory");
    fs::write(&registry, b"{\"version\":99,\"aliases\":{}}\n")
        .expect("failed to seed unsupported registry");
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create target agent directory");
    let user_file = agent_dir.join("user-owned.md");
    fs::write(&user_file, b"must remain untouched\n").expect("failed to seed user file");

    let output = install_source(
        &temp,
        &agent_dir,
        &source_root,
        TOPOLOGY,
        Some("registry"),
        false,
    );
    assert!(
        !output.status.success(),
        "install accepted an unsupported registry version"
    );
    assert_eq!(
        fs::read(&user_file).unwrap().as_slice(),
        b"must remain untouched\n"
    );
    assert!(
        !agent_dir.join("config.yml").exists(),
        "install mutated config after registry validation failed"
    );
    assert!(
        !agent_dir.join("RULES.md").exists(),
        "install copied owned files after registry validation failed"
    );
}

#[test]
fn invalid_registry_install_id_uses_public_terminology_and_fails_closed() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    write_topology(
        &source_root,
        TOPOLOGY,
        "task:\n  eager: true\n",
        &[("RULES.md", b"managed rules\n")],
    );
    let registry = temp.state_home().join("omp-topology/registry.json");
    fs::create_dir_all(registry.parent().unwrap()).expect("failed to create registry directory");
    fs::write(
        &registry,
        serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "aliases": {
                "Invalid ID": {}
            }
        }))
        .expect("failed to encode invalid install ID registry"),
    )
    .expect("failed to seed invalid install ID registry");
    let agent_dir = temp.0.join("agent");
    fs::create_dir_all(&agent_dir).expect("failed to create target agent directory");
    let user_file = agent_dir.join("user-owned.md");
    fs::write(&user_file, b"must remain untouched\n").expect("failed to seed user file");

    let output = install_source(
        &temp,
        &agent_dir,
        &source_root,
        TOPOLOGY,
        Some("valid-install-id"),
        false,
    );
    assert!(
        !output.status.success(),
        "install accepted an invalid registry install ID"
    );
    let diagnostic = output_text(&output);
    assert_eq!(
        fs::read(&user_file).expect("invalid registry validation changed user data"),
        b"must remain untouched\n"
    );
    assert!(
        !agent_dir.join("config.yml").exists(),
        "install mutated config after registry validation failed"
    );
    assert!(
        !agent_dir.join("RULES.md").exists(),
        "install copied owned files after registry validation failed"
    );
    assert!(
        diagnostic.contains("invalid install ID"),
        "registry validation did not use public install ID terminology:\n{diagnostic}"
    );
    assert!(
        !diagnostic.contains("invalid alias"),
        "registry validation leaked internal alias terminology:\n{diagnostic}"
    );
}

#[test]
fn malformed_manifest_fails_closed_before_remove_mutates_target() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("agent");
    write_topology(
        &source_root,
        TOPOLOGY,
        "task:\n  eager: true\n",
        &[("RULES.md", b"managed rules\n")],
    );
    let installed = install_source(
        &temp,
        &agent_dir,
        &source_root,
        TOPOLOGY,
        Some("manifest"),
        false,
    );
    assert_success(&installed);

    let rules = agent_dir.join("RULES.md");
    let rules_before = fs::read(&rules).expect("installed rules are missing");
    let config = agent_dir.join("config.yml");
    let config_before = fs::read(&config).expect("installed config is missing");
    let manifest = manifest_path(&temp);
    fs::write(&manifest, b"{ malformed manifest\n").expect("failed to corrupt manifest");

    let removed = run_for_agent(&temp, &agent_dir, &["remove", "--topology", TOPOLOGY]);
    assert!(
        !removed.status.success(),
        "remove accepted malformed ownership state"
    );
    assert_eq!(fs::read(&rules).unwrap(), rules_before);
    assert_eq!(fs::read(&config).unwrap(), config_before);
    assert_eq!(fs::read(&manifest).unwrap(), b"{ malformed manifest\n");
}

#[test]
fn malformed_ownership_config_fails_closed_before_remove_mutates_target() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("agent");
    write_topology(
        &source_root,
        TOPOLOGY,
        "task:\n  eager: true\n",
        &[("RULES.md", b"managed rules\n")],
    );
    fs::create_dir_all(&agent_dir).expect("failed to create target agent directory");
    fs::write(
        agent_dir.join("config.yml"),
        b"task:\n  eager: false\n  user_setting: keep\n",
    )
    .expect("failed to seed user config");
    let installed = install_source(
        &temp,
        &agent_dir,
        &source_root,
        TOPOLOGY,
        Some("bad-config"),
        false,
    );
    assert_success(&installed);

    let rules = agent_dir.join("RULES.md");
    let rules_before = fs::read(&rules).expect("installed rules are missing");
    let config = agent_dir.join("config.yml");
    let config_before = fs::read(&config).expect("installed config is missing");
    let manifest = manifest_path(&temp);
    let mut ownership: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).expect("failed to read ownership manifest"))
            .expect("ownership manifest was not valid JSON");
    ownership["config"]["prior"] = serde_json::json!("malformed prior-value map");
    fs::write(
        &manifest,
        serde_json::to_vec(&ownership).expect("failed to serialize corrupted manifest"),
    )
    .expect("failed to corrupt ownership config");
    let malformed_manifest =
        fs::read(&manifest).expect("failed to read corrupted ownership manifest");

    let removed = run_for_agent(&temp, &agent_dir, &["remove", "--topology", TOPOLOGY]);
    assert!(
        !removed.status.success(),
        "remove accepted malformed config ownership data:\n{}",
        output_text(&removed)
    );
    assert_eq!(
        fs::read(&rules).expect("remove deleted managed rules"),
        rules_before,
        "remove mutated managed files after config ownership decoding failed"
    );
    assert_eq!(
        fs::read(&config).expect("remove deleted user config"),
        config_before,
        "remove changed config after config ownership decoding failed"
    );
    assert_eq!(
        fs::read(&manifest).expect("remove discarded malformed ownership manifest"),
        malformed_manifest,
        "remove changed ownership state after config ownership decoding failed"
    );
}

#[test]
fn update_reinstalls_when_registry_alias_has_no_ownership_manifest() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("agent");
    write_topology(
        &source_root,
        TOPOLOGY,
        "task:\n  eager: true\n",
        &[("RULES.md", b"managed rules\n")],
    );
    let installed = install_source(
        &temp,
        &agent_dir,
        &source_root,
        TOPOLOGY,
        Some("repair"),
        false,
    );
    assert_success(&installed);

    fs::remove_file(manifest_path(&temp)).expect("failed to remove ownership manifest");
    fs::remove_file(agent_dir.join("RULES.md")).expect("failed to remove installed rules");
    fs::remove_file(agent_dir.join("config.yml")).expect("failed to remove installed config");

    let updated = run(&temp, &["update", "--install-id", "repair"]);
    assert_success(&updated);
    assert!(
        manifest_exists(&temp),
        "update reported success without recreating the missing ownership manifest"
    );
    assert_eq!(
        fs::read(agent_dir.join("RULES.md")).expect("update did not restore managed rules"),
        b"managed rules\n"
    );
    assert!(
        String::from_utf8(fs::read(agent_dir.join("config.yml")).unwrap())
            .unwrap()
            .contains("eager: true"),
        "update did not restore the topology config"
    );
    let registry: serde_json::Value = serde_json::from_slice(
        &fs::read(temp.state_home().join("omp-topology/registry.json"))
            .expect("failed to read registry after update"),
    )
    .expect("registry after update was not valid JSON");
    assert_eq!(registry["aliases"]["repair"]["state"], "installed");
}

#[cfg(unix)]
#[test]
fn default_git_branch_uses_commit_from_normal_ls_remote_symref_output() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("agent");
    write_topology(
        &source_root,
        TOPOLOGY,
        "task:\n  eager: true\n",
        &[("RULES.md", b"managed rules\n")],
    );
    let installed = install_source(
        &temp,
        &agent_dir,
        &source_root,
        TOPOLOGY,
        Some("remote"),
        false,
    );
    assert_success(&installed);

    let old_commit = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let current_commit = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let registry_path = temp.state_home().join("omp-topology/registry.json");
    let mut registry: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("failed to read registry"))
            .expect("registry was not valid JSON");
    registry["aliases"]["remote"]["source"] = serde_json::json!({
        "kind": "github",
        "owner": "example",
        "repo": "topologies"
    });
    registry["aliases"]["remote"]["resolved_commit"] = serde_json::json!(old_commit);
    registry["aliases"]["remote"]
        .as_object_mut()
        .expect("registry entry was not an object")
        .remove("overlay_digest");
    fs::write(
        &registry_path,
        serde_json::to_vec(&registry).expect("failed to serialize registry fixture"),
    )
    .expect("failed to seed GitHub registry entry");

    let fake_bin = temp.0.join("bin");
    fs::create_dir_all(&fake_bin).expect("failed to create fake Git directory");
    let fake_git = fake_bin.join("git");
    fs::write(
        &fake_git,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = ls-remote ] && [ \"$2\" = --symref ] && \
             [ \"$3\" = https://github.com/example/topologies.git ] && \
             [ \"$4\" = HEAD ]; then\n\
               printf 'ref: refs/heads/main\\tHEAD\\n%s\\tHEAD\\n' '{current_commit}'\n\
             else\n\
               exit 42\n\
             fi\n"
        ),
    )
    .expect("failed to write fake git executable");
    let mut permissions = fs::metadata(&fake_git)
        .expect("failed to inspect fake git executable")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_git, permissions).expect("failed to make fake git executable");

    let output = command(&temp)
        .env("PATH", &fake_bin)
        .args(["status", "--install-id", "remote", "--json"])
        .output()
        .expect("failed to execute status with fake git");
    let report = json_document(&output);
    assert_eq!(
        report["freshness"]["status"], "update-available",
        "normal symref output did not resolve HEAD's commit: {report}"
    );
    assert_eq!(report["freshness"]["current"], current_commit);
}

#[test]
fn topology_metadata_name_missing_uses_directory_name_for_list_and_install() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let topology_dir = write_topology(
        &source_root,
        TOPOLOGY,
        "task:\n  eager: true\n",
        &[("RULES.md", b"fallback name overlay\n")],
    );
    fs::write(
        topology_dir.join("topology.yml"),
        "version: 1.0.0\ndescription: regression fixture\ninstallable: true\n",
    )
    .expect("failed to remove fixture metadata name");

    let names = listed_topology_names(&temp, &source_root);
    assert_eq!(names, [TOPOLOGY]);
    let agent_dir = temp.0.join("agent");
    let installed = install_source(&temp, &agent_dir, &source_root, &names[0], None, false);
    assert_success(&installed);
    assert_eq!(
        fs::read(agent_dir.join("RULES.md")).expect("fallback overlay was not installed"),
        b"fallback name overlay\n"
    );
}

#[test]
fn topology_metadata_scalar_name_is_listed_and_installable() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let topology_name = "123";
    write_topology(
        &source_root,
        topology_name,
        "task:\n  eager: true\n",
        &[("RULES.md", b"scalar name overlay\n")],
    );

    let names = listed_topology_names(&temp, &source_root);
    assert_eq!(names, [topology_name]);
    let agent_dir = temp.0.join("agent");
    let installed = install_source(&temp, &agent_dir, &source_root, &names[0], None, false);
    assert_success(&installed);
    assert_eq!(
        fs::read(agent_dir.join("RULES.md")).expect("scalar-name overlay was not installed"),
        b"scalar name overlay\n"
    );
}

#[test]
fn topology_metadata_name_list_entry_can_be_selected_for_install() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let directory_name = TOPOLOGY;
    let declared_name = "listed-name";
    let topology_dir = write_topology(
        &source_root,
        directory_name,
        "task:\n  eager: true\n",
        &[("RULES.md", b"mismatched name overlay\n")],
    );
    fs::write(
        topology_dir.join("topology.yml"),
        format!(
            "name: {declared_name}\nversion: 1.0.0\ndescription: regression fixture\ninstallable: true\n"
        ),
    )
    .expect("failed to write mismatched fixture metadata name");

    let names = listed_topology_names(&temp, &source_root);
    assert_eq!(names, [declared_name]);
    let agent_dir = temp.0.join("agent");
    let selected = install_source(&temp, &agent_dir, &source_root, &names[0], None, false);
    assert!(
        selected.status.success(),
        "list advertised topology '{}' but selecting it for install failed:\n{}",
        names[0],
        output_text(&selected)
    );
}

#[cfg(unix)]
#[test]
fn install_rejects_non_utf8_overlay_filename_without_creating_target() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let topology_dir = write_topology(&source_root, TOPOLOGY, "task:\n  eager: true\n", &[]);
    let invalid_name = std::ffi::OsStr::from_bytes(b"bad-\xff-name.md");
    fs::write(
        topology_dir.join("agent").join(invalid_name),
        b"must not be silently aliased\n",
    )
    .expect("failed to write non-UTF-8 overlay fixture");

    let agent_dir = temp.0.join("agent");
    let output = install_source(&temp, &agent_dir, &source_root, TOPOLOGY, None, false);
    assert!(
        !output.status.success(),
        "install accepted a non-UTF-8 overlay filename"
    );
    assert!(
        output_text(&output).contains("non-UTF-8 file name"),
        "install did not clearly identify the non-UTF-8 overlay filename:\n{}",
        output_text(&output)
    );
    assert!(
        !agent_dir.exists(),
        "install mutated the target while rejecting a non-UTF-8 overlay filename"
    );
}

#[cfg(unix)]
#[test]
fn source_discovery_rejects_a_symlinked_topologies_root() {
    let temp = TestDir::new();
    let real_source = temp.0.join("real-source");
    write_topology(
        &real_source,
        TOPOLOGY,
        "task:\n  eager: true\n",
        &[("RULES.md", b"outside source tree\n")],
    );
    let source_root = temp.0.join("source");
    fs::create_dir_all(&source_root).expect("failed to create source root");
    symlink(
        real_source.join("topologies"),
        source_root.join("topologies"),
    )
    .expect("failed to link source topologies root");

    let source = source_root.to_string_lossy().into_owned();
    let output = run(&temp, &["list", "--source", &source, "--json"]);
    assert!(
        !output.status.success(),
        "list followed a symlinked topologies source root"
    );
    assert!(
        output.stdout.is_empty(),
        "failed source discovery emitted partial JSON"
    );
    assert!(
        output_text(&output).contains("symlink"),
        "source error did not identify the unsafe topologies root: {}",
        output_text(&output)
    );
}

#[cfg(unix)]
#[test]
fn install_rejects_a_symlinked_topology_directory_without_touching_target() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let external_source = temp.0.join("external");
    let external_topology = write_topology(
        &external_source,
        TOPOLOGY,
        "task:\n  eager: true\n",
        &[("RULES.md", b"external topology payload\n")],
    );
    let topologies = source_root.join("topologies");
    fs::create_dir_all(&topologies).expect("failed to create source topology root");
    symlink(&external_topology, topologies.join(TOPOLOGY))
        .expect("failed to create topology symlink");
    let agent_dir = temp.0.join("agent");
    let source = source_root.to_string_lossy().into_owned();

    let output = run_for_agent(
        &temp,
        &agent_dir,
        &["install", "-t", TOPOLOGY, "--source", &source],
    );
    assert!(
        !output.status.success(),
        "install followed a symlinked topology directory"
    );
    assert!(
        !agent_dir.exists(),
        "install created a target while rejecting a symlinked topology"
    );
    assert_eq!(
        fs::read(external_topology.join("agent/RULES.md")).unwrap(),
        b"external topology payload\n"
    );
}

#[test]
fn snapshot_retention_is_bounded_across_repeated_install_remove_cycles() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("cycle-agent");
    write_topology(
        &source_root,
        TOPOLOGY,
        "task:\n  eager: true\n",
        &[("RULES.md", b"managed rules\n")],
    );
    fs::create_dir_all(&agent_dir).expect("failed to create cycle target");
    let user_file = agent_dir.join("user-created.md");
    fs::write(&user_file, b"user-owned content\n").expect("failed to seed user file");

    let initial_install = install_source(
        &temp,
        &agent_dir,
        &source_root,
        TOPOLOGY,
        Some("cycles"),
        false,
    );
    assert_success(&initial_install);
    let agent_state = manifest_path(&temp)
        .parent()
        .expect("manifest has no agent state directory")
        .to_path_buf();
    let invalid_snapshots = [
        agent_state.join("backups/install-not-a-timestamp"),
        agent_state.join("snapshots/remove-not-a-timestamp"),
    ];
    for snapshot in &invalid_snapshots {
        fs::create_dir_all(snapshot).expect("failed to create unrelated snapshot directory");
        fs::write(snapshot.join("preserve.txt"), b"user state\n")
            .expect("failed to seed unrelated snapshot directory");
    }
    run_cycles(&temp, &source_root, &agent_dir, "cycles", 7);

    for snapshot in &invalid_snapshots {
        assert!(
            snapshot.is_dir(),
            "pruning removed an unrelated directory with an invalid timestamp: {}",
            snapshot.display()
        );
        assert_eq!(
            fs::read(snapshot.join("preserve.txt")).unwrap(),
            b"user state\n"
        );
    }

    let snapshots = snapshot_transaction_dirs(&temp.state_home().join("omp-topology"));
    assert!(
        !snapshots.is_empty(),
        "lifecycle operations produced no snapshots"
    );
    assert!(
        snapshots.len() <= 10,
        "snapshot retention exceeded its bound: {} transaction directories",
        snapshots.len()
    );
    assert_eq!(
        fs::read(&user_file).unwrap().as_slice(),
        b"user-owned content\n"
    );
    assert!(!agent_dir.join("RULES.md").exists());
    assert!(!agent_dir.join("config.yml").exists());
    assert!(
        !manifest_exists(&temp),
        "completed removal left ownership state behind"
    );
}

#[test]
fn partial_remove_preserves_recovery_marked_snapshot_outside_retention() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("recovery-agent");
    write_topology(
        &source_root,
        TOPOLOGY,
        "task:\n  eager: true\n",
        &[
            ("RULES.md", b"managed rules\n"),
            ("AGENTS.md", b"managed agents\n"),
        ],
    );
    fs::create_dir_all(&agent_dir).expect("failed to create recovery target");
    fs::write(agent_dir.join("config.yml"), b"task:\n  eager: never\n")
        .expect("failed to seed recovery target config");

    // Five install/remove cycles create ten real transactions and fill the
    // combined retention limit. The first of the final two installs mutates
    // the absent topology; the second is a no-op, leaving it installed.
    run_cycles(&temp, &source_root, &agent_dir, "recovery", 5);
    for _ in 0..2 {
        let installed = install_source(
            &temp,
            &agent_dir,
            &source_root,
            TOPOLOGY,
            Some("recovery"),
            false,
        );
        assert_success(&installed);
    }

    let state_root = temp.state_home().join("omp-topology");
    let mut snapshots: Vec<_> = snapshot_transaction_dirs(&state_root).into_iter().collect();
    assert_eq!(
        snapshots.len(),
        10,
        "fixture did not fill the combined snapshot retention limit"
    );
    let timestamp = |snapshot: &Path| {
        let name = snapshot
            .file_name()
            .expect("snapshot has no directory name")
            .to_string_lossy();
        name.strip_prefix("install-")
            .or_else(|| name.strip_prefix("remove-"))
            .expect("snapshot has an unexpected transaction name")
            .to_owned()
    };
    snapshots.sort_by(|left, right| {
        timestamp(left)
            .cmp(&timestamp(right))
            .then_with(|| left.cmp(right))
    });
    let protected_snapshot = snapshots
        .first()
        .expect("fixture produced no transaction snapshots")
        .clone();
    let snapshot_payload = files_under(&protected_snapshot)
        .into_iter()
        .next()
        .expect("oldest real snapshot has no recovery payload");
    let snapshot_payload_bytes =
        fs::read(&snapshot_payload).expect("failed to read oldest snapshot payload");
    let marker = protected_snapshot.join(".recovery-required");
    let marker_bytes = b"retained: recovery required after incomplete rollback\n";
    fs::write(&marker, marker_bytes).expect("failed to mark oldest snapshot for recovery");

    let edited_file = agent_dir.join("RULES.md");
    let edited_file_bytes = b"user-edited rules\n";
    fs::write(&edited_file, edited_file_bytes).expect("failed to edit owned rules file");
    let removed = run_for_agent(&temp, &agent_dir, &["remove", "--topology", TOPOLOGY]);
    assert_success(&removed);
    let report = output_text(&removed);
    assert!(
        report.contains("partially removed")
            && report.contains("preserved (modified after install): RULES.md"),
        "remove did not report preserving the edited owned file:\n{report}"
    );
    assert_eq!(
        fs::read(&edited_file).expect("partial remove deleted the edited owned file"),
        edited_file_bytes,
        "partial remove changed the user-edited owned file"
    );
    assert!(
        !agent_dir.join("AGENTS.md").exists(),
        "partial remove left an unedited owned file behind"
    );
    assert!(
        manifest_exists(&temp),
        "partial remove discarded retry ownership state"
    );

    assert!(
        protected_snapshot.is_dir(),
        "retention removed the recovery-protected snapshot"
    );
    assert_eq!(
        fs::read(&marker).expect("partial remove cleared the recovery marker"),
        marker_bytes,
        "partial remove changed the recovery marker"
    );
    assert_eq!(
        fs::read(&snapshot_payload).expect("protected snapshot payload was deleted"),
        snapshot_payload_bytes,
        "partial remove changed the protected snapshot payload"
    );

    let snapshots_after = snapshot_transaction_dirs(&state_root);
    let marked_count = snapshots_after
        .iter()
        .filter(|snapshot| snapshot.join(".recovery-required").is_file())
        .count();
    let ordinary_count = snapshots_after.len() - marked_count;
    assert_eq!(
        snapshots_after.len(),
        11,
        "unexpected retained snapshot count"
    );
    assert_eq!(marked_count, 1, "unexpected recovery-pinned snapshot count");
    assert_eq!(
        ordinary_count, 10,
        "ordinary snapshots exceeded or fell short of their combined retention bound"
    );
}

#[test]
#[ignore = "opt-in bounded installer soak/scale regression"]
fn ignored_repeated_lifecycle_soak_keeps_state_bounded_and_user_data_intact() {
    let temp = TestDir::new();
    let source_root = temp.0.join("source");
    let agent_dir = temp.0.join("soak-agent");
    write_topology(
        &source_root,
        TOPOLOGY,
        "task:\n  eager: true\n",
        &[("RULES.md", b"managed rules\n")],
    );
    fs::create_dir_all(&agent_dir).expect("failed to create soak target");
    let user_file = agent_dir.join("user-created.md");
    fs::write(&user_file, b"user-owned content\n").expect("failed to seed soak user file");

    run_cycles(&temp, &source_root, &agent_dir, "soak", 30);

    assert!(
        snapshot_transaction_dirs(&temp.state_home().join("omp-topology")).len() <= 10,
        "soak exceeded bounded snapshot retention"
    );
    assert_eq!(
        fs::read(&user_file).unwrap().as_slice(),
        b"user-owned content\n"
    );
    assert!(!agent_dir.join("RULES.md").exists());
    assert!(!manifest_exists(&temp));
}

#[test]
fn large_profile_and_registry_discovery_is_complete_and_stable() {
    const COUNT: usize = 128;

    let temp = TestDir::new();
    let omp_home = temp.0.join("home/.omp");
    let profiles_root = omp_home.join("profiles");
    fs::create_dir_all(&profiles_root).expect("failed to create profiles root");
    let local_source = temp.0.join("fixture-source").display().to_string();
    let mut aliases = serde_json::Map::new();
    let mut expected = BTreeSet::new();

    let registry_entry = |agent_dir: &Path, profile: Option<&str>| {
        let mut entry = serde_json::json!({
            "agent_dir": agent_dir.display().to_string(),
            "topology": TOPOLOGY,
            "source": { "kind": "local", "path": local_source },
            "installed_at": "fixture",
            "state": "installed",
        });
        if let Some(profile) = profile {
            entry["profile"] = serde_json::json!(profile);
        }
        entry
    };

    for index in 0..COUNT {
        let profile_name = format!("profile-{index:03}");
        let alias = profile_name.clone();
        let agent_dir = profiles_root.join(&profile_name).join("agent");
        fs::create_dir_all(&agent_dir).expect("failed to create discovered profile agent");
        let agent_dir = fs::canonicalize(agent_dir).expect("failed to resolve profile agent");
        aliases.insert(
            alias.clone(),
            registry_entry(&agent_dir, Some(&profile_name)),
        );
        expected.insert((profile_name, agent_dir.display().to_string(), alias));

        let alias = format!("custom-{index:03}");
        let agent_dir = temp.0.join(format!("custom-targets/{index:03}/agent"));
        fs::create_dir_all(&agent_dir).expect("failed to create registry-only target");
        let agent_dir = fs::canonicalize(agent_dir).expect("failed to resolve custom target");
        aliases.insert(alias.clone(), registry_entry(&agent_dir, None));
        expected.insert(("".to_string(), agent_dir.display().to_string(), alias));
    }

    let registry = temp.state_home().join("omp-topology/registry.json");
    fs::create_dir_all(registry.parent().unwrap()).expect("failed to create registry directory");
    fs::write(
        &registry,
        serde_json::to_vec_pretty(&serde_json::json!({
            "version": 1,
            "aliases": aliases,
        }))
        .expect("failed to encode fixture registry"),
    )
    .expect("failed to seed large registry");

    let first = json_document(&run(&temp, &["profiles", "list", "--json"]));
    assert_eq!(first["command"], "profiles list");
    let profiles = first["profiles"]
        .as_array()
        .expect("profiles output was not an array");
    assert_eq!(profiles.len(), COUNT * 2);
    let mut observed = BTreeSet::new();
    for profile in profiles {
        assert_eq!(profile["exists"], true);
        let name = profile["name"].as_str().unwrap_or("").to_string();
        let agent_dir = profile["agent_dir"]
            .as_str()
            .expect("profile omitted its agent directory")
            .to_string();
        let managed = profile["managed"]
            .as_array()
            .expect("profile omitted its managed installs");
        assert_eq!(managed.len(), 1);
        assert_eq!(managed[0]["topology"], TOPOLOGY);
        observed.insert((
            name,
            agent_dir,
            managed[0]["install_id"]
                .as_str()
                .expect("managed install omitted its install ID")
                .to_string(),
        ));
    }
    assert_eq!(
        observed, expected,
        "discovery omitted or duplicated entries"
    );

    let second = json_document(&run(&temp, &["profiles", "list", "--json"]));
    assert_eq!(
        first["profiles"], second["profiles"],
        "repeated discovery did not return the same sorted result"
    );
}

#[cfg(unix)]
#[test]
fn registry_removal_keeps_referenced_and_deterministic_bounded_source_caches() {
    const OWNER: &str = "cache-owner";
    const REPO: &str = "cache-repo";
    const KEEP_UNREFERENCED: usize = 10;

    let temp = TestDir::new();
    let cache_root = temp.state_home().join("omp-topology/sources");
    let repo_cache = cache_root.join(OWNER).join(REPO);
    fs::create_dir_all(&repo_cache).expect("failed to create source cache");
    let referenced = format!("{:040x}", 999);
    let unreferenced: Vec<String> = (1..=14).map(|id| format!("{id:040x}")).collect();
    let same_mtime = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);

    for commit in std::iter::once(&referenced).chain(unreferenced.iter()) {
        let checkout = repo_cache.join(commit);
        fs::create_dir_all(&checkout).expect("failed to create cache checkout");
        let marker = checkout.join(".omp-topology-checkout");
        fs::write(&marker, b"complete\n").expect("failed to write checkout marker");
        fs::File::open(&marker)
            .expect("failed to open checkout marker")
            .set_times(std::fs::FileTimes::new().set_modified(same_mtime))
            .expect("failed to set deterministic checkout marker time");
    }
    let staging = repo_cache.join(".staging-test");
    fs::create_dir_all(&staging).expect("failed to create in-progress cache entry");
    fs::write(staging.join(".omp-topology-checkout"), b"incomplete\n")
        .expect("failed to seed in-progress cache entry");
    let invalid = repo_cache.join("not-a-commit");
    fs::create_dir_all(&invalid).expect("failed to create invalid cache entry");
    fs::write(invalid.join(".omp-topology-checkout"), b"not a pin\n")
        .expect("failed to seed invalid cache entry");
    let unrelated_owner = cache_root.join("other-owner").join(REPO).join(&referenced);
    fs::create_dir_all(&unrelated_owner).expect("failed to create unrelated cache");
    fs::write(
        unrelated_owner.join(".omp-topology-checkout"),
        b"unrelated source\n",
    )
    .expect("failed to seed unrelated cache");

    let stale_target = temp.0.join("stale-target");
    let kept_target = temp.0.join("kept-target");
    fs::create_dir_all(&stale_target).expect("failed to create stale target");
    fs::create_dir_all(&kept_target).expect("failed to create retained target");
    let make_entry = |target: &Path, commit: &str| {
        serde_json::json!({
            "agent_dir": target.display().to_string(),
            "topology": TOPOLOGY,
            "source": { "kind": "github", "owner": OWNER, "repo": REPO },
            "resolved_commit": commit,
            "installed_at": "fixture",
            "state": "installed",
        })
    };
    let mut aliases = serde_json::Map::new();
    aliases.insert(
        "stale-cache".to_string(),
        make_entry(&stale_target, unreferenced.last().unwrap()),
    );
    aliases.insert(
        "kept-cache".to_string(),
        make_entry(&kept_target, &referenced),
    );
    let registry = temp.state_home().join("omp-topology/registry.json");
    fs::create_dir_all(registry.parent().unwrap()).expect("failed to create registry directory");
    fs::write(
        &registry,
        serde_json::to_vec_pretty(&serde_json::json!({
            "version": 1,
            "aliases": aliases,
        }))
        .expect("failed to encode fixture registry"),
    )
    .expect("failed to seed cache registry");

    let removed = json_document(&run(
        &temp,
        &["remove", "--install-id", "stale-cache", "--json"],
    ));
    assert_eq!(removed["command"], "remove");

    let expected_unreferenced: BTreeSet<String> = unreferenced
        .iter()
        .take(KEEP_UNREFERENCED)
        .cloned()
        .collect();
    let mut expected_checkouts = expected_unreferenced;
    expected_checkouts.insert(referenced.clone());
    let actual_checkouts: BTreeSet<String> = fs::read_dir(&repo_cache)
        .expect("failed to read pruned source cache")
        .map(|entry| entry.expect("failed to read cache entry"))
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.len() == 40 && name.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .collect();
    assert_eq!(
        actual_checkouts, expected_checkouts,
        "cache pruning did not retain all pins and the deterministic bounded history"
    );
    assert!(
        staging.is_dir(),
        "cache pruning removed an in-progress entry"
    );
    assert!(
        invalid.is_dir(),
        "cache pruning removed an invalid cache entry"
    );
    assert!(
        unrelated_owner.is_dir(),
        "cache pruning touched a different source owner"
    );
    assert_eq!(
        actual_checkouts.len(),
        KEEP_UNREFERENCED + 1,
        "retained entries exceeded the unreferenced bound plus registered pins"
    );
}

#[cfg(unix)]
fn fake_curl_bin(temp: &TestDir) -> (PathBuf, PathBuf) {
    let bin = temp.0.join("fake-bin");
    fs::create_dir_all(&bin).expect("failed to create fake tool directory");
    let curl = bin.join("curl");
    fs::write(
        &curl,
        "#!/bin/sh\nprintf 'called\\n' >> \"$OMP_TOPOLOGY_TEST_CURL_MARKER\"\nexit 97\n",
    )
    .expect("failed to write curl sentinel");
    fs::set_permissions(&curl, fs::Permissions::from_mode(0o700))
        .expect("failed to make curl sentinel executable");
    (bin, temp.0.join("curl-was-called"))
}

#[cfg(unix)]
fn run_with_fake_curl(temp: &TestDir, args: &[&str], bin: &Path, marker: &Path) -> Output {
    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = vec![bin.to_path_buf()];
    path_entries.extend(std::env::split_paths(&inherited_path));
    let path = std::env::join_paths(path_entries).expect("failed to build isolated PATH");
    let mut child = command(temp);
    child
        .env("PATH", path)
        .env("OMP_TOPOLOGY_TEST_CURL_MARKER", marker)
        .args(args)
        .output()
        .expect("failed to execute omp-profile")
}

#[cfg(all(
    unix,
    any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(
            target_os = "macos",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    )
))]
#[test]
fn pinned_self_update_dry_run_returns_plan_without_downloading() {
    let temp = TestDir::new();
    let (fake_bin, marker) = fake_curl_bin(&temp);
    let output = run_with_fake_curl(
        &temp,
        &["self-update", "--version", "v1.2.3", "--dry-run", "--json"],
        &fake_bin,
        &marker,
    );
    let document = json_document(&output);
    assert_eq!(document["command"], "self-update");
    let lines = document["lines"]
        .as_array()
        .expect("self-update output omitted its plan lines");
    assert!(lines.iter().any(|line| {
        line.as_str()
            .is_some_and(|line| line.contains("resolved release v1.2.3"))
    }));
    assert!(lines.iter().any(|line| {
        line.as_str().is_some_and(|line| {
            line.contains("dry-run: would download and verify omp-profile-1.2.3-")
        })
    }));
    assert!(lines.iter().any(|line| {
        line.as_str()
            .is_some_and(|line| line == "dry-run: no files were changed")
    }));
    assert!(!marker.exists(), "pinned self-update dry-run invoked curl");
}

#[cfg(all(
    unix,
    any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(
            target_os = "macos",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    )
))]
#[test]
fn malformed_self_update_versions_fail_before_network_access() {
    let temp = TestDir::new();
    let (fake_bin, marker) = fake_curl_bin(&temp);
    for version in [
        "v01.2.3",
        "1.02.3",
        "1.2.03",
        "1.2",
        "v1.2.3-rc.1",
        "../1.2.3",
    ] {
        let output = run_with_fake_curl(
            &temp,
            &["self-update", "--version", version, "--dry-run", "--json"],
            &fake_bin,
            &marker,
        );
        assert!(
            !output.status.success(),
            "self-update accepted malformed version {version:?}"
        );
        assert!(
            output_text(&output).contains(&format!("invalid version '{version}'")),
            "self-update did not identify malformed version {version:?}: {}",
            output_text(&output)
        );
        assert!(
            output.stdout.is_empty(),
            "failed JSON self-update emitted non-JSON stdout for {version:?}"
        );
        assert!(
            !marker.exists(),
            "self-update attempted network access for malformed version {version:?}"
        );
    }
}
