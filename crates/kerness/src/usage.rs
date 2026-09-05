//! Provider accounting and cooperative run budgets.
//!
//! Raw provider usage remains untouched. Missing measurements are `None`,
//! including failed requests that may still have incurred a charge. The
//! synchronous observer follows calls on the current thread only; providers
//! that override dispatch are recorded as opaque logical operations.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::provider::ProviderResponse;

/// Common usage counts. Cached input and reasoning are subsets, not extra
/// tokens to add to `total_tokens`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

impl NormalizedUsage {
    /// Normalize OpenAI-compatible and Anthropic token keys. Invalid counts
    /// and arithmetic overflow are unknown, never silently rounded or zeroed.
    pub fn from_reported(usage: &Map<String, Value>) -> Self {
        let count = |name: &str| usage.get(name).and_then(Value::as_u64);
        let nested = |parent: &str, name: &str| {
            usage
                .get(parent)
                .and_then(|v| v.get(name))
                .and_then(Value::as_u64)
        };
        let anthropic_cache = usage.contains_key("cache_read_input_tokens")
            || usage.contains_key("cache_creation_input_tokens");
        let cached_input_tokens = if anthropic_cache {
            count("cache_read_input_tokens")
        } else {
            nested("prompt_tokens_details", "cached_tokens")
                .or_else(|| nested("input_tokens_details", "cached_tokens"))
        };
        let cache_creation_input_tokens = count("cache_creation_input_tokens");
        let mut input_tokens = if usage.contains_key("input_tokens") {
            count("input_tokens")
        } else {
            count("prompt_tokens")
        };
        if anthropic_cache {
            // Anthropic's input_tokens excludes both cache buckets; OpenAI's
            // prompt_tokens already includes its cached-token subset.
            for key in ["cache_read_input_tokens", "cache_creation_input_tokens"] {
                if usage.contains_key(key) {
                    input_tokens = add_counts(input_tokens, count(key));
                }
            }
        }
        let output_tokens = if usage.contains_key("output_tokens") {
            count("output_tokens")
        } else {
            count("completion_tokens")
        };
        let derived_total = add_counts(input_tokens, output_tokens);
        let total_tokens = if usage.contains_key("total_tokens") {
            count("total_tokens").filter(|total| derived_total.is_none_or(|sum| sum == *total))
        } else {
            derived_total
        };
        Self {
            input_tokens,
            output_tokens,
            total_tokens,
            cached_input_tokens,
            cache_creation_input_tokens,
            reasoning_tokens: nested("completion_tokens_details", "reasoning_tokens")
                .or_else(|| nested("output_tokens_details", "reasoning_tokens")),
        }
    }

    fn zero() -> Self {
        Self {
            input_tokens: Some(0),
            output_tokens: Some(0),
            total_tokens: Some(0),
            cached_input_tokens: Some(0),
            cache_creation_input_tokens: Some(0),
            reasoning_tokens: Some(0),
        }
    }

    fn add(&mut self, other: &Self) {
        self.input_tokens = add_counts(self.input_tokens, other.input_tokens);
        self.output_tokens = add_counts(self.output_tokens, other.output_tokens);
        self.total_tokens = add_counts(self.total_tokens, other.total_tokens);
        self.cached_input_tokens = add_counts(self.cached_input_tokens, other.cached_input_tokens);
        self.cache_creation_input_tokens = add_counts(
            self.cache_creation_input_tokens,
            other.cache_creation_input_tokens,
        );
        self.reasoning_tokens = add_counts(self.reasoning_tokens, other.reasoning_tokens);
    }
}

fn add_counts(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    left?.checked_add(right?)
}

