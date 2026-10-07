//! 现有实时链路的 adapter 组合。
//!
//! 生产入口由 desktop-core 的唯一 Coordinator 持有；本模块只把既有
//! submission/polling/injection adapter 接到 Coordinator 的受控操作上。

use std::sync::Arc;

use seasnail_desktop_core::{
    poll_until_terminal_with_token, CancellationSignal, PollOutcome, PollSleeper,
    SessionStatusGateway, TextDeliveryPort,
};
use seasnail_desktop_core::{
    stable_failure_code, CoordinatorError, CoordinatorRecordingPort as RecordingPort,
    RealtimeCoordinator as RealtimeTaskCoordinator, RealtimeTaskFallback, RealtimeTaskPhase,
};
use seasnail_desktop_core::{
    CapturedAudio, SubmissionGateway, SubmissionWorker, SubmissionWorkerError,
};

#[derive(Debug, Eq, PartialEq)]
pub enum RealtimePipelineError {
    Coordinator(CoordinatorError),
    Submission(String),
    Cancelled,
    Polling(String),
    AutoPaste(String),
}

fn submission_error_code(error: &SubmissionWorkerError) -> String {
    match error {
        SubmissionWorkerError::AlreadySubmitted => "recording_submit_duplicate".into(),
        SubmissionWorkerError::StaleWorker => "recording_stale_worker".into(),
        SubmissionWorkerError::NoAudio => "recording_no_audio".into(),
        SubmissionWorkerError::InvalidSampleRate => "recording_no_audio".into(),
        SubmissionWorkerError::Submit(_) => "recording_submit_failed".into(),
    }
}

