//! Token and duration budgets of a run, shared by the pipeline and its agents.
//!
//! A [`RunBudget`] starts from what earlier invocations of the same run
//! already consumed, so limits hold across resumes. Agents add the usage of
//! every model call as it happens; once a limit is reached no new model call
//! starts and the pipeline pauses the run.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::Result;
use crate::provider::{
    CompletionRequest, CompletionResponse, DeltaSink, ModelProvider, ProviderInfo, SharedProvider,
    Usage,
};

tokio::task_local! {
    static CURRENT: Arc<RunBudget>;
}

/// Run `future` with `budget` as the current budget, so that
/// [`MeteredProvider`]s called from it count their usage against it.
pub async fn scope<F: Future>(budget: Arc<RunBudget>, future: F) -> F::Output {
    CURRENT.scope(budget, future).await
}

/// The budget set by the enclosing [`scope`], if any.
#[must_use]
pub fn current() -> Option<Arc<RunBudget>> {
    CURRENT.try_with(Arc::clone).ok()
}

/// A provider whose usage is added to the [`current`] run budget.
///
/// For model calls made outside agent runs (which count their own usage),
/// such as assisted conflict resolution.
#[derive(Clone)]
pub struct MeteredProvider {
    inner: SharedProvider,
}

impl std::fmt::Debug for MeteredProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeteredProvider")
            .field("inner", &self.inner.info().name)
            .finish()
    }
}

impl MeteredProvider {
    /// Meter `inner`.
    #[must_use]
    pub fn new(inner: SharedProvider) -> Self {
        Self { inner }
    }

    fn count(response: &Result<CompletionResponse>) {
        if let (Ok(r), Some(budget)) = (response, current()) {
            budget.add(r.usage);
        }
    }
}

#[async_trait::async_trait]
impl ModelProvider for MeteredProvider {
    fn info(&self) -> ProviderInfo {
        self.inner.info()
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        let response = self.inner.complete(request).await;
        Self::count(&response);
        response
    }

    async fn complete_streaming(
        &self,
        request: CompletionRequest,
        on_delta: DeltaSink<'_>,
    ) -> Result<CompletionResponse> {
        let response = self.inner.complete_streaming(request, on_delta).await;
        Self::count(&response);
        response
    }

    async fn health_check(&self) -> Result<()> {
        self.inner.health_check().await
    }
}

/// Limits of a run. `None` means unlimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BudgetLimits {
    /// Maximum input plus output tokens over the whole run.
    pub max_tokens: Option<u64>,
    /// Maximum active time over the whole run (time spent paused is not counted).
    pub max_duration: Option<Duration>,
}

impl BudgetLimits {
    /// Whether no limit is set.
    #[must_use]
    pub fn is_unlimited(&self) -> bool {
        self.max_tokens.is_none() && self.max_duration.is_none()
    }
}

/// Which limit a run reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetExceeded {
    /// The token limit.
    Tokens {
        /// Tokens consumed.
        used: u64,
        /// Configured limit.
        limit: u64,
    },
    /// The duration limit.
    Duration {
        /// Active time consumed.
        elapsed: Duration,
        /// Configured limit.
        limit: Duration,
    },
}

impl std::fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tokens { used, limit } => {
                write!(f, "token budget exhausted ({used} of {limit} tokens used)")
            }
            Self::Duration { elapsed, limit } => write!(
                f,
                "duration budget exhausted ({}s of {}s used)",
                elapsed.as_secs(),
                limit.as_secs()
            ),
        }
    }
}

/// Live accounting of a run against its [`BudgetLimits`]. Thread-safe; share
/// it behind an `Arc`.
#[derive(Debug)]
pub struct RunBudget {
    limits: BudgetLimits,
    tokens: AtomicU64,
    prior_elapsed: Duration,
    started: Instant,
    exceeded: Mutex<Option<BudgetExceeded>>,
}

impl RunBudget {
    /// Budget whose run already used `used_tokens` and `elapsed` in earlier
    /// invocations. The clock of this invocation starts now.
    #[must_use]
    pub fn new(limits: BudgetLimits, used_tokens: u64, elapsed: Duration) -> Self {
        Self {
            limits,
            tokens: AtomicU64::new(used_tokens),
            prior_elapsed: elapsed,
            started: Instant::now(),
            exceeded: Mutex::new(None),
        }
    }

