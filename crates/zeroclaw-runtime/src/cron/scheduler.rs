use crate::cron::{
    CronJob, CronJobPatch, DeliveryConfig, JobType, Schedule, SessionTarget, all_overdue_jobs,
    due_jobs, next_run_for_schedule, record_last_run, record_run, remove_job, reschedule_after_run,
    sync_declarative_jobs, update_job,
};
use crate::security::SecurityPolicy;
use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use futures_util::{StreamExt, stream};
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;
use tokio::time::{self, Duration};
use zeroclaw_config::schema::Config;
use zeroclaw_config::schema::{CronJobDecl, CronScheduleDecl};

const MIN_POLL_SECONDS: u64 = 5;
const SHELL_JOB_TIMEOUT_SECS: u64 = 120;
const SCHEDULER_COMPONENT: &str = "scheduler";

/// Type alias for the optional broadcast sender used to push cron results
/// to connected dashboard/SSE clients.
pub type EventBroadcast = Option<tokio::sync::broadcast::Sender<serde_json::Value>>;

pub async fn run(config: Config, event_tx: EventBroadcast) -> Result<()> {
    let poll_secs = config.reliability.scheduler_poll_secs.max(MIN_POLL_SECONDS);
    let mut interval = time::interval(Duration::from_secs(poll_secs));
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let security = Arc::new(SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));

    crate::health::mark_component_ok(SCHEDULER_COMPONENT);

    // ── Declarative job sync: reconcile config-defined jobs with the DB.
    let mut jobs_with_builtin = config.cron.jobs.clone();
    if let Some(ref schedule_cron) = config.backup.schedule_cron {
        let backup_job = CronJobDecl {
            id: "__builtin_backup".to_string(),
            name: Some("Scheduled backup".to_string()),
            job_type: "shell".to_string(),
            schedule: CronScheduleDecl::Cron {
                expr: schedule_cron.clone(),
                tz: config.backup.schedule_timezone.clone(),
            },
            command: Some("backup create".to_string()),
            prompt: None,
            enabled: true,
            model: None,
            allowed_tools: None,
            uses_memory: true,
            session_target: None,
            delivery: None,
        };
        tracing::debug!(
            schedule = %schedule_cron,
            "Synthesizing builtin backup cron job from config.backup.schedule_cron"
        );
        jobs_with_builtin.push(backup_job);
    }

    match sync_declarative_jobs(&config, &jobs_with_builtin) {
        Ok(()) => {
            if !jobs_with_builtin.is_empty() {
                tracing::info!(
                    count = jobs_with_builtin.len(),
                    "Synced declarative cron jobs from config"
                );
            }
        }
        Err(e) => tracing::warn!("Failed to sync declarative cron jobs: {e}"),
    }

    // Piece E: warn on declarative jobs that opt into the legacy
    // silent-failure semantic. The default flipped to false in
    // piece E -- explicit `true` is now an opt-in choice the
    // operator should be aware of. The cron-delivery posture
    // marker (piece D) still fires regardless of `best_effort`,
    // but per-tick cron history will hide the failure.
    warn_on_best_effort_declarative_jobs(&jobs_with_builtin);

    // ── Startup catch-up: run ALL overdue jobs before entering the
    //    normal polling loop. The regular loop is capped by `max_tasks`,
    //    which could leave some overdue jobs waiting across many cycles
    //    if the machine was off for a while. The catch-up phase fetches
    //    without the `max_tasks` limit so every missed job fires once.
    //    Controlled by `[cron] catch_up_on_startup` (default: true).
    if config.cron.catch_up_on_startup {
        catch_up_overdue_jobs(&config, &security, &event_tx).await;
    } else {
        tracing::info!("Scheduler startup: catch-up disabled by config");
    }

    loop {
        interval.tick().await;
        // Keep scheduler liveness fresh even when there are no due jobs.
        crate::health::mark_component_ok(SCHEDULER_COMPONENT);

        let jobs = match due_jobs(&config, Utc::now()) {
            Ok(jobs) => jobs,
            Err(e) => {
                crate::health::mark_component_error(SCHEDULER_COMPONENT, e.to_string());
                tracing::warn!("Scheduler query failed: {e}");
                continue;
            }
        };

        process_due_jobs(&config, &security, jobs, SCHEDULER_COMPONENT, &event_tx).await;
    }
}

/// Inspect the declarative cron jobs and emit a `tracing::warn` for
/// each one that opts into legacy silent-failure semantics (piece E).
/// Pulled out so unit tests can exercise the predicate without
/// running the full scheduler.
pub(crate) fn warn_on_best_effort_declarative_jobs(jobs: &[CronJobDecl]) -> Vec<String> {
    let mut warned: Vec<String> = Vec::new();
    for job in jobs {
        let Some(delivery) = job.delivery.as_ref() else {
            continue;
        };
        if !delivery.mode.eq_ignore_ascii_case("announce") {
            continue;
        }
        if !delivery.best_effort {
            continue;
        }
        let channel = delivery.channel.as_deref().unwrap_or("(unset)");
        tracing::warn!(
            job_id = %job.id,
            channel = %channel,
            "declarative cron job opts into best_effort=true: \
             delivery failures will be hidden in per-tick cron \
             history. The cron-delivery posture marker still \
             fires (zeroclaw doctor), but if you want failures \
             to surface in cron history too, set \
             best_effort = false."
        );
        warned.push(job.id.clone());
    }
    warned
}

/// Fetch **all** overdue jobs (ignoring `max_tasks`) and execute them.
///
/// Called once at scheduler startup so that jobs missed during downtime
/// (e.g. late boot, daemon restart) are caught up immediately.
async fn catch_up_overdue_jobs(
    config: &Config,
    security: &Arc<SecurityPolicy>,
    event_tx: &EventBroadcast,
) {
    let now = Utc::now();
    let jobs = match all_overdue_jobs(config, now) {
        Ok(jobs) => jobs,
        Err(e) => {
            tracing::warn!("Startup catch-up query failed: {e}");
            return;
        }
    };

    if jobs.is_empty() {
        tracing::info!("Scheduler startup: no overdue jobs to catch up");
        return;
    }

    tracing::info!(
        count = jobs.len(),
        "Scheduler startup: catching up overdue jobs"
    );

    process_due_jobs(config, security, jobs, SCHEDULER_COMPONENT, event_tx).await;

    tracing::info!("Scheduler startup: catch-up complete");
}

pub async fn execute_job_now(config: &Config, job: &CronJob) -> (bool, String) {
    let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);
    Box::pin(execute_job_with_retry(config, &security, job)).await
}

async fn execute_job_with_retry(
    config: &Config,
    security: &SecurityPolicy,
    job: &CronJob,
) -> (bool, String) {
    let mut last_output = String::new();
    let retries = config.reliability.scheduler_retries;
    let mut backoff_ms = config.reliability.provider_backoff_ms.max(200);

    for attempt in 0..=retries {
        let (success, output) = match job.job_type {
            JobType::Shell => run_job_command(config, security, job).await,
            JobType::Agent => Box::pin(run_agent_job(config, security, job)).await,
        };
        last_output = output;

        if success {
            return (true, last_output);
        }

        if last_output.starts_with("blocked by security policy:") {
            // Deterministic policy violations are not retryable.
            return (false, last_output);
        }

        if attempt < retries {
            let jitter_ms = u64::from(Utc::now().timestamp_subsec_millis() % 250);
            time::sleep(Duration::from_millis(backoff_ms + jitter_ms)).await;
            backoff_ms = (backoff_ms.saturating_mul(2)).min(30_000);
        }
    }

    (false, last_output)
}