/// Host-supplied prices in millionths of a US dollar per million tokens.
/// Integer prices cannot be negative, NaN, or infinite. No price registry or
/// assumption about a provider's billing is built into the engine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenPricing {
    pub provider: String,
    pub model: String,
    pub input_microusd_per_million_tokens: u64,
    pub output_microusd_per_million_tokens: u64,
    pub cached_input_microusd_per_million_tokens: Option<u64>,
    pub cache_creation_microusd_per_million_tokens: Option<u64>,
}

impl TokenPricing {
    /// Price known counts, rounding up to one microdollar per operation. A
    /// special cache price requires its corresponding cache measurement.
    pub fn cost_microusd(&self, usage: &NormalizedUsage) -> Option<u64> {
        let mut ordinary_input = usage.input_tokens?;
        let mut amount = 0_u128;
        for (rate, tokens) in [
            (
                self.cached_input_microusd_per_million_tokens,
                usage.cached_input_tokens,
            ),
            (
                self.cache_creation_microusd_per_million_tokens,
                usage.cache_creation_input_tokens,
            ),
        ] {
            if let Some(rate) = rate {
                let tokens = tokens?;
                ordinary_input = ordinary_input.checked_sub(tokens)?;
                amount = amount.checked_add(u128::from(tokens) * u128::from(rate))?;
            }
        }
        amount = amount.checked_add(
            u128::from(ordinary_input) * u128::from(self.input_microusd_per_million_tokens),
        )?;
        amount = amount.checked_add(
            u128::from(usage.output_tokens?) * u128::from(self.output_microusd_per_million_tokens),
        )?;
        u64::try_from(amount.div_ceil(1_000_000)).ok()
    }
}

/// One observed attempt, or one opaque logical operation when a custom
/// provider bypasses the supplied dispatch. Opaque usage is always unknown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderUsage {
    pub actor: String,
    pub provider: String,
    pub model: String,
    pub purpose: String,
    pub failed: bool,
    pub opaque: bool,
    pub usage: NormalizedUsage,
    pub cost_microusd: Option<u64>,
}

/// Aggregates retain unknown measurements; inspect records for known subtotals.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageTotals {
    pub provider_operations: u64,
    pub failed_operations: u64,
    pub opaque_operations: u64,
    pub usage: NormalizedUsage,
    pub cost_microusd: Option<u64>,
}

impl Default for UsageTotals {
    fn default() -> Self {
        Self {
            provider_operations: 0,
            failed_operations: 0,
            opaque_operations: 0,
            usage: NormalizedUsage::zero(),
            cost_microusd: Some(0),
        }
    }
}

impl UsageTotals {
    fn record(&mut self, record: &ProviderUsage) {
        self.provider_operations = self.provider_operations.saturating_add(1);
        self.failed_operations = self
            .failed_operations
            .saturating_add(u64::from(record.failed));
        self.opaque_operations = self
            .opaque_operations
            .saturating_add(u64::from(record.opaque));
        self.usage.add(&record.usage);
        self.cost_microusd = add_counts(self.cost_microusd, record.cost_microusd);
    }
}

/// Checkpointable run accounting. Elapsed time counts the lifetime of a live
/// collector, including host waits; time while a saved run is offline does not.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageLedger {
    pub totals: UsageTotals,
    pub tool_calls: u64,
    pub elapsed_ms: u64,
    pub records: Vec<ProviderUsage>,
}

impl UsageLedger {
    pub fn by_actor(&self) -> BTreeMap<String, UsageTotals> {
        self.group_by(|record| &record.actor)
    }

    pub fn by_provider(&self) -> BTreeMap<String, UsageTotals> {
        self.group_by(|record| &record.provider)
    }

    fn group_by(&self, key: impl Fn(&ProviderUsage) -> &String) -> BTreeMap<String, UsageTotals> {
        let mut grouped = BTreeMap::<String, UsageTotals>::new();
        for record in &self.records {
            grouped
                .entry(key(record).clone())
                .or_default()
                .record(record);
        }
        grouped
    }
}