    /// Budget without limits.
    #[must_use]
    pub fn unlimited() -> Self {
        Self::new(BudgetLimits::default(), 0, Duration::ZERO)
    }

    /// Configured limits.
    #[must_use]
    pub fn limits(&self) -> BudgetLimits {
        self.limits
    }

    /// Count the tokens of one model call.
    pub fn add(&self, usage: Usage) {
        self.tokens.fetch_add(usage.total(), Ordering::Relaxed);
    }

    /// Tokens consumed by the run so far, earlier invocations included.
    #[must_use]
    pub fn used_tokens(&self) -> u64 {
        self.tokens.load(Ordering::Relaxed)
    }

    /// Active time of the run so far, earlier invocations included.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.prior_elapsed + self.started.elapsed()
    }

    /// The limit reached, if any. Once reached, the answer never changes for
    /// this invocation, so every caller sees the same reason.
    pub fn exceeded(&self) -> Option<BudgetExceeded> {
        let mut first = self.exceeded.lock().unwrap_or_else(|e| e.into_inner());
        if first.is_none() {
            *first = self.check();
        }
        *first
    }

    fn check(&self) -> Option<BudgetExceeded> {
        if let Some(limit) = self.limits.max_tokens {
            let used = self.used_tokens();
            if used >= limit {
                return Some(BudgetExceeded::Tokens { used, limit });
            }
        }
        if let Some(limit) = self.limits.max_duration {
            let elapsed = self.elapsed();
            if elapsed >= limit {
                return Some(BudgetExceeded::Duration { elapsed, limit });
            }
        }
        None
    }
}

impl Default for RunBudget {
    fn default() -> Self {
        Self::unlimited()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, output: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            ..Usage::default()
        }
    }

    #[test]
    fn unlimited_never_exceeds() {
        let b = RunBudget::unlimited();
        b.add(usage(u64::MAX / 4, 0));
        assert!(b.limits().is_unlimited());
        assert_eq!(b.exceeded(), None);
    }

    #[test]
    fn tokens_count_earlier_invocations() {
        let limits = BudgetLimits {
            max_tokens: Some(100),
            max_duration: None,
        };
        let b = RunBudget::new(limits, 90, Duration::ZERO);
        assert_eq!(b.exceeded(), None);
        b.add(usage(6, 4));
        assert_eq!(
            b.exceeded(),
            Some(BudgetExceeded::Tokens {
                used: 100,
                limit: 100
            })
        );
        // The first reason sticks.
        b.add(usage(1, 0));
        assert_eq!(
            b.exceeded().unwrap().to_string(),
            "token budget exhausted (100 of 100 tokens used)"
        );
    }

    #[test]
    fn duration_counts_earlier_invocations() {
        let limits = BudgetLimits {
            max_tokens: None,
            max_duration: Some(Duration::from_secs(60)),
        };
        assert_eq!(
            RunBudget::new(limits, 0, Duration::from_secs(30)).exceeded(),
            None
        );
        let b = RunBudget::new(limits, 0, Duration::from_secs(61));
        assert!(matches!(
            b.exceeded(),
            Some(BudgetExceeded::Duration { limit, .. }) if limit == Duration::from_secs(60)
        ));
        assert!(b.elapsed() >= Duration::from_secs(61));
    }

    struct Fixed;

    #[async_trait::async_trait]
    impl ModelProvider for Fixed {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                name: "fixed".into(),
                supports_tools: false,
                supports_thinking: false,
                default_model: "m".into(),
            }
        }

        async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse> {
            Ok(CompletionResponse {
                message: crate::Message::assistant("ok"),
                stop_reason: crate::StopReason::EndTurn,
                usage: usage(7, 3),
                model: "m".into(),
            })
        }
    }

    #[tokio::test]
    async fn metered_calls_count_against_the_scoped_budget() {
        let provider = MeteredProvider::new(Arc::new(Fixed));
        let request = || CompletionRequest::new("m", vec![]);
        // Outside a scope nothing is counted, and nothing fails.
        provider.complete(request()).await.unwrap();
        assert!(current().is_none());
        let budget = Arc::new(RunBudget::unlimited());
        scope(Arc::clone(&budget), async {
            provider.complete(request()).await.unwrap();
            let sink = |_d: crate::StreamDelta| {};
            provider.complete_streaming(request(), &sink).await.unwrap();
        })
        .await;
        assert_eq!(budget.used_tokens(), 20);
    }
}