async fn process_due_jobs(
    config: &Config,
    security: &Arc<SecurityPolicy>,
    jobs: Vec<CronJob>,
    component: &str,
    event_tx: &EventBroadcast,
) {
    // Refresh scheduler health on every successful poll cycle, including idle cycles.
    crate::health::mark_component_ok(component);

    let max_concurrent = config.scheduler.max_concurrent.max(1);
    let mut in_flight = stream::iter(jobs.into_iter().map(|job| {
        let config = config.clone();
        let security = Arc::clone(security);
        let component = component.to_owned();
        async move {
            Box::pin(execute_and_persist_job(
                &config,
                security.as_ref(),
                &job,
                &component,
            ))
            .await
        }
    }))
    .buffer_unordered(max_concurrent);

    while let Some((job_id, success, output)) = in_flight.next().await {
        if !success {
            tracing::warn!("Scheduler job '{job_id}' failed: {output}");
        }
        // Broadcast cron result to dashboard/SSE clients.
        if let Some(tx) = event_tx {
            let _ = tx.send(serde_json::json!({
                "type": "cron_result",
                "job_id": job_id,
                "success": success,
                "output": output,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }));
        }
    }
}

async fn execute_and_persist_job(
    config: &Config,
    security: &SecurityPolicy,
    job: &CronJob,
    component: &str,
) -> (String, bool, String) {
    crate::health::mark_component_ok(component);
    warn_if_high_frequency_agent_job(job);

    let started_at = Utc::now();
    let (success, output) = Box::pin(execute_job_with_retry(config, security, job)).await;
    let finished_at = Utc::now();
    let success = Box::pin(persist_job_result(
        config,
        job,
        success,
        &output,
        started_at,
        finished_at,
    ))
    .await;

    (job.id.clone(), success, output)
}

async fn run_agent_job(
    config: &Config,
    security: &SecurityPolicy,
    job: &CronJob,
) -> (bool, String) {
    if !security.can_act() {
        return (
            false,
            "blocked by security policy: autonomy is read-only".to_string(),
        );
    }

    if security.is_rate_limited() {
        return (
            false,
            "blocked by security policy: rate limit exceeded".to_string(),
        );
    }

    if !security.record_action() {
        return (
            false,
            "blocked by security policy: action budget exhausted".to_string(),
        );
    }
    let name = job.name.clone().unwrap_or_else(|| "cron-job".to_string());
    let prompt = job.prompt.clone().unwrap_or_default();

    // Recall relevant memories so cron jobs have context awareness.
    // Skipped when `job.uses_memory` is false (e.g. stateless digest jobs).
    // Exclude `Conversation` memories to prevent chat context from
    // leaking into scheduled executions (see #5415).
    let memory_context = if !job.uses_memory {
        String::new()
    } else {
        match zeroclaw_memory::create_memory(
            &config.memory,
            &config.workspace_dir,
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.api_key.as_deref()),
        ) {
            Ok(mem) => match mem.recall(&prompt, 5, None, None, None).await {
                Ok(entries) if !entries.is_empty() => {
                    let ctx: String = entries
                        .iter()
                        .filter(|e| {
                            !matches!(
                                e.category,
                                zeroclaw_memory::traits::MemoryCategory::Conversation
                            )
                        })
                        .map(|e| format!("- {}: {}", e.key, e.content))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if ctx.is_empty() {
                        String::new()
                    } else {
                        format!("[Memory context]\n{ctx}\n\n")
                    }
                }
                _ => String::new(),
            },
            Err(_) => String::new(),
        }
    };

    let prefixed_prompt = format!("{memory_context}[cron:{} {name}] {prompt}", job.id);
    let model_override = job.model.clone();

    let mut cron_config = config.clone();
    cron_config.memory.auto_save = false;

    // Assign a unique session ID so memories written during this run can be
    // purged atomically if the run fails (prevents snowball accumulation).
    let run_session_id = uuid::Uuid::new_v4().to_string();
    let session_path = std::path::PathBuf::from(format!("cron-{run_session_id}"));

    let run_result = match job.session_target {
        SessionTarget::Main | SessionTarget::Isolated => {
            Box::pin(crate::agent::run(
                cron_config,
                Some(prefixed_prompt),
                None,
                model_override,
                config
                    .providers
                    .fallback_provider()
                    .and_then(|e| e.temperature)
                    .unwrap_or(0.7),
                vec![],
                false,
                Some(session_path.clone()),
                job.allowed_tools.clone(),
            ))
            .await
        }
    };

    match run_result {
        Ok(response) => (
            true,
            if response.trim().is_empty() {
                "agent job executed".to_string()
            } else {
                response
            },
        ),
        Err(e) => {
            // Purge memories written during this failed run so they don't
            // pollute future recall and cause context snowball.
            let mem_session_key = format!("cli:{}", session_path.display());
            if let Ok(mem) = zeroclaw_memory::create_memory(
                &config.memory,
                &config.workspace_dir,
                config
                    .providers
                    .fallback_provider()
                    .and_then(|e| e.api_key.as_deref()),
            ) {
                let _ = mem.purge_session(&mem_session_key).await;
            }
            (false, format!("agent job failed: {e}"))
        }
    }
}

async fn persist_job_result(
    config: &Config,
    job: &CronJob,
    mut success: bool,
    output: &str,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
) -> bool {
    let duration_ms = (finished_at - started_at).num_milliseconds();

    if let Err(e) = deliver_if_configured(config, job, output).await {
        if job.delivery.best_effort {
            tracing::warn!("Cron delivery failed (best_effort): {e}");
        } else {
            success = false;
            tracing::warn!("Cron delivery failed: {e}");
        }
    }

    let _ = record_run(
        config,
        &job.id,
        started_at,
        finished_at,
        if success { "ok" } else { "error" },
        Some(output),
        duration_ms,
    );

    if is_one_shot_auto_delete(job) {
        if success {
            if let Err(e) = remove_job(config, &job.id) {
                tracing::warn!("Failed to remove one-shot cron job after success: {e}");
                // Fall back to disabling the job so it won't re-trigger.
                let _ = update_job(
                    config,
                    &job.id,
                    CronJobPatch {
                        enabled: Some(false),
                        ..CronJobPatch::default()
                    },
                );
            }
        } else {
            let _ = record_last_run(config, &job.id, finished_at, false, output);
            if let Err(e) = update_job(
                config,
                &job.id,
                CronJobPatch {
                    enabled: Some(false),
                    ..CronJobPatch::default()
                },
            ) {
                tracing::warn!("Failed to disable failed one-shot cron job: {e}");
            }
        }
        return success;
    }

    if let Err(e) = reschedule_after_run(config, job, success, output) {
        tracing::warn!("Failed to persist scheduler run result: {e}");
    }

    success
}

fn is_one_shot_auto_delete(job: &CronJob) -> bool {
    job.delete_after_run && matches!(job.schedule, Schedule::At { .. })
}

fn is_high_frequency_agent_job(job: &CronJob) -> bool {
    if !matches!(job.job_type, JobType::Agent) {
        return false;
    }
    match &job.schedule {
        Schedule::Every { every_ms } => *every_ms < 5 * 60 * 1000,
        Schedule::Cron { .. } => {
            let now = Utc::now();
            next_run_for_schedule(&job.schedule, now)
                .and_then(|a| next_run_for_schedule(&job.schedule, a).map(|b| (a, b)))
                .map(|(a, b)| (b - a).num_minutes() < 5)
                .unwrap_or(false)
        }
        Schedule::At { .. } => false,
    }
}

