//! Exercise the real CLI supervisor with a controlled runtime protocol, without
//! PATH/environment mutation or a Docker socket. Live acceptance is separate.

use super::*;
use std::os::unix::fs::PermissionsExt;

pub(crate) struct RuntimeFixture {
    pub(crate) dir: tempfile::TempDir,
    pub(crate) runner: CliRunner,
}

impl RuntimeFixture {
    pub(crate) fn new(runtime: ContainerRuntime) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join(runtime.binary());
        let script = r#"#!/usr/bin/env python3
import json, pathlib, sys, time
root = pathlib.Path(__file__).parent
args = sys.argv[1:]
if args[0] == 'run':
    name = args[args.index('--name') + 1]
    (root / name).touch()
    (root / 'started').write_text(name)
    time.sleep(60)
elif args[0] == 'rm':
    if (root / 'fail-cleanup').exists():
        sys.exit(1)
    (root / args[-1]).unlink(missing_ok=True)
elif args[0] == 'ps':
    for path in root.glob('lunarwing-sandbox-*'):
        print(json.dumps({'Names': path.name,
            'Labels': 'lunarwing.sandbox=2000-01-01T00:00:00Z'}))
elif args[0] == 'info':
    print('["name=seccomp,profile=builtin"]')
else:
    sys.exit(1)
"#;
        std::fs::write(&binary, script).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            dir,
            runner: CliRunner {
                runtime,
                keep_id: KeepIdMode::UidPinned,
                image: "fixture".into(),
                binary,
            },
        }
    }

    pub(crate) async fn started(&self) -> String {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(name) = std::fs::read_to_string(self.dir.path().join("started"))
                    && !name.is_empty()
                {
                    return name;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }

    pub(crate) async fn removed(&self, name: &str) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while self.dir.path().join(name).exists()
                || inflight_sandbox_containers().iter().any(|n| n == name)
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("container removed and in-flight registration released");
    }

    fn execution(&self, timeout: Duration) -> tokio::task::JoinHandle<Result<ContainerOutput>> {
        let runner = self.runner.clone();
        let workspace = self.dir.path().to_owned();
        tokio::spawn(async move {
            runner
                .execute(ExecParams {
                    command: "sleep 60",
                    working_dir: &workspace,
                    policy: SandboxPolicy::ReadOnly,
                    limits: &ResourceLimits {
                        timeout,
                        ..ResourceLimits::default()
                    },
                    env: &HashMap::new(),
                    network: SandboxNetworkMode::None,
                    proxy_port: 0,
                })
                .await
        })
    }
}

#[tokio::test]
async fn cancellation_cleans_up_and_releases_inflight_on_both_runtimes() {
    for runtime in [ContainerRuntime::Podman, ContainerRuntime::Docker] {
        let fixture = RuntimeFixture::new(runtime);
        let execution = fixture.execution(Duration::from_secs(60));
        let name = fixture.started().await;
        assert!(inflight_sandbox_containers().contains(&name));
        execution.abort();
        assert!(execution.await.unwrap_err().is_cancelled());
        fixture.removed(&name).await;
    }
}

#[tokio::test]
async fn timeout_cleans_up_container_not_just_cli() {
    let fixture = RuntimeFixture::new(ContainerRuntime::Podman);
    let execution = fixture.execution(Duration::from_secs(2));
    let name = fixture.started().await;
    let error = execution.await.unwrap().unwrap_err();
    assert_eq!(error.kind(), "timeout");
    fixture.removed(&name).await;
}

#[tokio::test]
async fn socketless_reaper_uses_selected_runtime_and_skips_active_execution() {
    let fixture = RuntimeFixture::new(ContainerRuntime::Docker);
    let execution = fixture.execution(Duration::from_secs(60));
    let active = fixture.started().await;
    let orphan = "lunarwing-sandbox-abandoned";
    std::fs::write(fixture.dir.path().join(orphan), "").unwrap();
    fixture.runner.reap_orphans(Duration::ZERO).await;
    assert!(
        fixture.dir.path().join(&active).exists(),
        "live container must survive sweep"
    );
    assert!(inflight_sandbox_containers().contains(&active));
    assert!(
        !fixture.dir.path().join(orphan).exists(),
        "old orphan must be removed"
    );
    execution.abort();
    let _ = execution.await;
    fixture.removed(&active).await;
}

#[tokio::test]
async fn failed_cleanup_leaves_orphan_eligible_for_next_reaper_sweep() {
    let fixture = RuntimeFixture::new(ContainerRuntime::Podman);
    let block = fixture.dir.path().join("fail-cleanup");
    std::fs::write(&block, "").unwrap();
    let execution = fixture.execution(Duration::from_secs(2));
    let name = fixture.started().await;
    assert_eq!(execution.await.unwrap().unwrap_err().kind(), "timeout");
    assert!(
        fixture.dir.path().join(&name).exists(),
        "fixture rejected removal"
    );
    assert!(
        !inflight_sandbox_containers().contains(&name),
        "orphan must not retain immunity"
    );
    std::fs::remove_file(block).unwrap();
    fixture.runner.reap_orphans(Duration::ZERO).await;
    fixture.removed(&name).await;
}
