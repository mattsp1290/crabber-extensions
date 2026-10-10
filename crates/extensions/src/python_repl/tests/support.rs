use std::{path::PathBuf, process::Command};

pub(super) fn python() -> Option<PathBuf> {
    let found = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|path| {
            let candidate = path.join("python3");
            Command::new(&candidate)
                .args(["-c", "import sys; print(sys.executable)"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| {
                    PathBuf::from(String::from_utf8(o.stdout).ok()?.trim())
                        .canonicalize()
                        .ok()
                })
        })
    });
    let strict = std::env::var("PYTHON_REPL_REQUIRE_PYTHON").as_deref() == Ok("1");
    if let Some(path) = &found {
        if strict {
            assert!(
                Command::new(path)
                    .args([
                        "-c",
                        "import sys; assert (3,11) <= sys.version_info[:2] <= (3,14)"
                    ])
                    .status()
                    .unwrap()
                    .success(),
                "required Python version outside 3.11–3.14"
            );
        }
    } else {
        assert!(!strict, "required python3 absent");
        eprintln!("python_repl: python3 absent, skipping");
    }
    found
}

pub(super) fn group_members(id: i32) -> Vec<(i32, String)> {
    let output = Command::new("ps")
        .args(["-A", "-o", "pid=,pgid=,stat="])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let pgid: i32 = fields.next()?.parse().ok()?;
            let state = fields.next()?.to_owned();
            (pgid == id).then_some((pid, state))
        })
        .collect()
}

pub(super) async fn group_gone(id: i32) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !group_members(id).is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("runner process group survived cleanup");
}

pub(super) fn options(directory: &std::path::Path) -> Option<crate::python_repl::Options> {
    use crate::python_repl::*;
    use std::{collections::BTreeMap, time::Duration};
    Some(Options {
        python_path: python()?,
        python_identity: "test-python-v1".into(),
        temp_root: directory.into(),
        environment: Environment {
            mode: EnvironmentMode::ExplicitOnly,
            overrides: BTreeMap::new(),
            identity: "test-env-v1".into(),
        },
        limits: Limits {
            max_sessions: 4,
            max_queued_per_session: 2,
            max_code_bytes: 4096,
            max_output_bytes_per_stream: 4096,
            max_result_bytes: 4096,
            max_exception_bytes: 8192,
            max_environment_entries: 1024,
            max_environment_bytes: 256 * 1024,
            default_timeout: Duration::from_secs(10),
            max_timeout: Duration::from_secs(30),
            runner_start_timeout: Duration::from_secs(10),
            terminate_grace: Duration::from_millis(300),
            kill_wait: Duration::from_secs(3),
            shutdown_grace: Duration::from_secs(5),
        },
    })
}