fn warn_if_high_frequency_agent_job(job: &CronJob) {
    if is_high_frequency_agent_job(job) {
        tracing::warn!(
            "Cron agent job '{}' is scheduled more frequently than every 5 minutes",
            job.id
        );
    }
}

async fn deliver_if_configured(config: &Config, job: &CronJob, output: &str) -> Result<()> {
    let delivery: &DeliveryConfig = &job.delivery;
    if !delivery.mode.eq_ignore_ascii_case("announce") {
        return Ok(());
    }

    let channel = delivery
        .channel
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("delivery.channel is required for announce mode"))?;
    let target = delivery
        .to
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("delivery.to is required for announce mode"))?;

    // Pre-flight readiness gate. For stateful channels (WhatsApp
    // Web, Matrix, Mattermost) the underlying client takes a few
    // seconds to a couple of minutes to connect after daemon
    // start; without this gate, a 09:00 cron tick that fires
    // mid-connect produces "client not connected" and (with
    // best_effort=true today) silently drops the brief.
    //
    // The gate is a no-op for stateless channels because the
    // default Channel::health_check trait impl returns true.
    //
    // max_wait_secs == 0 disables the gate entirely (escape
    // hatch for operators who would rather fail fast than wait).
    if delivery.max_wait_secs > 0 {
        match wait_for_channel_ready(channel, delivery.max_wait_secs, Duration::from_secs(1)).await
        {
            Ok(()) => {}
            Err(err) => {
                // Typed `DeliveryError::ChannelNotReady` -- record on
                // the `cron-delivery` health component so `zeroclaw
                // doctor` and the wiki posture surface can flag the
                // operator before they notice their morning brief is
                // missing. Piece D of
                // /tmp/tnm_brief_zeroclaw_cron_diagnosis_2026-05-02.md.
                crate::health::mark_component_error("cron-delivery", err.to_string());
                return Err(anyhow!("{err}"));
            }
        }
    }

    let result = deliver_announcement(config, channel, target, output).await;
    if result.is_ok() {
        crate::health::mark_component_ok("cron-delivery");
    } else if let Err(ref err) = result {
        crate::health::mark_component_error(
            "cron-delivery",
            format!("delivery to '{channel}' failed: {err}"),
        );
    }
    result
}

/// Typed error surface for delivery failures. Currently a single
/// variant -- `ChannelNotReady` -- but introduced as an enum so
/// piece D's posture-marker layer can pattern-match on the exact
/// failure class (e.g. "channel never connected" vs "channel
/// connected but send rejected") without grepping anyhow strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryError {
    /// The pre-flight readiness gate timed out: channel never
    /// reported `health_check() == true` within `waited_secs`.
    /// Carries enough context for the posture marker (piece D)
    /// to surface "WhatsApp not ready, briefs won't deliver"
    /// to the customer.
    ChannelNotReady { channel: String, waited_secs: u64 },
}

impl std::fmt::Display for DeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeliveryError::ChannelNotReady {
                channel,
                waited_secs,
            } => write!(
                f,
                "channel '{channel}' did not report ready within {waited_secs}s; \
                 briefs configured against this channel will not deliver until \
                 the channel reconnects"
            ),
        }
    }
}

impl std::error::Error for DeliveryError {}

/// Optional readiness probe registered alongside `DeliveryFn`.
/// The binary crate wires this to the channel orchestrator's
/// `channel_is_ready`, which consults the per-channel
/// `health_check`. When no fn is registered (e.g. in a test
/// harness or a daemon built without channels) the gate
/// short-circuits to "ready" -- callers that care about the
/// distinction can check via `is_readiness_fn_registered`.
pub type ReadinessFn = Box<
    dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        + Send
        + Sync,
>;

static READINESS_FN: std::sync::OnceLock<ReadinessFn> = std::sync::OnceLock::new();

/// Register the channel-readiness probe. Called once at startup
/// by the binary alongside `register_delivery_fn`.
pub fn register_readiness_fn(f: ReadinessFn) {
    let _ = READINESS_FN.set(f);
}

/// Whether a readiness fn is registered. Tests use this to
/// distinguish "gate active" from "no probe configured".
pub fn is_readiness_fn_registered() -> bool {
    READINESS_FN.get().is_some()
}

/// Public for the binary's wiring + tests; queries the
/// registered readiness probe. Returns `true` when no probe is
/// registered so a daemon built without channels still delivers
/// (rather than spinning the gate forever waiting for a probe
/// that will never fire).
pub async fn check_channel_readiness(channel: &str) -> bool {
    if let Some(f) = READINESS_FN.get() {
        f(channel.to_string()).await
    } else {
        true
    }
}

/// Generic polling loop. Public callers go through
/// `wait_for_channel_ready` which probes the global readiness fn;
/// tests pass their own closure to `wait_until_ready_with` to
/// exercise the timeout / becomes-ready paths without wrestling
/// with the OnceLock.
async fn wait_until_ready_with<F, Fut>(
    channel: &str,
    max_wait_secs: u64,
    tick: Duration,
    is_ready: F,
) -> std::result::Result<(), DeliveryError>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(max_wait_secs);
    loop {
        if is_ready(channel.to_string()).await {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(DeliveryError::ChannelNotReady {
                channel: channel.to_string(),
                waited_secs: max_wait_secs,
            });
        }
        tokio::time::sleep(tick).await;
    }
}

/// Poll the registered channel-readiness probe with `tick`
/// cadence until it reports ready or `max_wait_secs` elapses.
/// Returns `DeliveryError::ChannelNotReady` on timeout.
pub(crate) async fn wait_for_channel_ready(
    channel: &str,
    max_wait_secs: u64,
    tick: Duration,
) -> std::result::Result<(), DeliveryError> {
    wait_until_ready_with(channel, max_wait_secs, tick, |c| async move {
        check_channel_readiness(&c).await
    })
    .await
}

/// One-shot startup readiness sweep for the `cron-delivery`
/// component. After waiting `max(startup_grace_secs)` across all
/// enabled cron jobs with `mode = "announce"` and a configured
/// channel, probe each unique target channel via the registered
/// readiness fn and update the `cron-delivery` health component.
///
/// The per-tick pre-flight gate (piece C) handles transient
/// un-readiness on cron firing; this sweep specifically catches
/// "channels never came up at all" -- the failure mode where
/// `health_check()` keeps returning false because the underlying
/// client (e.g. WhatsApp Web) failed to connect, so cron ticks
/// time out indefinitely.
///
/// Spawned as a one-shot tokio task from the daemon `run` so it
/// does not block scheduler startup or hold up channel boot.
///
/// See piece D of
/// /tmp/tnm_brief_zeroclaw_cron_diagnosis_2026-05-02.md.
pub async fn run_startup_readiness_sweep(config: Config) {
    const COMPONENT: &str = "cron-delivery";

    let jobs = match crate::cron::list_jobs(&config) {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!("cron-delivery startup sweep: cannot enumerate jobs: {e}");
            return;
        }
    };

    let plan = build_sweep_plan(&jobs);
    if plan.is_empty() {
        // Nothing declarative to sweep -- mark ok so doctor doesn't
        // report "starting" forever for daemons without cron-delivery.
        crate::health::mark_component_ok(COMPONENT);
        return;
    }

    if plan.max_grace_secs > 0 {
        tokio::time::sleep(Duration::from_secs(plan.max_grace_secs)).await;
    }

    let not_ready =
        probe_sweep_plan(&plan, |c| async move { check_channel_readiness(&c).await }).await;

    if not_ready.is_empty() {
        crate::health::mark_component_ok(COMPONENT);
    } else {
        crate::health::mark_component_error(
            COMPONENT,
            format!(
                "channels not ready after {grace}s grace: {list}",
                grace = plan.max_grace_secs,
                list = not_ready.join(", "),
            ),
        );
    }
}