/// Hard operation/tool limits are exact at the synchronous action boundary.
/// Token and cost thresholds can overshoot by the one operation in flight.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetMode {
    #[default]
    Hard,
    MeasuredThreshold,
}

/// Cooperative execution limits. All `None` means unbounded.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunBudget {
    pub max_provider_operations: Option<u64>,
    pub max_tool_calls: Option<u64>,
    pub max_tokens: Option<u64>,
    pub max_cost_microusd: Option<u64>,
    pub max_elapsed_ms: Option<u64>,
    #[serde(default)]
    pub mode: BudgetMode,
}

impl RunBudget {
    pub fn validate(&self) -> Result<()> {
        if self.mode == BudgetMode::Hard
            && (self.max_tokens.is_some() || self.max_cost_microusd.is_some())
        {
            return Err(Error::Value(
                "Hard token/cost budgets require a provable per-operation upper bound, which \
                 Provider does not expose; select measured_threshold explicitly."
                    .to_string(),
            ));
        }
        Ok(())
    }
}

/// A typed reason a budget refused the next action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetExceeded {
    ProviderOperations,
    ToolCalls,
    Tokens,
    Cost,
    Elapsed,
    UnknownTokenUsage,
    UnknownCost,
}

impl std::fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ProviderOperations => "provider operation budget exhausted",
            Self::ToolCalls => "tool call budget exhausted",
            Self::Tokens => "token budget exhausted",
            Self::Cost => "cost budget exhausted",
            Self::Elapsed => "elapsed time budget exhausted",
            Self::UnknownTokenUsage => "token budget cannot continue with unknown usage",
            Self::UnknownCost => "cost budget cannot continue with unknown cost",
        })
    }
}

struct CollectorState {
    ledger: UsageLedger,
    budget: RunBudget,
    pricing: Vec<TokenPricing>,
    started: Instant,
    blocked: Option<BudgetExceeded>,
}

/// Run-owned accounting handle. Callbacks run without its mutex held. A scope
/// is thread-local and restored on unwind, so independent runs cannot borrow
/// each other's attribution or accounting.
#[derive(Clone)]
pub struct UsageCollector(Arc<Mutex<CollectorState>>);

impl UsageCollector {
    pub fn new(budget: RunBudget, pricing: Vec<TokenPricing>) -> Result<Self> {
        Self::restore(budget, pricing, UsageLedger::default())
    }

    pub fn restore(
        budget: RunBudget,
        pricing: Vec<TokenPricing>,
        ledger: UsageLedger,
    ) -> Result<Self> {
        budget.validate()?;
        for (index, price) in pricing.iter().enumerate() {
            if pricing[..index].iter().any(|previous| {
                previous.provider == price.provider && previous.model == price.model
            }) {
                return Err(Error::Value(format!(
                    "Duplicate token pricing for provider {} model {}",
                    price.provider, price.model
                )));
            }
        }
        let mut totals = UsageTotals::default();
        for record in &ledger.records {
            totals.record(record);
        }
        if totals != ledger.totals {
            return Err(Error::Value(
                "Usage ledger totals do not match its records".to_string(),
            ));
        }
        Ok(Self(Arc::new(Mutex::new(CollectorState {
            ledger,
            budget,
            pricing,
            started: Instant::now(),
            blocked: None,
        }))))
    }

    fn state(&self) -> MutexGuard<'_, CollectorState> {
        self.0.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    pub fn snapshot(&self) -> UsageLedger {
        let state = self.state();
        let mut ledger = state.ledger.clone();
        ledger.elapsed_ms = elapsed_ms(&state);
        ledger
    }

    pub fn blocked_reason(&self) -> Option<BudgetExceeded> {
        self.state().blocked.clone()
    }

    /// Check elapsed, token, and cost limits at any action boundary. Provider
    /// and tool counts are checked only for their respective action kinds.
    pub fn check_next(&self) -> Result<()> {
        self.check(None)
    }

