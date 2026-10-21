//! Вспомогательный бинарник теста «родитель убит» (`tests/supervisor.rs`).
//!
//! Аргументы: `<data_dir> <qdrant_binary> <http_port> <grpc_port>`. Запускает супервизор,
//! ждёт `Ready`, печатает `QDRANT_PID=<pid>` и ждёт, пока тест не убьёт процесс жёстко
//! (`TerminateProcess` / SIGKILL). Qdrant должен умереть вместе с ним: Job Object с
//! `KILL_ON_JOB_CLOSE` на Windows, `PR_SET_PDEATHSIG` на Linux.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use zam_index::supervisor::{ManagedConfig, QdrantState, Supervisor, SupervisorConfig};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    assert_eq!(
        args.len(),
        4,
        "использование: kill_parent_probe <data_dir> <qdrant> <http_port> <grpc_port>"
    );
    let data_dir = PathBuf::from(&args[0]);
    let mut cfg = ManagedConfig::new(PathBuf::from(&args[1]));
    cfg.http_port = args[2].parse().expect("http_port");
    cfg.grpc_port = args[3].parse().expect("grpc_port");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async move {
        let sup = Supervisor::start(SupervisorConfig::Managed(cfg), &data_dir)
            .await
            .expect("старт супервизора");
        let mut rx = sup.state();
        let ready = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if matches!(*rx.borrow_and_update(), QdrantState::Ready { .. }) {
                    return true;
                }
                if rx.changed().await.is_err() {
                    return false;
                }
            }
        })
        .await;
        assert!(
            matches!(ready, Ok(true)),
            "Qdrant не готов: {:?}",
            *rx.borrow()
        );
        let pid = sup.pid().expect("pid Qdrant");
        println!("QDRANT_PID={pid}");
        let _ = std::io::stdout().flush();
        // Ждём жёсткого завершения извне; супервизор не останавливается штатно.
        let _keep = &sup;
        std::future::pending::<()>().await;
    });
}
