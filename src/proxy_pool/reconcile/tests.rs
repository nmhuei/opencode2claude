#[cfg(test)]
mod verification_tests {
    use crate::proxy_pool::reconcile::verification::{
        verify_candidate, ProxyVerifier, VerificationFailure,
    };
    use crate::proxy_pool::ExitIdentity;
    #[allow(unused_imports)]
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[derive(Debug)]
    struct FakeVerifier {
        transport_error: Option<String>,
        identity_error: Option<String>,
        route_error: Option<String>,
        delay: Duration,
        transport_calls: Arc<AtomicUsize>,
        identity_calls: Arc<AtomicUsize>,
        route_calls: Arc<AtomicUsize>,
    }

    impl FakeVerifier {
        fn success() -> Self {
            Self {
                transport_error: None,
                identity_error: None,
                route_error: None,
                delay: Duration::ZERO,
                transport_calls: Arc::new(AtomicUsize::new(0)),
                identity_calls: Arc::new(AtomicUsize::new(0)),
                route_calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn identity_error(message: &str) -> Self {
            let mut verifier = Self::success();
            verifier.identity_error = Some(message.to_string());
            verifier
        }

        fn transport_error(message: &str) -> Self {
            let mut verifier = Self::success();
            verifier.transport_error = Some(message.to_string());
            verifier
        }

        fn route_error(message: &str) -> Self {
            let mut verifier = Self::success();
            verifier.route_error = Some(message.to_string());
            verifier
        }

        fn with_delay(delay: Duration) -> Self {
            let mut verifier = Self::success();
            verifier.delay = delay;
            verifier
        }

        fn identity_calls(&self) -> usize {
            self.identity_calls.load(Ordering::Relaxed)
        }

        fn route_calls(&self) -> usize {
            self.route_calls.load(Ordering::Relaxed)
        }
    }

    #[async_trait]
    impl ProxyVerifier for FakeVerifier {
        async fn verify_transport(
            &self,
            _client: &reqwest::Client,
            _timeout: Duration,
        ) -> Result<(), String> {
            self.transport_calls.fetch_add(1, Ordering::Relaxed);
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            self.transport_error.clone().map_or(Ok(()), Err)
        }

        async fn verify_identity(
            &self,
            _client: &reqwest::Client,
            _endpoints: &[String],
            _timeout: Duration,
        ) -> Result<ExitIdentity, String> {
            self.identity_calls.fetch_add(1, Ordering::Relaxed);
            self.identity_error.clone().map_or_else(
                || {
                    Ok(ExitIdentity {
                        public_ip: "203.0.113.10".to_string(),
                        provider: Some("test".to_string()),
                        colo: Some("TST".to_string()),
                        verified_at_unix_secs: 1,
                    })
                },
                Err,
            )
        }

        async fn verify_route(
            &self,
            _client: &reqwest::Client,
            _upstream_base_url: &str,
            _timeout: Duration,
        ) -> Result<(), String> {
            self.route_calls.fetch_add(1, Ordering::Relaxed);
            self.route_error.clone().map_or(Ok(()), Err)
        }
    }

    fn test_client() -> reqwest::Client {
        reqwest::Client::new()
    }

    fn endpoints() -> Vec<String> {
        vec!["https://identity.invalid".to_string()]
    }

    #[tokio::test]
    async fn staged_verification_never_reaches_route_after_identity_failure() {
        let verifier = FakeVerifier::identity_error("warp=off");
        let result = verify_candidate(
            &verifier,
            &test_client(),
            &endpoints(),
            "https://upstream.invalid/v1",
            Duration::from_millis(50),
        )
        .await;
        assert!(matches!(result, Err(VerificationFailure::Identity(_))));
        assert_eq!(verifier.route_calls(), 0);
    }

    #[tokio::test]
    async fn transport_failure_short_circuits_identity_and_route() {
        let verifier = FakeVerifier::transport_error("socks dead");
        let result = verify_candidate(
            &verifier,
            &test_client(),
            &endpoints(),
            "https://upstream.invalid/v1",
            Duration::from_millis(50),
        )
        .await;
        assert!(matches!(result, Err(VerificationFailure::Transport(_))));
        assert_eq!(verifier.identity_calls(), 0);
        assert_eq!(verifier.route_calls(), 0);
    }

    #[tokio::test]
    async fn route_failure_is_reported_after_identity_passes() {
        let verifier = FakeVerifier::route_error("tls failed");
        let result = verify_candidate(
            &verifier,
            &test_client(),
            &endpoints(),
            "https://upstream.invalid/v1",
            Duration::from_millis(50),
        )
        .await;
        assert!(matches!(result, Err(VerificationFailure::Route(_))));
        assert_eq!(verifier.identity_calls(), 1);
        assert_eq!(verifier.route_calls(), 1);
    }

    #[tokio::test]
    async fn stage_timeout_is_bounded() {
        let verifier = FakeVerifier::with_delay(Duration::from_secs(60));
        let result = verify_candidate(
            &verifier,
            &test_client(),
            &endpoints(),
            "https://upstream.invalid/v1",
            Duration::from_millis(10),
        )
        .await;
        assert!(
            matches!(result, Err(VerificationFailure::Transport(message)) if message.contains("10ms"))
        );
    }

    #[tokio::test]
    async fn staged_verification_full_pass_returns_identity() {
        let verifier = FakeVerifier::success();
        let identity = verify_candidate(
            &verifier,
            &test_client(),
            &endpoints(),
            "https://upstream.invalid/v1",
            Duration::from_millis(50),
        )
        .await
        .expect("verification");
        assert_eq!(identity.public_ip, "203.0.113.10");
        assert_eq!(verifier.route_calls(), 1);
    }
}

#[cfg(test)]
mod reconciler_tests {
    use super::super::*;
    use crate::config::{BridgeConfig, EgressMode};
    use crate::docker::{
        ContainerRuntime, ContainerState, ContainerSummary, DockerError, DockerResult, ProxySpec,
    };
    use crate::observability::Metrics;
    use crate::proxy_pool::ExitIdentity;
    use crate::proxy_pool::{ProxyPool, ProxySubsystemPhase, ProxySubsystemStatus};
    use crate::workers::WorkerRegistry;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::sync::RwLock;