    /// Reserve one tool invocation before executing its handler. Denials and
    /// approval waits should not call this; invoked handlers count even on error.
    pub fn begin_tool(&self) -> Result<()> {
        self.check(Some(false))?;
        let mut state = self.state();
        state.ledger.tool_calls = state.ledger.tool_calls.saturating_add(1);
        Ok(())
    }

    pub fn with_scope<T>(&self, actor: &str, purpose: &str, operation: impl FnOnce() -> T) -> T {
        let _guard = ScopeGuard::enter(Scope {
            collector: self.clone(),
            actor: actor.to_string(),
            purpose: purpose.to_string(),
        });
        operation()
    }

    /// Wrap every engine call to a provider, including compaction and closing.
    /// Default dispatch reports individual attempts. An override bypassing it
    /// contributes one opaque operation, whose hidden retries cannot be metered.
    pub fn provider_call(
        &self,
        actor: &str,
        provider: &str,
        model: &str,
        purpose: &str,
        operation: impl FnOnce() -> Result<ProviderResponse>,
    ) -> Result<ProviderResponse> {
        self.with_scope(actor, purpose, || observe(provider, model, true, operation))
    }

    fn check(&self, provider_action: Option<bool>) -> Result<()> {
        let mut state = self.state();
        let budget = &state.budget;
        let ledger = &state.ledger;
        let reason = state.blocked.clone().or_else(|| {
            if budget
                .max_elapsed_ms
                .is_some_and(|max| elapsed_ms(&state) >= max)
            {
                return Some(BudgetExceeded::Elapsed);
            }
            if provider_action == Some(true)
                && budget
                    .max_provider_operations
                    .is_some_and(|max| ledger.totals.provider_operations >= max)
            {
                return Some(BudgetExceeded::ProviderOperations);
            }
            if provider_action == Some(false)
                && budget
                    .max_tool_calls
                    .is_some_and(|max| ledger.tool_calls >= max)
            {
                return Some(BudgetExceeded::ToolCalls);
            }
            if let Some(max) = budget.max_tokens {
                match ledger.totals.usage.total_tokens {
                    None => return Some(BudgetExceeded::UnknownTokenUsage),
                    Some(count) if count >= max => return Some(BudgetExceeded::Tokens),
                    _ => {}
                }
            }
            if let Some(max) = budget.max_cost_microusd {
                match ledger.totals.cost_microusd {
                    None => return Some(BudgetExceeded::UnknownCost),
                    Some(count) if count >= max => return Some(BudgetExceeded::Cost),
                    _ => {}
                }
            }
            None
        });
        if let Some(reason) = reason {
            state.blocked = Some(reason.clone());
            return Err(Error::session(reason.to_string()));
        }
        Ok(())
    }

    fn record(
        &self,
        actor: &str,
        provider: &str,
        model: &str,
        purpose: &str,
        opaque: bool,
        result: &Result<ProviderResponse>,
    ) {
        let usage = match result {
            Ok(response) if !opaque => NormalizedUsage::from_reported(&response.usage),
            _ => NormalizedUsage::default(),
        };
        let mut state = self.state();
        let cost_microusd = state
            .pricing
            .iter()
            .find(|price| price.provider == provider && price.model == model)
            .and_then(|price| price.cost_microusd(&usage));
        let record = ProviderUsage {
            actor: actor.to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            purpose: purpose.to_string(),
            failed: result.is_err(),
            opaque,
            usage,
            cost_microusd,
        };
        state.ledger.totals.record(&record);
        state.ledger.records.push(record);
    }
}

fn elapsed_ms(state: &CollectorState) -> u64 {
    state
        .ledger
        .elapsed_ms
        .saturating_add(u64::try_from(state.started.elapsed().as_millis()).unwrap_or(u64::MAX))
}

#[derive(Clone)]
struct Scope {
    collector: UsageCollector,
    actor: String,
    purpose: String,
}