/// Per-channel sweep target. One entry per unique channel name;
/// the originating `job_id` is captured so the error message can
/// point the operator at the offending declarative job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SweepTarget {
    pub channel: String,
    pub job_id: String,
}

/// Output of `build_sweep_plan` -- the de-duplicated set of
/// channels to probe and the longest configured startup grace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SweepPlan {
    pub targets: Vec<SweepTarget>,
    pub max_grace_secs: u64,
}

impl SweepPlan {
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }
}

/// Build the de-duplicated sweep plan from a list of cron jobs.
/// Pulled out as a pure fn so unit tests can exercise the filter
/// + dedup + max-grace computation without sleeping or touching
/// the global readiness probe.
pub(crate) fn build_sweep_plan(jobs: &[CronJob]) -> SweepPlan {
    let mut targets: Vec<SweepTarget> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut max_grace: u64 = 0;

    for job in jobs {
        if !job.enabled {
            continue;
        }
        if !job.delivery.mode.eq_ignore_ascii_case("announce") {
            continue;
        }
        let Some(channel) = job
            .delivery
            .channel
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
        else {
            continue;
        };
        // Track grace across all candidates -- even duplicates,
        // since the operator may have configured per-job graces
        // and we want the longest before giving up.
        max_grace = max_grace.max(job.delivery.startup_grace_secs);
        if seen.insert(channel.to_string()) {
            targets.push(SweepTarget {
                channel: channel.to_string(),
                job_id: job.id.clone(),
            });
        }
    }

    SweepPlan {
        targets,
        max_grace_secs: max_grace,
    }
}

/// Probe each target in the plan with the supplied async fn.
/// Returns formatted "'channel' (job id)" strings for targets
/// whose probe returned false. Pulled out so tests can pass an
/// in-memory probe instead of the global OnceLock.
pub(crate) async fn probe_sweep_plan<F, Fut>(plan: &SweepPlan, is_ready: F) -> Vec<String>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let mut not_ready = Vec::new();
    for target in &plan.targets {
        if !is_ready(target.channel.clone()).await {
            not_ready.push(format!("'{}' (job {})", target.channel, target.job_id));
        }
    }
    not_ready
}

/// Delivery function type — takes owned values so the returned future is 'static.
pub type DeliveryFn = Box<
    dyn Fn(
            Config,
            String,
            String,
            String,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>
        + Send
        + Sync,
>;

/// Global delivery function, injected by the binary crate at startup.
static DELIVERY_FN: std::sync::OnceLock<DeliveryFn> = std::sync::OnceLock::new();

/// Register the channel delivery function. Called once at startup by the binary.
pub fn register_delivery_fn(f: DeliveryFn) {
    let _ = DELIVERY_FN.set(f);
}

pub async fn deliver_announcement(
    config: &Config,
    channel: &str,
    target: &str,
    output: &str,
) -> Result<()> {
    if let Some(f) = DELIVERY_FN.get() {
        f(
            config.clone(),
            channel.to_string(),
            target.to_string(),
            output.to_string(),
        )
        .await
    } else {
        // Returning `Ok(())` here would make `cron_run` report `status: "ok"`
        // for a job that never delivered — the caller has no way to
        // distinguish "executed + delivered" from "executed + silently
        // dropped". The existing `best_effort` flag on `CronDelivery` is the
        // operator-facing knob for "delivery failure is non-fatal", so we
        // return a real error and let that flag decide how to route it.
        tracing::warn!(
            channel = %channel,
            target = %target,
            "Cron delivery skipped: no delivery handler registered"
        );
        Err(anyhow!(
            "cron delivery handler not registered for channel '{channel}' \
             (register_delivery_fn was not called; set delivery.best_effort=true \
             on the job if this configuration is intentional)"
        ))
    }
}

async fn run_job_command(
    config: &Config,
    security: &SecurityPolicy,
    job: &CronJob,
) -> (bool, String) {
    run_job_command_with_timeout(
        config,
        security,
        job,
        Duration::from_secs(SHELL_JOB_TIMEOUT_SECS),
    )
    .await
}

async fn run_job_command_with_timeout(
    config: &Config,
    security: &SecurityPolicy,
    job: &CronJob,
    timeout: Duration,
) -> (bool, String) {
    if !security.can_act() {
        return (
            false,
            "blocked by security policy: autonomy is read-only".to_string(),
        );
    }

    if security.is_rate_limited() {
        return (
            false,
            "blocked by security policy: rate limit exceeded".to_string(),
        );
    }

    // Unified command validation: allowlist + risk + path checks in one call.
    // Jobs created via the validated helpers were already checked at creation
    // time, but we re-validate at execution time to catch policy changes and
    // manually-edited job stores.
    let approved = false; // scheduler runs are never pre-approved
    if let Err(error) =
        crate::cron::validate_shell_command_with_security(security, &job.command, approved)
    {
        return (false, error.to_string());
    }

    if let Some(path) = security.forbidden_path_argument(&job.command) {
        return (
            false,
            format!("blocked by security policy: forbidden path argument: {path}"),
        );
    }

    if !security.record_action() {
        return (
            false,
            "blocked by security policy: action budget exhausted".to_string(),
        );
    }

    let child = match build_cron_shell_command(&job.command, &config.workspace_dir) {
        Ok(mut cmd) => match cmd.spawn() {
            Ok(child) => child,
            Err(e) => return (false, format!("spawn error: {e}")),
        },
        Err(e) => return (false, format!("shell setup error: {e}")),
    };

    match time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = format!(
                "status={}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                stdout.trim(),
                stderr.trim()
            );
            (output.status.success(), combined)
        }
        Ok(Err(e)) => (false, format!("spawn error: {e}")),
        Err(_) => (
            false,
            format!("job timed out after {}s", timeout.as_secs_f64()),
        ),
    }
}