    #[derive(Debug)]
    struct TestRuntime {
        inspect_delay: Duration,
        inspect_error: bool,
    }

    #[async_trait]
    impl ContainerRuntime for TestRuntime {
        async fn daemon_version(&self) -> DockerResult<String> {
            Ok("test".to_string())
        }

        async fn inspect(&self, _spec: &ProxySpec) -> DockerResult<ContainerState> {
            if !self.inspect_delay.is_zero() {
                tokio::time::sleep(self.inspect_delay).await;
            }
            if self.inspect_error {
                return Err(DockerError::CommandFailed("docker unavailable".to_string()));
            }
            Ok(ContainerState {
                exists: true,
                running: true,
                has_expected_volume: true,
            })
        }

        async fn create_missing(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Ok(())
        }

        async fn recreate_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Err(DockerError::CommandFailed(
                "reconciler must not recreate".to_string(),
            ))
        }

        async fn remove_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Err(DockerError::CommandFailed(
                "reconciler must not remove".to_string(),
            ))
        }

        async fn restart_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Err(DockerError::CommandFailed(
                "reconciler must not restart".to_string(),
            ))
        }

        async fn stop_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Err(DockerError::CommandFailed(
                "reconciler must not stop".to_string(),
            ))
        }

        async fn start_managed(&self, _spec: &ProxySpec) -> DockerResult<()> {
            Ok(())
        }

        async fn logs(&self, _spec: &ProxySpec, _tail: usize) -> DockerResult<String> {
            Ok(String::new())
        }

        async fn list(&self, _specs: &[ProxySpec]) -> DockerResult<Vec<ContainerSummary>> {
            Ok(Vec::new())
        }
    }

    #[derive(Debug, Default)]
    struct AlwaysVerifier;

    #[async_trait]
    impl ProxyVerifier for AlwaysVerifier {
        async fn verify_transport(
            &self,
            _client: &reqwest::Client,
            _timeout: Duration,
        ) -> Result<(), String> {
            Ok(())
        }

        async fn verify_identity(
            &self,
            _client: &reqwest::Client,
            _endpoints: &[String],
            _timeout: Duration,
        ) -> Result<ExitIdentity, String> {
            Ok(ExitIdentity {
                public_ip: "203.0.113.10".to_string(),
                provider: Some("test".to_string()),
                colo: Some("TST".to_string()),
                verified_at_unix_secs: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            })
        }

        async fn verify_route(
            &self,
            _client: &reqwest::Client,
            _upstream_base_url: &str,
            _timeout: Duration,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    fn hybrid_config() -> Arc<BridgeConfig> {
        let mut config = BridgeConfig::default();
        config.egress.mode = EgressMode::Hybrid;
        config.primary_proxies = Some(vec!["socks5h://127.0.0.1:40001".to_string()]);
        config.warm_standby_proxies = Some(vec!["socks5h://127.0.0.1:40004".to_string()]);
        config.egress.active_proxy_count = 1;
        config.egress.verify_timeout = Duration::from_millis(50);
        config.egress.bootstrap_timeout = Duration::from_secs(30);
        config.egress.recovery_backoff_max = Duration::from_secs(120);
        Arc::new(config)
    }

    fn pool(config: &BridgeConfig) -> Arc<RwLock<ProxyPool>> {
        let urls = config
            .primary_proxies
            .iter()
            .flatten()
            .chain(config.warm_standby_proxies.iter().flatten())
            .cloned()
            .collect::<Vec<_>>();
        Arc::new(RwLock::new(ProxyPool::new_with_egress_policy(
            &urls,
            config.egress.active_proxy_count,
            config.egress.require_verified_exit_ip,
            config.egress.identity_ttl,
        )))
    }

    #[tokio::test]
    async fn slow_docker_reconcile_is_cancelled_without_waiting_for_inspect() {
        let config = hybrid_config();
        let pool = pool(&config);
        let subsystem = Arc::new(RwLock::new(ProxySubsystemStatus::starting()));
        let runtime: Arc<dyn ContainerRuntime> = Arc::new(TestRuntime {
            inspect_delay: Duration::from_secs(60),
            inspect_error: false,
        });
        let verifier: Arc<dyn ProxyVerifier> = Arc::new(AlwaysVerifier);
        let metrics = Arc::new(Metrics::default());
        let registry = WorkerRegistry::new();
        let task_pool = pool.clone();
        let task_subsystem = subsystem.clone();
        let task_config = config.clone();
        registry.spawn_critical("test-reconcile", move |context| async move {
            hybrid_proxy_reconciler(
                task_pool,
                task_subsystem,
                runtime,
                verifier,
                task_config,
                metrics,
                context,
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        registry
            .shutdown(Duration::from_millis(250))
            .await
            .expect("cancellation must interrupt slow Docker inspect");
    }

    #[tokio::test]
    async fn docker_unavailable_marks_degraded_and_keeps_worker_alive() {
        let config = hybrid_config();
        let pool = pool(&config);
        let subsystem = Arc::new(RwLock::new(ProxySubsystemStatus::starting()));
        let runtime: Arc<dyn ContainerRuntime> = Arc::new(TestRuntime {
            inspect_delay: Duration::ZERO,
            inspect_error: true,
        });
        let verifier: Arc<dyn ProxyVerifier> = Arc::new(AlwaysVerifier);
        let metrics = Arc::new(Metrics::default());
        let registry = WorkerRegistry::new();
        let task_pool = pool.clone();
        let task_subsystem = subsystem.clone();
        let task_config = config.clone();
        registry.spawn_critical("test-reconcile", move |context| async move {
            hybrid_proxy_reconciler(
                task_pool,
                task_subsystem,
                runtime,
                verifier,
                task_config,
                metrics,
                context,
            )
            .await
        });

        for _ in 0..50 {
            if subsystem.read().await.snapshot().phase == ProxySubsystemPhase::Degraded {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let snapshot = subsystem.read().await.snapshot();
        assert_eq!(snapshot.phase, ProxySubsystemPhase::Degraded);
        assert!(snapshot.backoff_until_unix_secs.is_some());
        assert!(snapshot
            .last_error
            .as_deref()
            .is_some_and(|message| message.contains("docker unavailable")));

        registry
            .shutdown(Duration::from_millis(250))
            .await
            .expect("worker remains cancellable while backing off");
    }

    #[test]
    fn recovery_backoff_is_bounded_and_nonzero() {
        let max = Duration::from_secs(120);
        assert_eq!(recovery_backoff(0, max, 0), Duration::from_secs(2));
        assert_eq!(recovery_backoff(1, max, 0), Duration::from_secs(5));
        assert_eq!(recovery_backoff(2, max, 0), Duration::from_secs(10));
        assert_eq!(recovery_backoff(3, max, 0), Duration::from_secs(30));
        assert_eq!(recovery_backoff(4, max, 0), Duration::from_secs(60));
        assert_eq!(
            recovery_backoff(99, Duration::from_secs(45), 0),
            Duration::from_secs(45)
        );
    }

    #[tokio::test]
    async fn full_reconcile_cycle_marks_subsystem_ready_without_destructive_lifecycle() {
        let config = hybrid_config();
        let pool = pool(&config);
        let subsystem = Arc::new(RwLock::new(ProxySubsystemStatus::starting()));
        let runtime = TestRuntime {
            inspect_delay: Duration::ZERO,
            inspect_error: false,
        };
        let verifier = AlwaysVerifier;
        let metrics = Metrics::default();

        reconcile_once(&pool, &subsystem, &runtime, &verifier, &config, &metrics)
            .await
            .expect("full verification cycle");

        assert_eq!(
            subsystem.read().await.snapshot().phase,
            ProxySubsystemPhase::Ready
        );
        assert!(subsystem.read().await.is_ready());
    }

    #[tokio::test]
    async fn reconcile_cycle_is_mode_independent_for_pure_proxy_egress() {
        // The reconciler body must drive the identical lifecycle in pure
        // proxy mode (state.rs now spawns it there too) with zero changes.
        let mut raw = (*hybrid_config()).clone();
        raw.egress.mode = EgressMode::Proxy;
        let config = Arc::new(raw);
        let pool = pool(&config);
        let subsystem = Arc::new(RwLock::new(ProxySubsystemStatus::starting()));
        let runtime = TestRuntime {
            inspect_delay: Duration::ZERO,
            inspect_error: false,
        };
        let verifier = AlwaysVerifier;
        let metrics = Metrics::default();

        reconcile_once(&pool, &subsystem, &runtime, &verifier, &config, &metrics)
            .await
            .expect("pure proxy verification cycle");

        assert_eq!(
            subsystem.read().await.snapshot().phase,
            ProxySubsystemPhase::Ready
        );
    }

    #[test]
    fn successful_cycle_resets_backoff_to_first_step() {
        let attempt = failure_attempt_after_cycle(4, true);
        assert_eq!(attempt, 0);
        assert_eq!(
            recovery_backoff(attempt, Duration::from_secs(120), 0),
            Duration::from_secs(2)
        );
        assert_eq!(failure_attempt_after_cycle(0, false), 1);
    }

    /// Verifier handing every candidate a distinct fresh exit identity so the
    /// duplicate suppression keeps all candidates eligible for route probing.
    #[derive(Debug)]
    struct DistinctIdentityVerifier {
        identity_calls: Arc<AtomicUsize>,
        route_error: Option<String>,
    }

    impl DistinctIdentityVerifier {
        fn route_failure() -> Self {
            Self {
                identity_calls: Arc::new(AtomicUsize::new(0)),
                route_error: Some("upstream route probe failed".to_string()),
            }
        }
    }

    #[async_trait]
    impl ProxyVerifier for DistinctIdentityVerifier {
        async fn verify_transport(
            &self,
            _client: &reqwest::Client,
            _timeout: Duration,
        ) -> Result<(), String> {
            Ok(())
        }

        async fn verify_identity(
            &self,
            _client: &reqwest::Client,
            _endpoints: &[String],
            _timeout: Duration,
        ) -> Result<ExitIdentity, String> {
            let sequence = self.identity_calls.fetch_add(1, Ordering::SeqCst);
            Ok(ExitIdentity {
                public_ip: format!("192.0.2.{}", sequence + 1),
                provider: Some("test".to_string()),
                colo: Some("TST".to_string()),
                verified_at_unix_secs: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            })
        }

        async fn verify_route(
            &self,
            _client: &reqwest::Client,
            _upstream_base_url: &str,
            _timeout: Duration,
        ) -> Result<(), String> {
            match &self.route_error {
                Some(error) => Err(error.clone()),
                None => Ok(()),
            }
        }
    }

    #[tokio::test]
    async fn successful_reconcile_cycle_counts_bootstrap_and_transitions_exactly_once() {
        let config = hybrid_config();
        let pool = pool(&config);
        let subsystem = Arc::new(RwLock::new(ProxySubsystemStatus::starting()));
        let runtime = TestRuntime {
            inspect_delay: Duration::ZERO,
            inspect_error: false,
        };
        let verifier = AlwaysVerifier;
        let metrics = Metrics::default();

        reconcile_once(&pool, &subsystem, &runtime, &verifier, &config, &metrics)
            .await
            .expect("full verification cycle");

        let snapshot = metrics.snapshot();
        assert_eq!(
            snapshot.proxy_bootstrap_attempts, 2,
            "one bootstrap attempt per configured candidate"
        );
        assert_eq!(snapshot.proxy_bootstrap_successes, 2);
        assert_eq!(snapshot.proxy_bootstrap_failures, 0);
        assert_eq!(
            snapshot.proxy_state_transitions, 5,
            "Starting, TransportVerifying, IdentityVerifying, RouteVerifying, Ready"
        );
        assert_eq!(snapshot.proxy_route_probe_failures, 0);
        assert_eq!(snapshot.proxy_duplicate_exit_events, 0);
    }

    #[tokio::test]
    async fn bootstrap_failure_counts_one_attempt_and_failure_and_skips_later_stages() {
        let config = hybrid_config();
        let pool = pool(&config);
        let subsystem = Arc::new(RwLock::new(ProxySubsystemStatus::starting()));
        let runtime = TestRuntime {
            inspect_delay: Duration::ZERO,
            inspect_error: true,
        };
        let verifier = AlwaysVerifier;
        let metrics = Metrics::default();

        let result =
            reconcile_once(&pool, &subsystem, &runtime, &verifier, &config, &metrics).await;
        assert!(result.is_err(), "unavailable runtime must fail the cycle");

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.proxy_bootstrap_attempts, 2);
        assert_eq!(snapshot.proxy_bootstrap_successes, 0);
        assert_eq!(
            snapshot.proxy_bootstrap_failures, 2,
            "each failed container bootstrap counts exactly one failure"
        );
        assert_eq!(
            snapshot.proxy_state_transitions, 1,
            "only Starting is applied before bootstrap fails"
        );
        assert_eq!(snapshot.proxy_route_probe_failures, 0);
    }

    #[tokio::test]
    async fn route_probe_failure_counts_one_event_per_failing_candidate() {
        let config = hybrid_config();
        let pool = pool(&config);
        let subsystem = Arc::new(RwLock::new(ProxySubsystemStatus::starting()));
        let runtime = TestRuntime {
            inspect_delay: Duration::ZERO,
            inspect_error: false,
        };
        let verifier = DistinctIdentityVerifier::route_failure();
        let metrics = Metrics::default();

        let result =
            reconcile_once(&pool, &subsystem, &runtime, &verifier, &config, &metrics).await;
        assert!(
            result.is_err(),
            "all-failed route probes must fail the cycle"
        );

        let snapshot = metrics.snapshot();
        assert_eq!(
            snapshot.proxy_route_probe_failures, 2,
            "each failing candidate probe is exactly one event"
        );
        assert_eq!(
            snapshot.proxy_state_transitions, 4,
            "Starting through RouteVerifying; no Ready after failure"
        );
        assert_eq!(snapshot.proxy_bootstrap_attempts, 2);
        assert_eq!(snapshot.proxy_bootstrap_successes, 2);
        assert_eq!(snapshot.proxy_bootstrap_failures, 0);
    }

    #[tokio::test]
    async fn repeated_reconcile_cycles_accumulate_counters_additively() {
        let config = hybrid_config();
        let pool = pool(&config);
        let subsystem = Arc::new(RwLock::new(ProxySubsystemStatus::starting()));
        let runtime = TestRuntime {
            inspect_delay: Duration::ZERO,
            inspect_error: false,
        };
        let verifier = AlwaysVerifier;
        let metrics = Metrics::default();

        for _ in 0..2 {
            reconcile_once(&pool, &subsystem, &runtime, &verifier, &config, &metrics)
                .await
                .expect("full verification cycle");
        }

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.proxy_bootstrap_attempts, 4);
        assert_eq!(snapshot.proxy_bootstrap_successes, 4);
        assert_eq!(
            snapshot.proxy_state_transitions, 10,
            "two full cycles never double-charge a single transition application"
        );
    }

    #[tokio::test]
    async fn failed_cycle_marks_degraded_with_exactly_one_extra_transition() {
        let config = hybrid_config();
        let pool = pool(&config);
        let subsystem = Arc::new(RwLock::new(ProxySubsystemStatus::starting()));
        let runtime: Arc<dyn ContainerRuntime> = Arc::new(TestRuntime {
            inspect_delay: Duration::ZERO,
            inspect_error: true,
        });
        let verifier: Arc<dyn ProxyVerifier> = Arc::new(AlwaysVerifier);
        let metrics = Arc::new(Metrics::default());
        let registry = WorkerRegistry::new();
        let task_pool = pool.clone();
        let task_subsystem = subsystem.clone();
        let task_config = config.clone();
        let task_metrics = metrics.clone();
        registry.spawn_critical("test-reconcile", move |context| async move {
            hybrid_proxy_reconciler(
                task_pool,
                task_subsystem,
                runtime,
                verifier,
                task_config,
                task_metrics,
                context,
            )
            .await
        });

        for _ in 0..50 {
            if subsystem.read().await.snapshot().phase == ProxySubsystemPhase::Degraded {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        registry
            .shutdown(Duration::from_millis(250))
            .await
            .expect("worker remains cancellable while backing off");

        // The first backoff step exceeds the poll window, so exactly one cycle
        // has completed: Starting + Degraded transitions and one failed
        // container-bootstrap outcome per configured candidate.
        let snapshot = metrics.snapshot();
        assert_eq!(
            subsystem.read().await.snapshot().phase,
            ProxySubsystemPhase::Degraded
        );
        assert_eq!(
            snapshot.proxy_bootstrap_attempts, 2,
            "primary and warm standby are both bootstrapped"
        );
        assert_eq!(snapshot.proxy_bootstrap_failures, 2);
        assert_eq!(snapshot.proxy_bootstrap_successes, 0);
        assert_eq!(
            snapshot.proxy_state_transitions, 2,
            "Starting plus the degraded mark"
        );
    }
}
