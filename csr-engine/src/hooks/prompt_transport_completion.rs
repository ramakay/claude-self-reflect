use super::{AtomicBool, DecisionReason, Ordering, Transport};

/// The watchdog owns the row once it has claimed it. The client's early
/// return must never append a second row or a decision to that timeout.
pub(super) fn completion_line(
    mut line: String,
    transport: Transport,
    reason: Option<DecisionReason>,
    written: &AtomicBool,
) -> Option<String> {
    if written.swap(true, Ordering::SeqCst) {
        return None;
    }
    line.push_str(decision_suffix(transport, reason));
    Some(line)
}

pub(super) fn decision_suffix(
    transport: Transport,
    reason: Option<DecisionReason>,
) -> &'static str {
    if transport != Transport::Socket {
        return "";
    }
    match reason {
        Some(DecisionReason::OnTopic) => " decided=block reason=on_topic",
        Some(DecisionReason::LookalikeOnly) => " decided=abstain reason=lookalike_only",
        Some(DecisionReason::BelowFloor) => " decided=abstain reason=below_floor",
        Some(DecisionReason::ThinPool) => " decided=abstain reason=thin_pool",
        Some(DecisionReason::NoCandidates) => " decided=abstain reason=no_candidates",
        Some(DecisionReason::TooShort) => " decided=abstain reason=too_short",
        Some(DecisionReason::MachinePrompt) => " decided=abstain reason=machine_prompt",
        Some(DecisionReason::KindFiltered) => " decided=abstain reason=kind_filtered",
        Some(DecisionReason::BoilerplateOnly) => " decided=abstain reason=boilerplate_only",
        Some(DecisionReason::KeywordOnly) => " decided=abstain reason=keyword_only",
        Some(DecisionReason::AlreadyDelivered) => " decided=abstain reason=already_delivered",
        Some(DecisionReason::NoSession) => " decided=abstain reason=no_session",
        None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_client_watchdog_early_return_writes_no_completion_or_decision() {
        let written = AtomicBool::new(true);
        let line = "CSR hook prompt-submit [project]: transport=socket prompt_id=real injected=0";
        for reason in [None, Some(DecisionReason::OnTopic)] {
            let mut output = String::new();
            if let Some(line) = completion_line(line.into(), Transport::Socket, reason, &written) {
                output.push_str(&line);
            }
            assert!(output.is_empty());
            assert!(!output.contains("decided="));
        }
        let watchdog = super::super::watchdog_line("project", "real", super::super::PROCESS_BUDGET);
        assert!(!watchdog.contains("decided="));
    }

    #[tokio::test]
    async fn run_client_unlabeled_socket_completion_is_undecided_and_unanswered() {
        use super::super::{
            read_frame, resolve_decision, write_frame, BoundSocket, HookInput, Response, VERSION,
        };
        use tokio::io::AsyncReadExt;

        let dir = tempfile::tempdir().unwrap();
        let bound = BoundSocket::bind(dir.path()).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = bound.listener.accept().await.unwrap();
            read_frame(&mut stream).await.unwrap();
            let response = Response {
                version: VERSION,
                injection: String::new(),
                reason: None,
            };
            write_frame(&mut stream, &serde_json::to_vec(&response).unwrap())
                .await
                .unwrap();
            assert_eq!(stream.read_u8().await.unwrap(), 1);
        });
        let (answer, transport, reason) = resolve_decision(
            &dir.path().join("csr-engine.db"),
            dir.path(),
            HookInput {
                prompt: Some("fix the queue serialization race".into()),
                cwd: Some(dir.path().to_string_lossy().into_owned()),
                session_id: Some("current".into()),
                ..Default::default()
            },
            std::time::Instant::now(),
            |_| true,
        )
        .await;
        assert_eq!(answer.as_deref(), Some(""));
        assert_eq!(transport, Transport::Socket);
        assert_eq!(reason, None);
        server.await.unwrap();
        let written = AtomicBool::new(false);
        let at = chrono::Utc::now();
        let line = format!(
            "{} CSR hook prompt-submit [project]: transport=socket prompt_id=real injected=0",
            at.to_rfc3339()
        );
        let line = completion_line(line, transport, reason, &written).unwrap();
        assert!(!line.contains("decided="));
        let report = crate::telemetry::prompt_delivery::report_at(&line, at);
        let p = &report.last_72h["project"];
        assert_eq!((p.n, p.answered, p.delivered, p.undecided), (1, 0, 0, 1));
        assert!(completion_line(line, Transport::Socket, None, &written).is_none());
    }
}
