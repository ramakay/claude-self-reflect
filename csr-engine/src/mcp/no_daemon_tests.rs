//! A search with the launch agent installed but no daemon running must not
//! wait out the proxy's 20 s backoff, and a daemon that is coming up still
//! gets that wait.

use super::*;
use crate::search::vector_ipc::test_support::seed;
use std::time::{Duration, Instant};

fn text(result: Result<CallToolResult, rmcp::ErrorData>) -> String {
    result
        .unwrap()
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
        .collect()
}

/// A home whose launch agent names `db`, as after an install whose daemon
/// never started. Returns the home and the server over a proxied engine with
/// the production timings.
async fn installed_agent(
    tmp: &tempfile::TempDir,
) -> (
    std::path::PathBuf,
    CsrServer,
    Arc<crate::search::vector_ipc::VectorProxy>,
) {
    let (db, projects) = seed(tmp.path());
    let home = tmp.path().join("home");
    let agents = home.join("Library/LaunchAgents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("com.ramakay.csr-engine.daemon.plist"),
        format!(
            "<?xml version=\"1.0\"?>\n<plist version=\"1.0\"><dict>\
             <key>ProgramArguments</key><array><string>/bin/csr-engine</string>\
             <string>daemon</string><string>--db-path</string><string>{}</string>\
             </array></dict></plist>",
            db.display()
        ),
    )
    .unwrap();
    let engine = crate::engine::Engine::new_proxied_with_home(&db, &projects, Some(&home))
        .unwrap()
        .expect("the launch agent configures a daemon");
    let proxy = engine.search().read().await.proxy().unwrap().clone();
    let server = CsrServer::new(
        engine.storage().clone(),
        engine.embeddings().clone(),
        engine.search().clone(),
        projects,
        tmp.path().join("index-v3"),
        None,
        None,
    );
    (home, server, proxy)
}

fn reflect() -> ReflectOnPastParams {
    ReflectOnPastParams {
        query: "socket handshake".into(),
        limit: Some(5),
        min_score: Some(0.0),
        project: Some("all".into()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_daemon_process_answers_at_once_and_says_so() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (_home, server, _proxy) = installed_agent(&tmp).await;
    let started = Instant::now();
    let answer = text(server.reflect_on_past(Parameters(reflect())).await);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the search waited out the daemon backoff"
    );
    assert!(
        answer.contains("the daemon is not running"),
        "the answer does not say the daemon is not running"
    );
    assert!(
        !answer.contains("restarting"),
        "the answer claims a restart"
    );
    assert!(
        answer.contains("daemon lock and socket handshake"),
        "the keyword results are missing"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_daemon_holding_its_lock_is_still_waited_for() {
    use fs2::FileExt;
    let tmp = tempfile::TempDir::new().unwrap();
    let (home, server, proxy) = installed_agent(&tmp).await;
    proxy.set_timings(crate::search::vector_ipc::ProxyTimings {
        backoff_first: Duration::from_millis(20),
        backoff_total: Duration::from_millis(600),
        down_window: Duration::from_secs(30),
    });
    // The daemon takes its lock before it loads the index, so a lock holder
    // with no socket yet is a daemon that is starting.
    let data = home.join(".claude-self-reflect");
    std::fs::create_dir_all(&data).unwrap();
    let lock = std::fs::File::create(data.join("daemon.lock")).unwrap();
    lock.try_lock_exclusive().unwrap();
    let started = Instant::now();
    let answer = text(server.reflect_on_past(Parameters(reflect())).await);
    assert!(
        started.elapsed() >= Duration::from_millis(500),
        "gave up before the backoff ran out"
    );
    assert!(
        answer.contains("daemon restarting"),
        "the answer does not say the daemon is restarting"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_socket_file_is_still_waited_for() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (_home, server, proxy) = installed_agent(&tmp).await;
    proxy.set_timings(crate::search::vector_ipc::ProxyTimings {
        backoff_first: Duration::from_millis(20),
        backoff_total: Duration::from_millis(600),
        down_window: Duration::from_secs(30),
    });
    let socket = crate::search::vector_ipc::socket_path(tmp.path());
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    std::fs::File::create(&socket).unwrap();
    let started = Instant::now();
    let answer = text(server.reflect_on_past(Parameters(reflect())).await);
    assert!(
        started.elapsed() >= Duration::from_millis(500),
        "gave up before the backoff ran out"
    );
    assert!(
        answer.contains("daemon restarting"),
        "the answer does not say the daemon is restarting"
    );
}