pub fn run_after_stop<P, E, G, S, I, F, C>(
    coordinator: &RealtimeTaskCoordinator<P, E>,
    captured: P::Captured,
    to_audio: F,
    gateway: Arc<G>,
    sleeper: &S,
    injector: &I,
    cancelled: &C,
) -> Result<(), RealtimePipelineError>
where
    P: RecordingPort,
    E: seasnail_desktop_core::PresentationSink,
    G: SubmissionGateway + SessionStatusGateway + 'static,
    S: PollSleeper,
    I: TextDeliveryPort,
    F: FnOnce(P::Captured) -> (CapturedAudio, Option<seasnail_desktop_core::ContextPayload>),
    C: CancellationSignal,
{
    let stopped = coordinator.snapshot();
    if stopped.task_id.is_none() {
        return Err(RealtimePipelineError::Coordinator(CoordinatorError::Busy));
    }
    let submit_token = coordinator
        .worker_token(RealtimeTaskPhase::Submitting)
        .map_err(|_| RealtimePipelineError::Submission("recording_stale_worker".into()))?;
    if cancelled.is_cancelled() {
        let _ = coordinator.fail(
            &submit_token,
            "recording_cancelled".into(),
            RealtimeTaskFallback::History,
        );
        return Err(RealtimePipelineError::Cancelled);
    }
    if !coordinator.commit_submission(&submit_token) {
        let error = SubmissionWorkerError::AlreadySubmitted;
        let code = stable_failure_code(&submission_error_code(&error));
        return Err(RealtimePipelineError::Submission(code));
    }
    let worker = SubmissionWorker::new(Arc::clone(&gateway));
    let (audio, context) = to_audio(captured);
    let accepted = worker
        .run_with_token(audio, context, &stopped, &submit_token)
        .map_err(|error| {
            let code = stable_failure_code(&submission_error_code(&error));
            let _ = coordinator.fail(&submit_token, code.clone(), RealtimeTaskFallback::None);
            RealtimePipelineError::Submission(code)
        })?;
    coordinator
        .submission_accepted(&submit_token, accepted.session_id.clone())
        .map_err(RealtimePipelineError::Coordinator)?;

    let transcribing = coordinator.snapshot();
    let mut poll_token = coordinator
        .worker_token(RealtimeTaskPhase::Transcribing)
        .map_err(RealtimePipelineError::Coordinator)?;
    let poll = poll_until_terminal_with_token(
        gateway.as_ref(),
        sleeper,
        &accepted.session_id,
        cancelled,
        &mut poll_token,
        &transcribing,
        |token| coordinator.is_worker_current(token),
        |token, status| coordinator.observe_session_status(token, status).ok(),
    );
    match poll {
        PollOutcome::Cancelled => {
            let _ = coordinator.fail(
                &poll_token,
                "recording_cancelled".into(),
                RealtimeTaskFallback::History,
            );
            Err(RealtimePipelineError::Cancelled)
        }
        PollOutcome::Failed(code) => {
            let fallback = match code.as_str() {
                "recording_transcription_failed" | "no_speech_detected" => {
                    RealtimeTaskFallback::None
                }
                _ => RealtimeTaskFallback::History,
            };
            coordinator
                .fail(&poll_token, code.clone(), fallback)
                .map_err(RealtimePipelineError::Coordinator)?;
            Err(RealtimePipelineError::Polling(code))
        }
        PollOutcome::Completed => {
            if cancelled.is_cancelled() {
                let _ = coordinator.fail(
                    &poll_token,
                    "recording_cancelled".into(),
                    RealtimeTaskFallback::History,
                );
                return Err(RealtimePipelineError::Cancelled);
            }
            let next = coordinator
                .session_completed(&poll_token)
                .map_err(RealtimePipelineError::Coordinator)?;
            if next.phase == RealtimeTaskPhase::Completed {
                return Ok(());
            }
            if cancelled.is_cancelled() {
                let _ = coordinator.fail(
                    &poll_token,
                    "recording_cancelled".into(),
                    RealtimeTaskFallback::History,
                );
                return Err(RealtimePipelineError::Cancelled);
            }
            let auto_paste_token = coordinator
                .worker_token(RealtimeTaskPhase::AutoPasting)
                .map_err(RealtimePipelineError::Coordinator)?;
            let permit = match coordinator.acquire_delivery(&auto_paste_token) {
                Ok(permit) => permit,
                Err(CoordinatorError::Cancelled) => {
                    let _ = coordinator.fail(
                        &auto_paste_token,
                        "recording_cancelled".into(),
                        RealtimeTaskFallback::History,
                    );
                    return Err(RealtimePipelineError::Cancelled);
                }
                Err(error) => return Err(RealtimePipelineError::Coordinator(error)),
            };
            match injector.deliver(&accepted.session_id, permit).failure() {
                None => coordinator
                    .complete(&auto_paste_token)
                    .map_err(RealtimePipelineError::Coordinator),
                Some((code, fallback)) => {
                    coordinator
                        .fail(&auto_paste_token, code.clone(), fallback)
                        .map_err(RealtimePipelineError::Coordinator)?;
                    Err(RealtimePipelineError::AutoPaste(code))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::injection::InjectionOutcome;
    use seasnail_desktop_core::{
        DeliveryOutcome, DeliveryPermit, SessionStatus, SessionStatusGateway, TextDeliveryPort,
    };
    use seasnail_desktop_core::{
        PresentationSink, RealtimeTaskSnapshot, RecordingStartInfo, SubmissionAccepted,
        SubmissionRequest,
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    struct Recording;
    impl RecordingPort for Recording {
        type Captured = Vec<f32>;
        fn start(&self) -> Result<RecordingStartInfo, String> {
            Ok(RecordingStartInfo {
                input_device: "fake".into(),
                sample_rate: 16_000,
            })
        }
        fn stop(&self) -> Result<Self::Captured, String> {
            Ok(vec![0.2; 160])
        }
        fn context_count(_: &Self::Captured) -> usize {
            0
        }
    }

    struct Events;
    impl PresentationSink for Events {
        fn publish(&self, _: RealtimeTaskSnapshot) {}
    }

    struct Gateway {
        submits: Mutex<usize>,
        statuses: Mutex<Vec<SessionStatus>>,
        submit_fails: bool,
    }
    impl SubmissionGateway for Gateway {
        fn submit(
            &self,
            request: SubmissionRequest,
            _: Option<&seasnail_desktop_core::ContextPayload>,
        ) -> Result<SubmissionAccepted, String> {
            if self.submit_fails {
                return Err("gateway detail must not escape".into());
            }
            assert!(request.wav.len() > 44);
            *self.submits.lock().unwrap() += 1;
            Ok(SubmissionAccepted {
                session_id: "session-1".into(),
                status: "transcribing".into(),
            })
        }
    }
    impl SessionStatusGateway for Gateway {
        fn status(&self, _: &str) -> Result<SessionStatus, String> {
            Ok(self.statuses.lock().unwrap().remove(0))
        }
    }

    struct ImmediateSleeper;
    impl PollSleeper for ImmediateSleeper {
        fn sleep(&self, _: Duration, _: &dyn CancellationSignal) -> bool {
            true
        }
    }

    struct Injector {
        outcome: InjectionOutcome,
    }
    impl TextDeliveryPort for Injector {
        fn deliver(&self, _: &str, _permit: DeliveryPermit) -> DeliveryOutcome {
            match &self.outcome {
                InjectionOutcome::Pasted { .. } => DeliveryOutcome::Pasted,
                InjectionOutcome::ClipboardOnly { .. } => DeliveryOutcome::ClipboardOnly,
                InjectionOutcome::Failed {
                    code,
                    clipboard_written,
                    ..
                } => DeliveryOutcome::Failed {
                    code: code.clone(),
                    clipboard_written: *clipboard_written,
                },
            }
        }
    }

    fn audio(pcm: Vec<f32>) -> (CapturedAudio, Option<seasnail_desktop_core::ContextPayload>) {
        (
            CapturedAudio {
                pcm,
                sample_rate: 16_000,
                input_device: "fake".into(),
            },
            None,
        )
    }

    #[test]
    fn full_auto_paste_pipeline_reaches_completed_once() {
        let coordinator = RealtimeTaskCoordinator::new(Arc::new(Recording), Arc::new(Events));
        coordinator.start(true).unwrap();
        let captured = coordinator.stop().unwrap();
        let gateway = Arc::new(Gateway {
            submits: Mutex::new(0),
            statuses: Mutex::new(vec![
                SessionStatus::Transcribing,
                SessionStatus::CleaningUp,
                SessionStatus::Completed,
            ]),
            submit_fails: false,
        });
        let injector = Injector {
            outcome: InjectionOutcome::Pasted {
                method: "fake".into(),
                clipboard_restored: true,
            },
        };
        run_after_stop(
            &coordinator,
            captured,
            audio,
            gateway.clone(),
            &ImmediateSleeper,
            &injector,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(gateway.submits.lock().unwrap().to_owned(), 1);
        assert_eq!(coordinator.snapshot().phase, RealtimeTaskPhase::Completed);
    }

    #[test]
    fn failed_auto_paste_preserves_clipboard_fallback() {
        let coordinator = RealtimeTaskCoordinator::new(Arc::new(Recording), Arc::new(Events));
        coordinator.start(true).unwrap();
        let captured = coordinator.stop().unwrap();
        let gateway = Arc::new(Gateway {
            submits: Mutex::new(0),
            statuses: Mutex::new(vec![SessionStatus::Completed]),
            submit_fails: false,
        });
        let injector = Injector {
            outcome: InjectionOutcome::Failed {
                code: "recording_auto_paste_failed".into(),
                paste_dispatched: true,
                clipboard_written: true,
                clipboard_restored: false,
            },
        };
        assert!(matches!(
            run_after_stop(
                &coordinator,
                captured,
                audio,
                gateway,
                &ImmediateSleeper,
                &injector,
                &AtomicBool::new(false)
            ),
            Err(RealtimePipelineError::AutoPaste(_))
        ));
        let snapshot = coordinator.snapshot();
        assert_eq!(snapshot.phase, RealtimeTaskPhase::Failed);
        assert_eq!(snapshot.fallback, RealtimeTaskFallback::Clipboard);
    }

    #[test]
    fn submission_failure_releases_task_lock_in_failed_terminal_state() {
        let coordinator = RealtimeTaskCoordinator::new(Arc::new(Recording), Arc::new(Events));
        coordinator.start(false).unwrap();
        let captured = coordinator.stop().unwrap();
        let gateway = Arc::new(Gateway {
            submits: Mutex::new(0),
            statuses: Mutex::new(vec![]),
            submit_fails: true,
        });
        let injector = Injector {
            outcome: InjectionOutcome::Pasted {
                method: "unused".into(),
                clipboard_restored: true,
            },
        };
        assert!(matches!(
            run_after_stop(
                &coordinator,
                captured,
                audio,
                gateway,
                &ImmediateSleeper,
                &injector,
                &AtomicBool::new(false)
            ),
            Err(RealtimePipelineError::Submission(_))
        ));
        assert_eq!(coordinator.snapshot().phase, RealtimeTaskPhase::Failed);
        assert!(coordinator.start(false).is_ok());
    }

    #[test]
    fn transcription_failure_does_not_offer_history_recovery() {
        let coordinator = RealtimeTaskCoordinator::new(Arc::new(Recording), Arc::new(Events));
        coordinator.start(false).unwrap();
        let captured = coordinator.stop().unwrap();
        let gateway = Arc::new(Gateway {
            submits: Mutex::new(0),
            statuses: Mutex::new(vec![SessionStatus::Failed(None)]),
            submit_fails: false,
        });
        let injector = Injector {
            outcome: InjectionOutcome::Pasted {
                method: "unused".into(),
                clipboard_restored: true,
            },
        };
        let _ = run_after_stop(
            &coordinator,
            captured,
            audio,
            gateway,
            &ImmediateSleeper,
            &injector,
            &AtomicBool::new(false),
        );
        assert_eq!(coordinator.snapshot().fallback, RealtimeTaskFallback::None);
    }

    #[test]
    fn auto_paste_disabled_completes_without_injecting() {
        let coordinator = RealtimeTaskCoordinator::new(Arc::new(Recording), Arc::new(Events));
        coordinator.start(false).unwrap();
        let captured = coordinator.stop().unwrap();
        let gateway = Arc::new(Gateway {
            submits: Mutex::new(0),
            statuses: Mutex::new(vec![SessionStatus::Completed]),
            submit_fails: false,
        });
        let injector = Injector {
            outcome: InjectionOutcome::Failed {
                code: "must_not_run".into(),
                paste_dispatched: false,
                clipboard_written: false,
                clipboard_restored: false,
            },
        };
        run_after_stop(
            &coordinator,
            captured,
            audio,
            gateway,
            &ImmediateSleeper,
            &injector,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(coordinator.snapshot().phase, RealtimeTaskPhase::Completed);
    }

    struct CountingInjector {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl TextDeliveryPort for CountingInjector {
        fn deliver(&self, _: &str, _permit: DeliveryPermit) -> DeliveryOutcome {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            DeliveryOutcome::Pasted
        }
    }

    #[test]
    fn pipeline_reentry_does_not_resubmit_or_inject_again() {
        let coordinator = RealtimeTaskCoordinator::new(Arc::new(Recording), Arc::new(Events));
        coordinator.start(true).unwrap();
        let captured = coordinator.stop().unwrap();
        let gateway = Arc::new(Gateway {
            submits: Mutex::new(0),
            statuses: Mutex::new(vec![SessionStatus::Completed]),
            submit_fails: false,
        });
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let injector = CountingInjector {
            calls: Arc::clone(&calls),
        };

        run_after_stop(
            &coordinator,
            captured.clone(),
            audio,
            Arc::clone(&gateway),
            &ImmediateSleeper,
            &injector,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            run_after_stop(
                &coordinator,
                captured,
                audio,
                Arc::clone(&gateway),
                &ImmediateSleeper,
                &injector,
                &AtomicBool::new(false),
            ),
            Err(RealtimePipelineError::Submission(
                "recording_stale_worker".into()
            ))
        );
        assert_eq!(*gateway.submits.lock().unwrap(), 1);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn cancellation_after_submission_releases_task_lock() {
        let coordinator = RealtimeTaskCoordinator::new(Arc::new(Recording), Arc::new(Events));
        coordinator.start(false).unwrap();
        let captured = coordinator.stop().unwrap();
        let gateway = Arc::new(Gateway {
            submits: Mutex::new(0),
            statuses: Mutex::new(vec![SessionStatus::Completed]),
            submit_fails: false,
        });
        let injector = Injector {
            outcome: InjectionOutcome::Pasted {
                method: "unused".into(),
                clipboard_restored: true,
            },
        };
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            run_after_stop(
                &coordinator,
                captured,
                audio,
                gateway,
                &ImmediateSleeper,
                &injector,
                &cancelled
            ),
            Err(RealtimePipelineError::Cancelled)
        );
        assert_eq!(coordinator.snapshot().phase, RealtimeTaskPhase::Failed);
        assert!(coordinator.start(false).is_ok());
    }

    struct CancelDuringSubmitGateway {
        cancelled: Arc<AtomicBool>,
        submits: AtomicUsize,
        status_calls: AtomicUsize,
    }

    impl SubmissionGateway for CancelDuringSubmitGateway {
        fn submit(
            &self,
            _: SubmissionRequest,
            _: Option<&seasnail_desktop_core::ContextPayload>,
        ) -> Result<SubmissionAccepted, String> {
            self.submits.fetch_add(1, Ordering::SeqCst);
            self.cancelled.store(true, Ordering::Release);
            Ok(SubmissionAccepted {
                session_id: "session-in-flight".into(),
                status: "transcribing".into(),
            })
        }
    }

    impl SessionStatusGateway for CancelDuringSubmitGateway {
        fn status(&self, _: &str) -> Result<SessionStatus, String> {
            self.status_calls.fetch_add(1, Ordering::SeqCst);
            Ok(SessionStatus::Completed)
        }
    }

    #[test]
    fn cancellation_before_submit_does_not_create_a_session() {
        let coordinator = RealtimeTaskCoordinator::new(Arc::new(Recording), Arc::new(Events));
        coordinator.start(false).unwrap();
        let captured = coordinator.stop().unwrap();
        let gateway = Arc::new(Gateway {
            submits: Mutex::new(0),
            statuses: Mutex::new(vec![]),
            submit_fails: false,
        });
        let result = run_after_stop(
            &coordinator,
            captured,
            audio,
            Arc::clone(&gateway),
            &ImmediateSleeper,
            &Injector {
                outcome: InjectionOutcome::Pasted {
                    method: "unused".into(),
                    clipboard_restored: true,
                },
            },
            &AtomicBool::new(true),
        );
        assert_eq!(result, Err(RealtimePipelineError::Cancelled));
        assert_eq!(*gateway.submits.lock().unwrap(), 0);
        assert_eq!(
            coordinator.snapshot().fallback,
            RealtimeTaskFallback::History
        );
    }

    #[test]
    fn cancellation_during_submit_keeps_accepted_session_out_of_desktop_wait() {
        let coordinator = RealtimeTaskCoordinator::new(Arc::new(Recording), Arc::new(Events));
        coordinator.start(false).unwrap();
        let captured = coordinator.stop().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let gateway = Arc::new(CancelDuringSubmitGateway {
            cancelled: Arc::clone(&cancelled),
            submits: AtomicUsize::new(0),
            status_calls: AtomicUsize::new(0),
        });
        let result = run_after_stop(
            &coordinator,
            captured,
            audio,
            Arc::clone(&gateway),
            &ImmediateSleeper,
            &Injector {
                outcome: InjectionOutcome::Pasted {
                    method: "unused".into(),
                    clipboard_restored: true,
                },
            },
            cancelled.as_ref(),
        );
        assert_eq!(result, Err(RealtimePipelineError::Cancelled));
        assert_eq!(gateway.submits.load(Ordering::SeqCst), 1);
        assert_eq!(gateway.status_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            coordinator.snapshot().fallback,
            RealtimeTaskFallback::History
        );
    }

    struct CancelAfterStatusSleeper;

    impl PollSleeper for CancelAfterStatusSleeper {
        fn sleep(&self, _: Duration, _cancelled: &dyn CancellationSignal) -> bool {
            false
        }
    }

    #[test]
    fn cancellation_after_accepted_202_stops_polling_before_next_request() {
        let coordinator = RealtimeTaskCoordinator::new(Arc::new(Recording), Arc::new(Events));
        coordinator.start(false).unwrap();
        let captured = coordinator.stop().unwrap();
        let gateway = Arc::new(Gateway {
            submits: Mutex::new(0),
            statuses: Mutex::new(vec![SessionStatus::Transcribing]),
            submit_fails: false,
        });
        let cancelled = AtomicBool::new(false);
        let result = run_after_stop(
            &coordinator,
            captured,
            audio,
            Arc::clone(&gateway),
            &CancelAfterStatusSleeper,
            &Injector {
                outcome: InjectionOutcome::Pasted {
                    method: "unused".into(),
                    clipboard_restored: true,
                },
            },
            &cancelled,
        );
        assert_eq!(result, Err(RealtimePipelineError::Cancelled));
        assert_eq!(*gateway.submits.lock().unwrap(), 1);
        assert_eq!(gateway.statuses.lock().unwrap().len(), 0);
        assert_eq!(
            coordinator.snapshot().fallback,
            RealtimeTaskFallback::History
        );
    }

    struct CancelOnCompletedGateway {
        cancelled: Arc<AtomicBool>,
    }

    impl SubmissionGateway for CancelOnCompletedGateway {
        fn submit(
            &self,
            _: SubmissionRequest,
            _: Option<&seasnail_desktop_core::ContextPayload>,
        ) -> Result<SubmissionAccepted, String> {
            Ok(SubmissionAccepted {
                session_id: "session-terminal".into(),
                status: "transcribing".into(),
            })
        }
    }

    impl SessionStatusGateway for CancelOnCompletedGateway {
        fn status(&self, _: &str) -> Result<SessionStatus, String> {
            self.cancelled.store(true, Ordering::Release);
            Ok(SessionStatus::Completed)
        }
    }

    #[test]
    fn cancellation_after_terminal_status_skips_injection() {
        let coordinator = RealtimeTaskCoordinator::new(Arc::new(Recording), Arc::new(Events));
        coordinator.start(true).unwrap();
        let captured = coordinator.stop().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let gateway = Arc::new(CancelOnCompletedGateway {
            cancelled: Arc::clone(&cancelled),
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let injector = CountingInjector {
            calls: Arc::clone(&calls),
        };
        let result = run_after_stop(
            &coordinator,
            captured,
            audio,
            gateway,
            &ImmediateSleeper,
            &injector,
            cancelled.as_ref(),
        );
        assert_eq!(result, Err(RealtimePipelineError::Cancelled));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            coordinator.snapshot().fallback,
            RealtimeTaskFallback::History
        );
    }
}