/// Build a shell `Command` for cron job execution.
///
/// Uses `sh -c <command>` (non-login shell). On Windows, ZeroClaw users
/// typically have Git Bash installed which provides `sh` in PATH, and
/// cron commands are written with Unix shell syntax. The previous `-lc`
/// (login shell) flag was dropped: login shells load the full user
/// profile on every invocation which is slow and may cause side effects.
///
/// The command is configured with:
/// - `current_dir` set to the workspace
/// - `stdin` piped to `/dev/null` (no interactive input)
/// - `stdout` and `stderr` piped for capture
/// - `kill_on_drop(true)` for safe timeout handling
fn build_cron_shell_command(
    command: &str,
    workspace_dir: &std::path::Path,
) -> anyhow::Result<Command> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(workspace_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron::{self, DeliveryConfig};
    use crate::security::SecurityPolicy;
    use chrono::{Duration as ChronoDuration, Utc};
    use tempfile::TempDir;
    use zeroclaw_config::schema::Config;

    async fn test_config(tmp: &TempDir) -> Config {
        let config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        tokio::fs::create_dir_all(&config.workspace_dir)
            .await
            .unwrap();
        config
    }

    fn test_job(command: &str) -> CronJob {
        CronJob {
            id: "test-job".into(),
            expression: "* * * * *".into(),
            schedule: crate::cron::Schedule::Cron {
                expr: "* * * * *".into(),
                tz: None,
            },
            command: command.into(),
            prompt: None,
            name: None,
            job_type: JobType::Shell,
            session_target: SessionTarget::Isolated,
            model: None,
            enabled: true,
            delivery: DeliveryConfig::default(),
            delete_after_run: false,
            allowed_tools: None,
            uses_memory: true,
            source: "imperative".into(),
            created_at: Utc::now(),
            next_run: Utc::now(),
            last_run: None,
            last_status: None,
            last_output: None,
        }
    }

    fn unique_component(prefix: &str) -> String {
        format!("{prefix}-{}", uuid::Uuid::new_v4())
    }

    fn agent_job_with_schedule(schedule: crate::cron::Schedule) -> CronJob {
        CronJob {
            job_type: JobType::Agent,
            schedule,
            ..test_job("echo test")
        }
    }

    #[test]
    fn high_frequency_daily_cron_is_not_flagged() {
        // `0 6 * * *` fires once per day — must never warn regardless of when the check runs
        let job = agent_job_with_schedule(crate::cron::Schedule::Cron {
            expr: "0 6 * * *".into(),
            tz: Some("America/Chicago".into()),
        });
        assert!(!is_high_frequency_agent_job(&job));
    }

    #[test]
    fn high_frequency_every_4min_cron_is_flagged() {
        let job = agent_job_with_schedule(crate::cron::Schedule::Cron {
            expr: "*/4 * * * *".into(),
            tz: None,
        });
        assert!(is_high_frequency_agent_job(&job));
    }

    #[test]
    fn high_frequency_every_5min_cron_is_not_flagged() {
        // Exactly 5 minutes is acceptable (threshold is strictly less than 5)
        let job = agent_job_with_schedule(crate::cron::Schedule::Cron {
            expr: "*/5 * * * *".into(),
            tz: None,
        });
        assert!(!is_high_frequency_agent_job(&job));
    }

    #[test]
    fn high_frequency_every_interval_below_threshold_is_flagged() {
        let job = agent_job_with_schedule(crate::cron::Schedule::Every {
            every_ms: 4 * 60 * 1000, // 4 minutes
        });
        assert!(is_high_frequency_agent_job(&job));
    }

    #[test]
    fn high_frequency_every_interval_at_threshold_is_not_flagged() {
        let job = agent_job_with_schedule(crate::cron::Schedule::Every {
            every_ms: 5 * 60 * 1000, // exactly 5 minutes
        });
        assert!(!is_high_frequency_agent_job(&job));
    }

    #[test]
    fn high_frequency_shell_job_is_never_flagged() {
        // Shell jobs are exempt regardless of frequency
        let job = CronJob {
            job_type: JobType::Shell,
            schedule: crate::cron::Schedule::Every {
                every_ms: 60 * 1000, // 1 minute
            },
            ..test_job("echo test")
        };
        assert!(!is_high_frequency_agent_job(&job));
    }

    #[tokio::test]
    async fn run_job_command_success() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("echo scheduler-ok");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(success);
        assert!(output.contains("scheduler-ok"));
        assert!(output.contains("status=exit status: 0"));
    }

    #[tokio::test]
    async fn run_job_command_failure() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("ls definitely_missing_file_for_scheduler_test");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("definitely_missing_file_for_scheduler_test"));
        assert!(output.contains("status=exit status:"));
    }

    #[tokio::test]
    async fn run_job_command_times_out() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.allowed_commands = vec!["sleep".into()];
        let job = test_job("sleep 1");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) =
            run_job_command_with_timeout(&config, &security, &job, Duration::from_millis(50)).await;
        assert!(!success);
        assert!(output.contains("job timed out after"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_disallowed_command() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.allowed_commands = vec!["echo".into()];
        let job = test_job("curl https://evil.example");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.to_lowercase().contains("not allowed"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_forbidden_path_argument() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.allowed_commands = vec!["cat".into()];
        let job = test_job("cat /etc/passwd");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("forbidden path argument"));
        assert!(output.contains("/etc/passwd"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_forbidden_option_assignment_path_argument() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.allowed_commands = vec!["grep".into()];
        let job = test_job("grep --file=/etc/passwd root ./src");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("forbidden path argument"));
        assert!(output.contains("/etc/passwd"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_forbidden_short_option_attached_path_argument() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.allowed_commands = vec!["grep".into()];
        let job = test_job("grep -f/etc/passwd root ./src");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("forbidden path argument"));
        assert!(output.contains("/etc/passwd"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_tilde_user_path_argument() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.allowed_commands = vec!["cat".into()];
        let job = test_job("cat ~root/.ssh/id_rsa");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("forbidden path argument"));
        assert!(output.contains("~root/.ssh/id_rsa"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_input_redirection_path_bypass() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.allowed_commands = vec!["cat".into()];
        let job = test_job("cat </etc/passwd");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.to_lowercase().contains("not allowed"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_readonly_mode() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.level = crate::security::AutonomyLevel::ReadOnly;
        let job = test_job("echo should-not-run");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("read-only"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_rate_limited() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.max_actions_per_hour = 0;
        let job = test_job("echo should-not-run");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("rate limit exceeded"));
    }

    #[tokio::test]
    async fn execute_job_with_retry_recovers_after_first_failure() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.reliability.scheduler_retries = 1;
        config.reliability.provider_backoff_ms = 1;
        config.autonomy.allowed_commands = vec!["sh".into()];
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        tokio::fs::write(
            config.workspace_dir.join("retry-once.sh"),
            "#!/bin/sh\nif [ -f retry-ok.flag ]; then\n  echo recovered\n  exit 0\nfi\ntouch retry-ok.flag\nexit 1\n",
        )
        .await
        .unwrap();
        let job = test_job("sh ./retry-once.sh");

        let (success, output) = Box::pin(execute_job_with_retry(&config, &security, &job)).await;
        assert!(success);
        assert!(output.contains("recovered"));
    }

    #[tokio::test]
    async fn execute_job_with_retry_exhausts_attempts() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.reliability.scheduler_retries = 1;
        config.reliability.provider_backoff_ms = 1;
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let job = test_job("ls always_missing_for_retry_test");

        let (success, output) = Box::pin(execute_job_with_retry(&config, &security, &job)).await;
        assert!(!success);
        assert!(output.contains("always_missing_for_retry_test"));
    }

    #[tokio::test]
    async fn run_agent_job_returns_error_without_provider_key() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.prompt = Some("Say hello".into());
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = Box::pin(run_agent_job(&config, &security, &job)).await;
        assert!(!success);
        assert!(output.contains("agent job failed:"));
    }

    #[tokio::test]
    async fn run_agent_job_blocks_readonly_mode() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.level = crate::security::AutonomyLevel::ReadOnly;
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.prompt = Some("Say hello".into());
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = Box::pin(run_agent_job(&config, &security, &job)).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("read-only"));
    }

    #[tokio::test]
    async fn run_agent_job_blocks_rate_limited() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.max_actions_per_hour = 0;
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.prompt = Some("Say hello".into());
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = Box::pin(run_agent_job(&config, &security, &job)).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("rate limit exceeded"));
    }

    #[tokio::test]
    async fn process_due_jobs_marks_component_ok_even_when_idle() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));
        let component = unique_component("scheduler-idle");

        crate::health::mark_component_error(&component, "pre-existing error");
        process_due_jobs(&config, &security, Vec::new(), &component, &None).await;

        let snapshot = crate::health::snapshot_json();
        let entry = &snapshot["components"][component.as_str()];
        assert_eq!(entry["status"], "ok");
        assert!(entry["last_ok"].as_str().is_some());
        assert!(entry["last_error"].is_null());
    }

    #[tokio::test]
    async fn process_due_jobs_failure_does_not_mark_component_unhealthy() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("ls definitely_missing_file_for_scheduler_component_health_test");
        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));
        let component = unique_component("scheduler-fail");

        crate::health::mark_component_ok(&component);
        process_due_jobs(&config, &security, vec![job], &component, &None).await;

        let snapshot = crate::health::snapshot_json();
        let entry = &snapshot["components"][component.as_str()];
        assert_eq!(entry["status"], "ok");
    }

    #[tokio::test]
    async fn persist_job_result_records_run_and_reschedules_shell_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = cron::add_job(&config, "*/5 * * * *", "echo ok").unwrap();
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;
        assert!(success);

        let runs = cron::list_runs(&config, &job.id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        let updated = cron::get_job(&config, &job.id).unwrap();
        assert_eq!(updated.last_status.as_deref(), Some("ok"));
    }

    #[tokio::test]
    async fn persist_job_result_success_deletes_one_shot() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let at = Utc::now() + ChronoDuration::minutes(10);
        let job = cron::add_agent_job(
            &config,
            Some("one-shot".into()),
            crate::cron::Schedule::At { at },
            "Hello",
            SessionTarget::Isolated,
            None,
            None,
            true,
            None,
        )
        .unwrap();
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;
        assert!(success);
        let lookup = cron::get_job(&config, &job.id);
        assert!(lookup.is_err());
    }

    #[tokio::test]
    async fn persist_job_result_failure_disables_one_shot() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let at = Utc::now() + ChronoDuration::minutes(10);
        let job = cron::add_agent_job(
            &config,
            Some("one-shot".into()),
            crate::cron::Schedule::At { at },
            "Hello",
            SessionTarget::Isolated,
            None,
            None,
            true,
            None,
        )
        .unwrap();
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, false, "boom", started, finished).await;
        assert!(!success);
        let updated = cron::get_job(&config, &job.id).unwrap();
        assert!(!updated.enabled);
        assert_eq!(updated.last_status.as_deref(), Some("error"));
    }

    #[tokio::test]
    async fn persist_job_result_success_deletes_one_shot_shell_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let at = Utc::now() + ChronoDuration::minutes(10);
        let job = cron::add_once_at(&config, at, "echo one-shot-shell").unwrap();
        assert!(job.delete_after_run);
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;
        assert!(success);
        let lookup = cron::get_job(&config, &job.id);
        assert!(lookup.is_err());
    }

    #[tokio::test]
    async fn persist_job_result_failure_disables_one_shot_shell_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let at = Utc::now() + ChronoDuration::minutes(10);
        let job = cron::add_once_at(&config, at, "echo one-shot-shell").unwrap();
        assert!(job.delete_after_run);
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, false, "boom", started, finished).await;
        assert!(!success);
        let updated = cron::get_job(&config, &job.id).unwrap();
        assert!(!updated.enabled);
        assert_eq!(updated.last_status.as_deref(), Some("error"));
    }

    #[tokio::test]
    async fn persist_job_result_delivery_stubbed_succeeds() {
        // Delivery is stubbed (moved to zeroclaw-channels orchestrator).
        // This test verifies the stub returns Ok, so persist_job_result succeeds.
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = cron::add_agent_job(
            &config,
            Some("announce-job".into()),
            crate::cron::Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "deliver this",
            SessionTarget::Isolated,
            None,
            Some(DeliveryConfig {
                mode: "announce".into(),
                channel: Some("telegram".into()),
                to: Some("123456".into()),
                best_effort: false,
                // 0 disables the readiness gate so this test
                // exercises the legacy delivery path without
                // waiting for a probe that nothing here registers.
                max_wait_secs: 0,
                startup_grace_secs: 0,
            }),
            false,
            None,
        )
        .unwrap();
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;
        assert!(success);

        let updated = cron::get_job(&config, &job.id).unwrap();
        assert!(updated.enabled);
        assert_eq!(updated.last_status.as_deref(), Some("ok"));

        let runs = cron::list_runs(&config, &job.id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "ok");
    }

    #[tokio::test]
    async fn persist_job_result_delivery_failure_best_effort_keeps_success() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = cron::add_agent_job(
            &config,
            Some("announce-job-best-effort".into()),
            crate::cron::Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "deliver this",
            SessionTarget::Isolated,
            None,
            Some(DeliveryConfig {
                mode: "announce".into(),
                channel: Some("telegram".into()),
                to: Some("123456".into()),
                best_effort: true,
                max_wait_secs: 0,
                startup_grace_secs: 0,
            }),
            false,
            None,
        )
        .unwrap();
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;
        assert!(success);

        let updated = cron::get_job(&config, &job.id).unwrap();
        assert!(updated.enabled);
        assert_eq!(updated.last_status.as_deref(), Some("ok"));

        let runs = cron::list_runs(&config, &job.id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "ok");
    }

    #[tokio::test]
    async fn persist_job_result_at_schedule_without_delete_after_run_is_disabled() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let at = Utc::now() + ChronoDuration::minutes(10);
        let job = cron::add_agent_job(
            &config,
            Some("at-no-autodelete".into()),
            crate::cron::Schedule::At { at },
            "Hello",
            SessionTarget::Isolated,
            None,
            None,
            false,
            None,
        )
        .unwrap();
        assert!(!job.delete_after_run);

        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);
        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;
        assert!(success);

        // After reschedule_after_run, At schedule jobs should be disabled
        // to prevent re-execution with a past next_run timestamp.
        let updated = cron::get_job(&config, &job.id).unwrap();
        assert!(
            !updated.enabled,
            "At schedule job should be disabled after execution via reschedule"
        );
        assert_eq!(updated.last_status.as_deref(), Some("ok"));
    }

    #[tokio::test]
    async fn deliver_if_configured_handles_none_mode() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("echo ok");

        // Default delivery mode is not "announce", so should be a no-op.
        assert!(deliver_if_configured(&config, &job, "x").await.is_ok());
    }

    #[tokio::test]
    async fn deliver_if_configured_announce_returns_err_when_no_handler() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let mut job = test_job("echo ok");
        job.delivery = DeliveryConfig {
            mode: "announce".into(),
            channel: Some("telegram".into()),
            to: Some("123456".into()),
            best_effort: true,
            max_wait_secs: 0,
            startup_grace_secs: 0,
        };

        // `best_effort` controls whether cron_run surfaces this as a failed
        // run — but `deliver_announcement` itself must return an error when
        // no delivery handler is registered so callers can distinguish
        // "executed + delivered" from "executed + silently dropped".
        let err = deliver_if_configured(&config, &job, "x")
            .await
            .expect_err("expected Err when DELIVERY_FN is not registered");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("delivery handler not registered"),
            "error message should explain the missing handler: {msg}"
        );
    }

    #[tokio::test]
    async fn deliver_announcement_errors_with_unregistered_handler() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let err = deliver_announcement(&config, "telegram", "chat-id", "payload")
            .await
            .expect_err("expected Err when DELIVERY_FN is not registered");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("delivery handler not registered"),
            "error must clearly state the missing handler: {msg}"
        );
        assert!(
            msg.contains("telegram"),
            "error must reference the requested channel: {msg}"
        );
    }

    #[test]
    fn build_cron_shell_command_uses_sh_non_login() {
        let workspace = std::env::temp_dir();
        let cmd = build_cron_shell_command("echo cron-test", &workspace).unwrap();
        let debug = format!("{cmd:?}");
        assert!(debug.contains("echo cron-test"));
        assert!(debug.contains("\"sh\""), "should use sh: {debug}");
        // Must NOT use login shell (-l) — login shells load full profile
        // and are slow/unpredictable for cron jobs.
        assert!(
            !debug.contains("\"-lc\""),
            "must not use login shell: {debug}"
        );
    }

    #[tokio::test]
    async fn build_cron_shell_command_executes_successfully() {
        let workspace = std::env::temp_dir();
        let mut cmd = build_cron_shell_command("echo cron-ok", &workspace).unwrap();
        let output = cmd.output().await.unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("cron-ok"));
    }

    #[tokio::test]
    async fn catch_up_queries_all_overdue_jobs_ignoring_max_tasks() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.scheduler.max_tasks = 1; // limit normal polling to 1

        // Create 3 jobs with "every minute" schedule
        for i in 0..3 {
            let _ = cron::add_job(&config, "* * * * *", &format!("echo catchup-{i}")).unwrap();
        }

        // Verify normal due_jobs is limited to max_tasks=1
        let far_future = Utc::now() + ChronoDuration::days(1);
        let due = cron::due_jobs(&config, far_future).unwrap();
        assert_eq!(due.len(), 1, "due_jobs must respect max_tasks");

        // all_overdue_jobs ignores the limit
        let overdue = cron::all_overdue_jobs(&config, far_future).unwrap();
        assert_eq!(overdue.len(), 3, "all_overdue_jobs must return all");
    }

    // scan_and_redact_output tests moved to zeroclaw-channels orchestrator

    // ── Broadcast / EventBroadcast tests ─────────────────────────────

    #[tokio::test]
    async fn broadcast_sends_cron_result_on_success() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("echo broadcast-ok");
        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));
        let component = unique_component("broadcast-ok");

        let (tx, mut rx) = tokio::sync::broadcast::channel::<serde_json::Value>(16);
        let event_tx: EventBroadcast = Some(tx);

        process_due_jobs(&config, &security, vec![job], &component, &event_tx).await;

        let event = rx.try_recv().expect("should receive a broadcast event");
        assert_eq!(event["type"], "cron_result");
        assert_eq!(event["job_id"], "test-job");
        assert_eq!(event["success"], true);
        assert!(event["output"].as_str().unwrap().contains("broadcast-ok"));
        assert!(event["timestamp"].as_str().is_some());
    }

    #[tokio::test]
    async fn broadcast_sends_cron_result_on_failure() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("ls definitely_missing_file_for_broadcast_fail_test");
        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));
        let component = unique_component("broadcast-fail");

        let (tx, mut rx) = tokio::sync::broadcast::channel::<serde_json::Value>(16);
        let event_tx: EventBroadcast = Some(tx);

        process_due_jobs(&config, &security, vec![job], &component, &event_tx).await;

        let event = rx.try_recv().expect("should receive a broadcast event");
        assert_eq!(event["type"], "cron_result");
        assert_eq!(event["job_id"], "test-job");
        assert_eq!(event["success"], false);
        assert!(event["timestamp"].as_str().is_some());
    }

    #[tokio::test]
    async fn broadcast_none_skips_without_error() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("echo no-broadcast");
        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));
        let component = unique_component("broadcast-none");

        // event_tx = None — should complete without panic.
        process_due_jobs(&config, &security, vec![job], &component, &None).await;
    }

    #[tokio::test]
    async fn broadcast_handles_no_subscribers() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("echo no-subscribers");
        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));
        let component = unique_component("broadcast-no-sub");

        let (tx, _) = tokio::sync::broadcast::channel::<serde_json::Value>(16);
        // Drop the only receiver immediately — `let _ = tx.send(...)` in
        // process_due_jobs must not panic when there are no subscribers.
        let event_tx: EventBroadcast = Some(tx);

        process_due_jobs(&config, &security, vec![job], &component, &event_tx).await;
        // If we got here without panic, the test passes.
    }

    // ── Pre-flight readiness gate (piece C) ─────────────────
    //
    // The gate is exercised through `wait_until_ready_with`,
    // which is the test-friendly variant of
    // `wait_for_channel_ready` -- it takes the readiness probe
    // as a closure rather than going through the global
    // `READINESS_FN` OnceLock. Tests assert the three reachable
    // outcomes: ready immediately, becomes ready during the
    // wait window, never becomes ready (typed
    // `DeliveryError::ChannelNotReady`).

    #[tokio::test]
    async fn pre_flight_returns_immediately_when_channel_ready() {
        // Real-time runtime with a 10ms tick keeps tests fast
        // without needing tokio/test-util's paused-time.
        let result = wait_until_ready_with(
            "stub-channel",
            120,
            Duration::from_millis(10),
            |_| async move { true },
        )
        .await;

        assert!(result.is_ok(), "ready probe should yield Ok immediately");
    }

    #[tokio::test]
    async fn pre_flight_succeeds_when_channel_becomes_ready_mid_wait() {
        // Probe returns false 3 times, then true. Exercises the
        // bounded retry path -- the workhorse of piece C.
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts_clone = attempts.clone();
        let result =
            wait_until_ready_with("stub-channel", 120, Duration::from_millis(10), move |_| {
                let attempts = attempts_clone.clone();
                async move {
                    let n = attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    n >= 3
                }
            })
            .await;

        assert!(result.is_ok(), "becomes-ready probe should yield Ok");
        // 3 not-ready probes (n=0,1,2) + 1 ready probe (n=3) = 4
        // total invocations.
        assert_eq!(
            attempts.load(std::sync::atomic::Ordering::SeqCst),
            4,
            "should have polled 4 times (3 not-ready + 1 ready)"
        );
    }

    #[tokio::test]
    async fn pre_flight_returns_typed_error_after_timeout() {
        let result = wait_until_ready_with(
            "stuck-channel",
            2,
            Duration::from_millis(10),
            |_| async move { false },
        )
        .await;

        match result {
            Err(DeliveryError::ChannelNotReady {
                channel,
                waited_secs,
            }) => {
                assert_eq!(channel, "stuck-channel");
                assert_eq!(waited_secs, 2);
            }
            other => panic!("expected DeliveryError::ChannelNotReady; got {other:?}"),
        }
    }

    #[tokio::test]
    async fn check_channel_readiness_is_optimistic_when_no_fn_registered() {
        // In a daemon built without the channels feature (or in
        // a test harness) no readiness fn is registered. The
        // gate should NOT spin forever -- it should treat
        // "no probe" as "assume ready" so delivery proceeds.
        // Note: this test relies on the fact that a daemon
        // running this test binary never registers a readiness
        // fn, so the OnceLock stays empty.
        if !is_readiness_fn_registered() {
            assert!(
                check_channel_readiness("anything").await,
                "no-fn case must return true"
            );
        }
    }

    #[test]
    fn delivery_error_display_includes_channel_and_wait() {
        let err = DeliveryError::ChannelNotReady {
            channel: "whatsapp".into(),
            waited_secs: 120,
        };
        let msg = format!("{err}");
        assert!(msg.contains("whatsapp"));
        assert!(msg.contains("120"));
        // Customer-facing wording: must read like a posture
        // marker would, since piece D will surface it via
        // zeroclaw doctor.
        assert!(msg.contains("did not report ready"));
    }

    #[test]
    fn default_delivery_config_has_120s_wait() {
        let d = DeliveryConfig::default();
        assert_eq!(d.max_wait_secs, 120);
    }

    #[test]
    fn default_delivery_config_has_60s_startup_grace() {
        let d = DeliveryConfig::default();
        assert_eq!(d.startup_grace_secs, 60);
    }

    // ── Sweep-plan helpers (piece D) ───────────────────────────

    fn announce_job(
        id: &str,
        channel: Option<&str>,
        enabled: bool,
        startup_grace_secs: u64,
    ) -> CronJob {
        let mut job = test_job("echo");
        job.id = id.into();
        job.enabled = enabled;
        job.delivery = DeliveryConfig {
            mode: "announce".into(),
            channel: channel.map(Into::into),
            to: Some("123456".into()),
            best_effort: true,
            max_wait_secs: 0,
            startup_grace_secs,
        };
        job
    }

    #[test]
    fn build_sweep_plan_filters_disabled_and_non_announce() {
        let jobs = vec![
            announce_job("a", Some("whatsapp"), true, 60),
            announce_job("b", Some("telegram"), false, 60), // disabled
            {
                // mode = "none" should be filtered
                let mut j = test_job("echo");
                j.delivery.mode = "none".into();
                j.delivery.channel = Some("matrix".into());
                j
            },
        ];
        let plan = build_sweep_plan(&jobs);
        assert_eq!(plan.targets.len(), 1);
        assert_eq!(plan.targets[0].channel, "whatsapp");
        assert_eq!(plan.targets[0].job_id, "a");
    }

    #[test]
    fn build_sweep_plan_dedups_by_channel() {
        let jobs = vec![
            announce_job("morning-brief", Some("whatsapp"), true, 60),
            announce_job("evening-brief", Some("whatsapp"), true, 90),
            announce_job("ops-channel", Some("telegram"), true, 30),
        ];
        let plan = build_sweep_plan(&jobs);
        assert_eq!(plan.targets.len(), 2);
        // Channel is deduplicated; first job seen wins for the
        // captured job_id.
        let whatsapp = plan
            .targets
            .iter()
            .find(|t| t.channel == "whatsapp")
            .unwrap();
        assert_eq!(whatsapp.job_id, "morning-brief");
    }

    #[test]
    fn build_sweep_plan_returns_max_grace_across_jobs() {
        let jobs = vec![
            announce_job("a", Some("whatsapp"), true, 30),
            announce_job("b", Some("telegram"), true, 120),
            announce_job("c", Some("matrix"), true, 60),
        ];
        let plan = build_sweep_plan(&jobs);
        assert_eq!(plan.max_grace_secs, 120);
    }

    #[test]
    fn build_sweep_plan_skips_blank_or_missing_channels() {
        let jobs = vec![
            announce_job("a", None, true, 60),
            announce_job("b", Some(""), true, 60),
            announce_job("c", Some("   "), true, 60),
            announce_job("d", Some("discord"), true, 60),
        ];
        let plan = build_sweep_plan(&jobs);
        assert_eq!(plan.targets.len(), 1);
        assert_eq!(plan.targets[0].channel, "discord");
    }

    #[test]
    fn build_sweep_plan_empty_when_no_announce_jobs() {
        let jobs = vec![test_job("echo a"), test_job("echo b")];
        let plan = build_sweep_plan(&jobs);
        assert!(plan.is_empty());
    }

    #[tokio::test]
    async fn probe_sweep_plan_collects_only_unready_channels() {
        let plan = SweepPlan {
            targets: vec![
                SweepTarget {
                    channel: "whatsapp".into(),
                    job_id: "morning-brief".into(),
                },
                SweepTarget {
                    channel: "telegram".into(),
                    job_id: "ops".into(),
                },
                SweepTarget {
                    channel: "matrix".into(),
                    job_id: "alerts".into(),
                },
            ],
            max_grace_secs: 60,
        };
        let not_ready = probe_sweep_plan(&plan, |c| async move {
            // Only telegram is "ready" in this scenario.
            c == "telegram"
        })
        .await;
        assert_eq!(not_ready.len(), 2);
        assert!(not_ready.iter().any(|s| s.contains("whatsapp")));
        assert!(not_ready.iter().any(|s| s.contains("morning-brief")));
        assert!(not_ready.iter().any(|s| s.contains("matrix")));
        assert!(not_ready.iter().any(|s| s.contains("alerts")));
        assert!(!not_ready.iter().any(|s| s.contains("telegram")));
    }

    // ── best_effort warn-on-load (piece E) ─────────────────────

    fn declarative_announce_job(id: &str, best_effort: bool) -> CronJobDecl {
        CronJobDecl {
            id: id.into(),
            name: None,
            job_type: "shell".into(),
            schedule: CronScheduleDecl::Cron {
                expr: "0 9 * * *".into(),
                tz: None,
            },
            command: Some("echo brief".into()),
            prompt: None,
            enabled: true,
            model: None,
            allowed_tools: None,
            uses_memory: true,
            session_target: None,
            delivery: Some(zeroclaw_config::schema::DeliveryConfigDecl {
                mode: "announce".into(),
                channel: Some("whatsapp".into()),
                to: Some("123456".into()),
                best_effort,
                max_wait_secs: 120,
                startup_grace_secs: 60,
            }),
        }
    }

    #[test]
    fn warn_on_best_effort_returns_ids_for_explicit_true() {
        let jobs = vec![
            declarative_announce_job("morning-brief", true),
            declarative_announce_job("evening-brief", false),
            declarative_announce_job("ops-alerts", true),
        ];
        let warned = warn_on_best_effort_declarative_jobs(&jobs);
        assert_eq!(warned.len(), 2);
        assert!(warned.contains(&"morning-brief".into()));
        assert!(warned.contains(&"ops-alerts".into()));
        assert!(!warned.contains(&"evening-brief".into()));
    }

    #[test]
    fn warn_on_best_effort_silent_when_all_false() {
        let jobs = vec![
            declarative_announce_job("a", false),
            declarative_announce_job("b", false),
        ];
        let warned = warn_on_best_effort_declarative_jobs(&jobs);
        assert!(warned.is_empty());
    }

    #[test]
    fn warn_on_best_effort_skips_non_announce() {
        let mut job = declarative_announce_job("backup", true);
        if let Some(d) = job.delivery.as_mut() {
            d.mode = "none".into();
        }
        let warned = warn_on_best_effort_declarative_jobs(&[job]);
        assert!(warned.is_empty());
    }

    #[test]
    fn warn_on_best_effort_skips_jobs_without_delivery() {
        let mut job = declarative_announce_job("backup", true);
        job.delivery = None;
        let warned = warn_on_best_effort_declarative_jobs(&[job]);
        assert!(warned.is_empty());
    }

    #[test]
    fn declarative_best_effort_default_is_false() {
        // Regression check: piece E flipped the schema default
        // from true to false. New operators get fail-loud unless
        // they opt in to best_effort = true explicitly.
        assert!(!zeroclaw_config::schema::default_best_effort_decl());
    }

    #[test]
    fn declarative_delivery_toml_without_best_effort_defaults_to_false() {
        // End-to-end TOML deserialisation: confirm the serde
        // default kicks in and operators opting into best_effort
        // must do so explicitly.
        let toml_src = r#"
            mode = "announce"
            channel = "whatsapp"
            to = "123456"
        "#;
        let decl: zeroclaw_config::schema::DeliveryConfigDecl = toml::from_str(toml_src).unwrap();
        assert!(!decl.best_effort);
        // And explicit true still parses as true (Andy's
        // existing config keeps its semantic).
        let toml_explicit = r#"
            mode = "announce"
            channel = "whatsapp"
            to = "123456"
            best_effort = true
        "#;
        let decl: zeroclaw_config::schema::DeliveryConfigDecl =
            toml::from_str(toml_explicit).unwrap();
        assert!(decl.best_effort);
    }

    #[tokio::test]
    async fn probe_sweep_plan_empty_when_all_ready() {
        let plan = SweepPlan {
            targets: vec![SweepTarget {
                channel: "whatsapp".into(),
                job_id: "morning-brief".into(),
            }],
            max_grace_secs: 60,
        };
        let not_ready = probe_sweep_plan(&plan, |_| async move { true }).await;
        assert!(not_ready.is_empty());
    }
}