thread_local! {
    static ACTIVE: RefCell<Option<Scope>> = const { RefCell::new(None) };
    static CLEANUP: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Cleanup may flush resources but cannot start framework provider work. The
/// flag also covers callers without an active accounting scope and restores
/// on unwind. A refused call remains an error even if a custom cleanup catches
/// it or a provider's retry policy changes the error variant.
pub(crate) fn without_provider_calls<T>(operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let guard = CleanupGuard(CLEANUP.with(|state| state.replace(Some(false))));
    let result = operation();
    let refused = CLEANUP.with(|state| state.get().unwrap_or(false));
    drop(guard);
    if refused {
        Err(cleanup_error())
    } else {
        result
    }
}

struct CleanupGuard(Option<bool>);

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        CLEANUP.with(|state| {
            let refused = state.get().unwrap_or(false);
            state.set(self.0.map(|previous| previous || refused));
        });
    }
}

fn cleanup_error() -> Error {
    Error::session(
        "Provider operations cannot start during run cleanup; use explicit memory maintenance.",
    )
}

fn check_provider_calls_allowed() -> Result<()> {
    CLEANUP.with(|state| match state.get() {
        Some(_) => {
            state.set(Some(true));
            Err(cleanup_error())
        }
        None => Ok(()),
    })
}

struct ScopeGuard(Option<Scope>);

impl ScopeGuard {
    fn enter(scope: Scope) -> Self {
        Self(ACTIVE.with(|active| active.replace(Some(scope))))
    }
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.replace(self.0.take()));
    }
}

fn observe(
    provider: &str,
    model: &str,
    opaque: bool,
    operation: impl FnOnce() -> Result<ProviderResponse>,
) -> Result<ProviderResponse> {
    check_provider_calls_allowed()?;
    let scope = ACTIVE.with(|active| active.borrow().clone());
    let Some(scope) = scope else {
        return operation();
    };
    scope.collector.check(Some(true))?;
    let before = scope.collector.state().ledger.totals.provider_operations;
    let result = operation();
    let after = scope.collector.state().ledger.totals.provider_operations;
    if before == after && scope.collector.blocked_reason().is_none() {
        scope.collector.record(
            &scope.actor,
            provider,
            model,
            &scope.purpose,
            opaque,
            &result,
        );
    }
    result
}

pub(crate) fn observe_provider_call(
    provider: &str,
    model: &str,
    purpose: &str,
    operation: impl FnOnce() -> Result<ProviderResponse>,
) -> Result<ProviderResponse> {
    check_provider_calls_allowed()?;
    let scope = ACTIVE.with(|active| active.borrow().clone());
    let Some(scope) = scope else {
        return operation();
    };
    scope.collector.with_scope(&scope.actor, purpose, || {
        observe(provider, model, true, operation)
    })
}

pub(crate) fn observe_attempt(
    provider: &str,
    model: &str,
    operation: impl FnOnce() -> Result<ProviderResponse>,
) -> Result<ProviderResponse> {
    observe(provider, model, false, operation)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn normalized(value: Value) -> NormalizedUsage {
        NormalizedUsage::from_reported(value.as_object().unwrap())
    }

    fn reply(input: u64, output: u64) -> Result<ProviderResponse> {
        Ok(ProviderResponse {
            usage: json!({"prompt_tokens": input, "completion_tokens": output})
                .as_object()
                .unwrap()
                .clone(),
            ..ProviderResponse::text("ok")
        })
    }

    fn pricing() -> TokenPricing {
        TokenPricing {
            provider: "provider".to_string(),
            model: "model".to_string(),
            input_microusd_per_million_tokens: 1_000_000,
            output_microusd_per_million_tokens: 2_000_000,
            cached_input_microusd_per_million_tokens: None,
            cache_creation_microusd_per_million_tokens: None,
        }
    }

    fn call(collector: &UsageCollector, actor: &str, purpose: &str) -> Result<ProviderResponse> {
        collector.provider_call(actor, "provider", "model", purpose, || {
            observe_attempt("provider", "model", || reply(2, 3))
        })
    }

    #[test]
    fn normalization_preserves_unknown_zero_and_vendor_subsets() {
        assert_eq!(normalized(json!({})), NormalizedUsage::default());
        assert_eq!(
            normalized(json!({"input_tokens": 0, "output_tokens": 0})).total_tokens,
            Some(0)
        );
        assert_eq!(normalized(json!({"prompt_tokens": 7})).total_tokens, None);
        let openai = normalized(json!({
            "prompt_tokens": 20, "completion_tokens": 5, "total_tokens": 25,
            "prompt_tokens_details": {"cached_tokens": 10},
            "completion_tokens_details": {"reasoning_tokens": 2}
        }));
        assert_eq!(openai.total_tokens, Some(25));
        assert_eq!(openai.cached_input_tokens, Some(10));
        assert_eq!(openai.reasoning_tokens, Some(2));
        let claude = normalized(json!({
            "input_tokens": 10, "output_tokens": 5,
            "cache_read_input_tokens": 20, "cache_creation_input_tokens": 30
        }));
        assert_eq!(claude.input_tokens, Some(60));
        assert_eq!(claude.total_tokens, Some(65));
        for invalid in [json!(-1), json!(1.5), json!("10"), Value::Null] {
            assert_eq!(
                normalized(json!({"input_tokens": invalid, "output_tokens": 0})).total_tokens,
                None
            );
        }
        assert_eq!(
            normalized(json!({"input_tokens": u64::MAX, "output_tokens": 1})).total_tokens,
            None
        );
        assert_eq!(
            normalized(json!({"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 1}))
                .total_tokens,
            None
        );
        assert_eq!(
            normalized(
                json!({"input_tokens": 2, "output_tokens": 3, "cache_read_input_tokens": -1})
            )
            .input_tokens,
            None
        );
    }

    #[test]
    fn prices_are_host_supplied_checked_and_rounded_up() {
        let counts = normalized(json!({"prompt_tokens": 2, "completion_tokens": 3}));
        assert_eq!(pricing().cost_microusd(&counts), Some(8));
        assert_eq!(pricing().cost_microusd(&NormalizedUsage::default()), None);
        let mut prices = pricing();
        prices.cached_input_microusd_per_million_tokens = Some(0);
        assert_eq!(prices.cost_microusd(&counts), None);
        let cached = NormalizedUsage {
            cached_input_tokens: Some(1),
            ..counts.clone()
        };
        assert_eq!(prices.cost_microusd(&cached), Some(7));
        assert_eq!(
            prices.cost_microusd(&NormalizedUsage {
                cached_input_tokens: Some(3),
                ..counts.clone()
            }),
            None
        );
        prices = pricing();
        prices.input_microusd_per_million_tokens = 1;
        prices.output_microusd_per_million_tokens = 1;
        assert_eq!(prices.cost_microusd(&counts), Some(1));
        prices.input_microusd_per_million_tokens = u64::MAX;
        prices.output_microusd_per_million_tokens = u64::MAX;
        assert_eq!(
            prices.cost_microusd(&NormalizedUsage {
                input_tokens: Some(u64::MAX),
                output_tokens: Some(u64::MAX),
                ..counts
            }),
            None
        );
        assert!(UsageCollector::new(RunBudget::default(), vec![pricing(), pricing()]).is_err());
        let mut invalid = serde_json::to_value(pricing()).unwrap();
        invalid["input_microusd_per_million_tokens"] = json!(-1);
        assert!(serde_json::from_value::<TokenPricing>(invalid).is_err());
    }

    #[test]
    fn collector_attributes_compaction_closing_and_opaque_overrides() {
        let collector = UsageCollector::new(RunBudget::default(), vec![pricing()]).unwrap();
        call(&collector, "alice", "turn").unwrap();
        call(&collector, "alice", "compaction").unwrap();
        call(&collector, "bob", "closing").unwrap();
        let ledger = collector.snapshot();
        assert_eq!(
            ledger.totals.provider_operations, 3,
            "nested wrappers count once"
        );
        assert_eq!(ledger.totals.usage.total_tokens, Some(15));
        assert_eq!(ledger.totals.cost_microusd, Some(24));
        assert_eq!(ledger.by_actor()["alice"].provider_operations, 2);
        assert_eq!(ledger.by_actor()["bob"].provider_operations, 1);
        assert_eq!(ledger.by_provider()["provider"].provider_operations, 3);
        assert_eq!(ledger.records[1].purpose, "compaction");
        assert_eq!(ledger.records[2].purpose, "closing");

        collector
            .provider_call("bob", "provider", "model", "override", || reply(1, 1))
            .unwrap();
        let ledger = collector.snapshot();
        assert_eq!(ledger.totals.provider_operations, 4);
        assert_eq!(ledger.totals.opaque_operations, 1);
        assert_eq!(
            ledger.totals.usage.total_tokens, None,
            "hidden retries may have usage"
        );
        assert_eq!(ledger.totals.cost_microusd, None);
        assert!(ledger.records[3].opaque);
        assert_eq!(ledger.records[3].usage, NormalizedUsage::default());
    }

    #[test]
    fn scopes_restore_on_return_and_unwind_without_cross_run_attribution() {
        let one = UsageCollector::new(RunBudget::default(), vec![]).unwrap();
        let two = UsageCollector::new(RunBudget::default(), vec![]).unwrap();
        one.with_scope("one", "outer", || {
            let failed =
                std::panic::catch_unwind(|| two.with_scope("two", "panic", || panic!("callback")));
            assert!(failed.is_err());
            observe_attempt("provider", "model", || reply(1, 1)).unwrap();
        });
        observe_attempt("provider", "model", || reply(1, 1)).unwrap();
        assert_eq!(one.snapshot().totals.provider_operations, 1);
        assert_eq!(one.snapshot().records[0].actor, "one");
        assert_eq!(two.snapshot().totals.provider_operations, 0);

        let refused = without_provider_calls(|| {
            observe_provider_call("provider", "model", "cleanup", || {
                panic!("must not dispatch")
            })
        });
        assert!(
            matches!(refused, Err(Error::Session(_))),
            "cleanup is guarded even without a collector"
        );
        assert!(without_provider_calls(|| call(&one, "one", "cleanup")).is_err());
        assert_eq!(
            one.snapshot().totals.provider_operations,
            1,
            "forbidden calls cost no operation"
        );
        let swallowed = without_provider_calls(|| {
            let _ = without_provider_calls(|| {
                observe_attempt("provider", "model", || {
                    panic!("nested cleanup cannot dispatch")
                })
            });
            Ok(())
        });
        assert!(
            matches!(swallowed, Err(Error::Session(_))),
            "nested refusal cannot be swallowed by cleanup"
        );
        let panicked = std::panic::catch_unwind(|| {
            without_provider_calls::<()>(|| panic!("cleanup callback"))
        });
        assert!(panicked.is_err());
        call(&one, "one", "after cleanup").unwrap();
        assert_eq!(
            one.snapshot().totals.provider_operations,
            2,
            "cleanup scope restores after unwind"
        );
        let failure = Error::Io("flush failed".into());
        assert_eq!(
            without_provider_calls::<()>(|| Err(failure.clone())),
            Err(failure)
        );
    }

    #[test]
    fn budgets_gate_next_actions_and_reject_unprovable_hard_limits() {
        for budget in [
            RunBudget {
                max_tokens: Some(10),
                ..RunBudget::default()
            },
            RunBudget {
                max_cost_microusd: Some(10),
                ..RunBudget::default()
            },
        ] {
            assert!(UsageCollector::new(budget, vec![pricing()]).is_err());
        }
        for (budget, expected) in [
            (
                RunBudget {
                    max_provider_operations: Some(1),
                    ..RunBudget::default()
                },
                BudgetExceeded::ProviderOperations,
            ),
            (
                RunBudget {
                    max_tokens: Some(4),
                    mode: BudgetMode::MeasuredThreshold,
                    ..RunBudget::default()
                },
                BudgetExceeded::Tokens,
            ),
            (
                RunBudget {
                    max_cost_microusd: Some(7),
                    mode: BudgetMode::MeasuredThreshold,
                    ..RunBudget::default()
                },
                BudgetExceeded::Cost,
            ),
        ] {
            let collector = UsageCollector::new(budget, vec![pricing()]).unwrap();
            call(&collector, "alice", "turn").unwrap();
            assert!(call(&collector, "alice", "next").is_err());
            assert_eq!(collector.snapshot().totals.provider_operations, 1);
            assert_eq!(collector.blocked_reason(), Some(expected));
        }
        let collector = UsageCollector::new(
            RunBudget {
                max_tool_calls: Some(1),
                ..RunBudget::default()
            },
            vec![],
        )
        .unwrap();
        collector.begin_tool().unwrap();
        assert!(collector.begin_tool().is_err());
        assert_eq!(collector.snapshot().tool_calls, 1);
        assert_eq!(collector.blocked_reason(), Some(BudgetExceeded::ToolCalls));
        let collector = UsageCollector::new(
            RunBudget {
                max_provider_operations: Some(0),
                ..RunBudget::default()
            },
            vec![],
        )
        .unwrap();
        assert!(collector
            .provider_call("alice", "provider", "model", "turn", || panic!(
                "must not dispatch"
            ))
            .is_err());
        assert_eq!(collector.snapshot().totals.provider_operations, 0);
    }

    #[test]
    fn unknown_measurements_stop_metered_runs_and_checkpoints_keep_budget_spent() {
        for (budget, expected) in [
            (
                RunBudget {
                    max_tokens: Some(100),
                    mode: BudgetMode::MeasuredThreshold,
                    ..RunBudget::default()
                },
                BudgetExceeded::UnknownTokenUsage,
            ),
            (
                RunBudget {
                    max_cost_microusd: Some(100),
                    mode: BudgetMode::MeasuredThreshold,
                    ..RunBudget::default()
                },
                BudgetExceeded::UnknownCost,
            ),
        ] {
            let collector = UsageCollector::new(budget, vec![]).unwrap();
            collector
                .provider_call("alice", "provider", "model", "opaque", || reply(1, 1))
                .unwrap();
            assert!(collector.check_next().is_err());
            assert_eq!(collector.blocked_reason(), Some(expected));
        }
        let collector = UsageCollector::new(RunBudget::default(), vec![pricing()]).unwrap();
        call(&collector, "alice", "turn").unwrap();
        collector.begin_tool().unwrap();
        let mut ledger: UsageLedger =
            serde_json::from_str(&serde_json::to_string(&collector.snapshot()).unwrap()).unwrap();
        ledger.elapsed_ms = 10;
        let restored = UsageCollector::restore(
            RunBudget {
                max_elapsed_ms: Some(10),
                ..RunBudget::default()
            },
            vec![pricing()],
            ledger.clone(),
        )
        .unwrap();
        assert_eq!(restored.snapshot().totals.usage.total_tokens, Some(5));
        assert_eq!(restored.snapshot().tool_calls, 1);
        assert!(restored.check_next().is_err());
        assert_eq!(restored.blocked_reason(), Some(BudgetExceeded::Elapsed));
        ledger.totals.provider_operations = 0;
        assert!(UsageCollector::restore(RunBudget::default(), vec![pricing()], ledger).is_err());
    }
}
