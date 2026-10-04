//! `lunarwing routines` — manage scheduled routines from the CLI.
//!
//! Provides subcommands for listing, creating, editing, enabling/disabling,
//! deleting, and viewing run history of routines without starting the full agent.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use clap::Subcommand;
use uuid::Uuid;

use crate::agent::routine::{
    MAX_TOOL_ROUNDS_LIMIT, NotifyConfig, Routine, RoutineAction, RoutineGuardrails, RunStatus,
    Trigger, next_cron_fire,
};
use crate::config::RoutineConfig;
use crate::db::Database;

/// Routines subcommands.
#[derive(Subcommand, Debug, Clone)]
pub enum RoutinesCommand {
    /// List routines
    List {
        /// Filter by trigger type (e.g. "cron", "webhook", "event")
        #[arg(long)]
        trigger: Option<String>,

        /// Include disabled routines
        #[arg(long)]
        disabled: bool,

        /// Output as JSON (for scripting)
        #[arg(long)]
        json: bool,
    },

    /// Create a new cron routine
    #[command(alias = "add")]
    Create {
        /// Routine name (must be unique per user)
        #[arg(long)]
        name: String,

        /// Cron schedule (6-field: "sec min hour day month weekday")
        #[arg(long)]
        schedule: String,

        /// Prompt for the LLM
        #[arg(long)]
        prompt: String,

        /// Optional description
        #[arg(long, default_value = "")]
        description: String,

        /// IANA timezone (e.g. "America/New_York")
        #[arg(long)]
        timezone: Option<String>,

        /// Cooldown between fires in seconds
        #[arg(long, default_value = "300")]
        cooldown: u64,

        /// Notification channel
        #[arg(long)]
        notify_channel: Option<String>,
    },

    /// Edit an existing routine
    #[command(alias = "update")]
    Edit {
        /// Routine name
        #[arg(long)]
        name: String,

        /// New schedule
        #[arg(long)]
        schedule: Option<String>,

        /// New prompt
        #[arg(long)]
        prompt: Option<String>,

        /// New description
        #[arg(long)]
        description: Option<String>,

        /// New timezone
        #[arg(long)]
        timezone: Option<String>,

        /// New cooldown in seconds
        #[arg(long)]
        cooldown: Option<u64>,

        /// New tool-calling rounds per run for a lightweight routine (1-10);
        /// runs still stay within ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS
        #[arg(long)]
        max_tool_rounds: Option<u32>,
    },

    /// Enable a routine
    Enable {
        /// Routine name
        name: String,
    },

    /// Disable a routine
    Disable {
        /// Routine name
        name: String,
    },

    /// Delete a routine
    #[command(alias = "rm")]
    Delete {
        /// Routine name
        name: String,

        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },

    /// Show run history for a routine
    #[command(alias = "runs")]
    History {
        /// Routine name
        name: String,

        /// Maximum number of runs to show
        #[arg(short, long, default_value = "10")]
        limit: i64,

        /// Output as JSON (for scripting)
        #[arg(long)]
        json: bool,
    },
}

/// Run a routines CLI command against the database.
pub async fn run_routines_command(
    cmd: RoutinesCommand,
    db: Arc<dyn Database>,
    user_id: &str,
) -> anyhow::Result<()> {
    match cmd {
        RoutinesCommand::List {
            trigger,
            disabled,
            json,
        } => list(&db, user_id, trigger.as_deref(), disabled, json).await,
        RoutinesCommand::Create {
            name,
            schedule,
            prompt,
            description,
            timezone,
            cooldown,
            notify_channel,
        } => {
            create(
                &db,
                user_id,
                &name,
                &schedule,
                &prompt,
                &description,
                timezone.as_deref(),
                cooldown,
                notify_channel,
            )
            .await
        }
        RoutinesCommand::Edit {
            name,
            schedule,
            prompt,
            description,
            timezone,
            cooldown,
            max_tool_rounds,
        } => {
            let edits = RoutineEdits {
                schedule: schedule.as_deref(),
                prompt: prompt.as_deref(),
                description: description.as_deref(),
                timezone: timezone.as_deref(),
                cooldown,
                max_tool_rounds,
            };
            edit(&db, user_id, &name, edits).await
        }
        RoutinesCommand::Enable { name } => set_enabled(&db, user_id, &name, true).await,
        RoutinesCommand::Disable { name } => set_enabled(&db, user_id, &name, false).await,
        RoutinesCommand::Delete { name, yes } => delete(&db, user_id, &name, yes).await,
        RoutinesCommand::History { name, limit, json } => {
            history(&db, user_id, &name, limit, json).await
        }
    }
}

// ── List ────────────────────────────────────────────────────

async fn list(
    db: &Arc<dyn Database>,
    user_id: &str,
    trigger_filter: Option<&str>,
    show_disabled: bool,
    json: bool,
) -> anyhow::Result<()> {
    let routines = db.list_routines(user_id).await?;

    let filtered: Vec<&Routine> = routines
        .iter()
        .filter(|r| {
            trigger_filter
                .map(|t| r.trigger.type_tag() == t)
                .unwrap_or(true)
        })
        .filter(|r| show_disabled || r.enabled)
        .collect();

    if json {
        let items: Vec<serde_json::Value> = filtered
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id.to_string(),
                    "name": r.name,
                    "trigger": r.trigger.type_tag(),
                    "enabled": r.enabled,
                    "next_fire_at": r.next_fire_at,
                    "last_run_at": r.last_run_at,
                    "run_count": r.run_count,
                    "consecutive_failures": r.consecutive_failures,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }

    if filtered.is_empty() {
        if let Some(t) = trigger_filter {
            println!("No {t} routines found.");
        } else {
            println!("No routines found.");
        }
        return Ok(());
    }

    // Header
    println!(
        "{:<36}  {:<20}  {:<8}  {:<8}  {:<22}  {:<22}  {:>5}",
        "ID", "NAME", "TRIGGER", "STATUS", "NEXT FIRE", "LAST RUN", "RUNS"
    );
    println!("{}", "-".repeat(130));

    // Fetch last-run status for all routines in a single batch query
    let routine_ids: Vec<Uuid> = filtered.iter().map(|r| r.id).collect();
    let last_run_results = db
        .batch_get_last_run_status(&routine_ids)
        .await
        .unwrap_or_default();

    for r in &filtered {
        let last_run_status = last_run_results.get(&r.id).copied();

        let status = if !r.enabled {
            "disabled".to_string()
        } else if last_run_status == Some(RunStatus::Running) {
            "running".to_string()
        } else if r.consecutive_failures > 0 {
            format!("err({})", r.consecutive_failures)
        } else if last_run_status == Some(RunStatus::Attention) {
            "attention".to_string()
        } else {
            "active".to_string()
        };

        let next_fire = r
            .next_fire_at
            .map(format_relative)
            .unwrap_or_else(|| "-".to_string());

        let last_run = r
            .last_run_at
            .map(format_relative)
            .unwrap_or_else(|| "-".to_string());

        let name = truncate(&r.name, 20);

        println!(
            "{:<36}  {:<20}  {:<8}  {:<8}  {:<22}  {:<22}  {:>5}",
            r.id,
            name,
            r.trigger.type_tag(),
            status,
            next_fire,
            last_run,
            r.run_count,
        );
    }

    println!("\n{} routine(s)", filtered.len());
    Ok(())
}

// ── Create ──────────────────────────────────────────────────

fn cli_notify_config(notify_channel: Option<String>) -> NotifyConfig {
    NotifyConfig {
        channel: notify_channel,
        user: None,
        on_attention: true,
        on_failure: true,
        on_success: false,
    }
}

#[allow(clippy::too_many_arguments)]
async fn create(
    db: &Arc<dyn Database>,
    user_id: &str,
    name: &str,
    schedule: &str,
    prompt: &str,
    description: &str,
    timezone: Option<&str>,
    cooldown_secs: u64,
    notify_channel: Option<String>,
) -> anyhow::Result<()> {
    validate_timezone_arg(timezone)?;

    // Validate the cron expression by computing next fire.
    let next_fire = next_cron_fire(schedule, timezone)
        .map_err(|e| anyhow::anyhow!("Invalid cron schedule: {e}"))?;

    // Check for name conflict.
    if db.get_routine_by_name(user_id, name).await?.is_some() {
        anyhow::bail!("Routine '{}' already exists", name);
    }

    let now = Utc::now();
    let routine = Routine {
        id: Uuid::new_v4(),
        name: name.to_string(),
        description: description.to_string(),
        user_id: user_id.to_string(),
        enabled: true,
        trigger: Trigger::Cron {
            schedule: schedule.to_string(),
            timezone: timezone.map(String::from),
        },
        action: RoutineAction::Lightweight {
            prompt: prompt.to_string(),
            context_paths: Vec::new(),
            max_tokens: 4096,
            use_tools: true,
            max_tool_rounds: 3,
        },
        guardrails: RoutineGuardrails {
            cooldown: std::time::Duration::from_secs(cooldown_secs),
            max_concurrent: 1,
            dedup_window: None,
            retry: Default::default(),
        },
        notify: cli_notify_config(notify_channel),
        last_run_at: None,
        next_fire_at: next_fire,
        run_count: 0,
        consecutive_failures: 0,
        state: serde_json::json!({}),
        created_at: now,
        updated_at: now,
    };

    db.create_routine(&routine).await?;

    println!("Created routine '{}'", name);
    println!("  ID:        {}", routine.id);
    println!("  Schedule:  {}", schedule);
    if let Some(tz) = timezone {
        println!("  Timezone:  {}", tz);
    }
    if let Some(nf) = next_fire {
        println!("  Next fire: {}", format_relative(nf));
    }
    Ok(())
}

// ── Edit ────────────────────────────────────────────────────

/// The changes one `routines edit` asks for; `None` leaves a field as it is.
struct RoutineEdits<'a> {
    schedule: Option<&'a str>,
    prompt: Option<&'a str>,
    description: Option<&'a str>,
    timezone: Option<&'a str>,
    cooldown: Option<u64>,
    max_tool_rounds: Option<u32>,
}

async fn edit(
    db: &Arc<dyn Database>,
    user_id: &str,
    name: &str,
    edits: RoutineEdits<'_>,
) -> anyhow::Result<()> {
    let mut routine = require_routine(db, user_id, name).await?;
    let timezone = edits.timezone;
    validate_timezone_arg(timezone)?;

    let mut changed = false;

    // Update schedule if provided (only valid for cron routines).
    if let Some(new_schedule) = edits.schedule {
        let tz = timezone.or(match &routine.trigger {
            Trigger::Cron { timezone, .. } => timezone.as_deref(),
            _ => None,
        });
        let next_fire = next_cron_fire(new_schedule, tz)
            .map_err(|e| anyhow::anyhow!("Invalid cron schedule: {e}"))?;
        routine.trigger = Trigger::Cron {
            schedule: new_schedule.to_string(),
            timezone: tz.map(String::from),
        };
        routine.next_fire_at = next_fire;
        changed = true;
    } else if let Some(tz) = timezone {
        // Update only timezone, recompute next fire with existing schedule.
        if let Trigger::Cron { ref schedule, .. } = routine.trigger {
            let next_fire = next_cron_fire(schedule, Some(tz))
                .map_err(|e| anyhow::anyhow!("Invalid cron schedule: {e}"))?;
            routine.trigger = Trigger::Cron {
                schedule: schedule.clone(),
                timezone: Some(tz.to_string()),
            };
            routine.next_fire_at = next_fire;
            changed = true;
        } else {
            anyhow::bail!("Cannot set timezone on non-cron trigger");
        }
    }

    if let Some(new_prompt) = edits.prompt {
        match &mut routine.action {
            RoutineAction::Lightweight { prompt: p, .. } => {
                *p = new_prompt.to_string();
                changed = true;
            }
            RoutineAction::FullJob { description: d, .. } => {
                *d = new_prompt.to_string();
                changed = true;
            }
        }
    }

    if let Some(new_desc) = edits.description {
        routine.description = new_desc.to_string();
        changed = true;
    }

    if let Some(cd) = edits.cooldown {
        routine.guardrails.cooldown = std::time::Duration::from_secs(cd);
        changed = true;
    }

    // Checked before anything is saved, so a refused value changes nothing.
    if let Some(rounds) = edits.max_tool_rounds {
        apply_max_tool_rounds(&mut routine.action, rounds)?;
        changed = true;
    }

    if !changed {
        println!("No changes specified.");
        return Ok(());
    }

    routine.updated_at = Utc::now();
    db.update_routine(&routine).await?;
    println!("Updated routine '{}'", name);
    if let Some(rounds) = edits.max_tool_rounds {
        print_max_tool_rounds_notes(&routine.action, rounds);
    }
    Ok(())
}

/// Set a lightweight routine's tool-round budget (WI#291). Unlike
/// `routine_update`, an out-of-range value is refused rather than clamped, so
/// the operator sees the real ceiling.
fn apply_max_tool_rounds(action: &mut RoutineAction, rounds: u32) -> anyhow::Result<()> {
    if !(1..=MAX_TOOL_ROUNDS_LIMIT).contains(&rounds) {
        anyhow::bail!("--max-tool-rounds must be between 1 and {MAX_TOOL_ROUNDS_LIMIT}");
    }
    if action.set_max_tool_rounds(u64::from(rounds)).is_none() {
        anyhow::bail!("--max-tool-rounds applies only to lightweight routines");
    }
    Ok(())
}

/// Settings that keep a new `max_tool_rounds` from taking effect. `config` is
/// the routine config as this process resolves it from the environment the
/// daemon also reads (`None` when it cannot be resolved).
fn max_tool_rounds_notes(
    rounds: u32,
    use_tools: bool,
    config: Option<&RoutineConfig>,
) -> Vec<String> {
    let mut notes = Vec::new();
    if !use_tools {
        notes.push(
            "this routine runs without tools (use_tools=false), so max_tool_rounds has no effect"
                .to_string(),
        );
    }
    if let Some(config) = config {
        if !config.lightweight_tools_enabled {
            notes.push(
                "ROUTINES_LIGHTWEIGHT_TOOLS=false disables tools for every lightweight routine, \
                 so max_tool_rounds has no effect until it is enabled and the daemon restarted"
                    .to_string(),
            );
        }
        let cap = config.lightweight_max_iterations;
        if cap < rounds {
            notes.push(format!(
                "runs stay within ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS={cap}; raise it and \
                 restart the daemon to use {rounds} rounds"
            ));
        }
    }
    notes
}

/// Print the notes for a `--max-tool-rounds` edit. The routine config is
/// resolved only here, so the other edits never depend on it.
fn print_max_tool_rounds_notes(action: &RoutineAction, rounds: u32) {
    let RoutineAction::Lightweight { use_tools, .. } = action else {
        return;
    };
    let config = RoutineConfig::resolve().ok();
    for note in max_tool_rounds_notes(rounds, *use_tools, config.as_ref()) {
        println!("Note: {note}");
    }
}

// ── Enable / Disable ────────────────────────────────────────

async fn set_enabled(
    db: &Arc<dyn Database>,
    user_id: &str,
    name: &str,
    enabled: bool,
) -> anyhow::Result<()> {
    let mut routine = require_routine(db, user_id, name).await?;

    if routine.enabled == enabled {
        println!(
            "Routine '{}' is already {}",
            name,
            if enabled { "enabled" } else { "disabled" }
        );
        return Ok(());
    }

    routine.enabled = enabled;

    // Recompute next fire when enabling a cron routine.
    if enabled
        && let Trigger::Cron {
            ref schedule,
            ref timezone,
        } = routine.trigger
    {
        routine.next_fire_at = next_cron_fire(schedule, timezone.as_deref())
            .map_err(|e| anyhow::anyhow!("Failed to compute next fire for stored schedule: {e}"))?;
    }

    routine.updated_at = Utc::now();
    db.update_routine(&routine).await?;
    println!(
        "{} routine '{}'",
        if enabled { "Enabled" } else { "Disabled" },
        name
    );
    Ok(())
}

// ── Delete ──────────────────────────────────────────────────

async fn delete(
    db: &Arc<dyn Database>,
    user_id: &str,
    name: &str,
    skip_confirm: bool,
) -> anyhow::Result<()> {
    let routine = require_routine(db, user_id, name).await?;

    if !skip_confirm {
        println!("Routine: {}", routine.name);
        println!("      ID: {}", routine.id);
        println!(" Trigger: {}", routine.trigger.type_tag());
        if let Trigger::Cron { ref schedule, .. } = routine.trigger {
            println!("Schedule: {}", schedule);
        }
        println!("   Runs: {}", routine.run_count);
        print!("\nDelete this routine? [y/N] ");
        std::io::Write::flush(&mut std::io::stdout())?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let deleted = db.delete_routine(routine.id).await?;
    if deleted {
        println!("Deleted routine '{}'", name);
    } else {
        anyhow::bail!("Failed to delete routine '{}'", name);
    }
    Ok(())
}

// ── History ─────────────────────────────────────────────────

async fn history(
    db: &Arc<dyn Database>,
    user_id: &str,
    name: &str,
    limit: i64,
    json: bool,
) -> anyhow::Result<()> {
    let routine = require_routine(db, user_id, name).await?;

    let limit = limit.clamp(1, 50);
    let runs = db.list_routine_runs(routine.id, limit).await?;

    if json {
        let items: Vec<serde_json::Value> = runs
            .iter()
            .map(|run| {
                serde_json::json!({
                    "id": run.id.to_string(),
                    "status": run.status.to_string(),
                    "started_at": run.started_at,
                    "completed_at": run.completed_at,
                    "result_summary": run.result_summary,
                    "tokens_used": run.tokens_used,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }

    if runs.is_empty() {
        println!("No runs found for routine '{}'", name);
        return Ok(());
    }

    println!("Run history for '{}' (last {}):\n", name, runs.len());

    println!(
        "{:<36}  {:<8}  {:<20}  {:<12}  SUMMARY",
        "RUN ID", "STATUS", "STARTED", "DURATION"
    );
    println!("{}", "-".repeat(100));

    for run in &runs {
        let duration = run
            .completed_at
            .map(|end| {
                let secs = (end - run.started_at).num_seconds();
                if secs < 60 {
                    format!("{}s", secs)
                } else {
                    format!("{}m{}s", secs / 60, secs % 60)
                }
            })
            .unwrap_or_else(|| "running".to_string());

        let summary = run
            .result_summary
            .as_deref()
            .map(|s| truncate(s, 40))
            .unwrap_or_else(|| "-".to_string());

        println!(
            "{:<36}  {:<8}  {:<20}  {:<12}  {}",
            run.id,
            run.status,
            run.started_at.format("%Y-%m-%d %H:%M:%S"),
            duration,
            summary,
        );
    }

    println!("\n{} run(s) shown", runs.len());
    Ok(())
}

// ── Shared lookup ────────────────────────────────────────────

/// Look up a routine by name.
async fn require_routine(
    db: &Arc<dyn Database>,
    user_id: &str,
    name: &str,
) -> anyhow::Result<Routine> {
    db.get_routine_by_name(user_id, name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Routine '{}' not found", name))
}

fn validate_timezone_arg(timezone: Option<&str>) -> anyhow::Result<()> {
    if let Some(tz) = timezone
        && crate::timezone::parse_timezone(tz).is_none()
    {
        anyhow::bail!("Invalid timezone: '{tz}' is not a valid IANA timezone");
    }
    Ok(())
}

// ── Helpers ─────────────────────────────────────────────────

/// Format a datetime relative to now (e.g. "in 2h", "3m ago").
fn format_relative(dt: DateTime<Utc>) -> String {
    let now = Utc::now();
    let diff = dt.signed_duration_since(now);
    let secs = diff.num_seconds();

    if secs.abs() < 60 {
        if secs >= 0 {
            "in <1m".to_string()
        } else {
            "<1m ago".to_string()
        }
    } else if secs.abs() < 3600 {
        let mins = secs.abs() / 60;
        if secs >= 0 {
            format!("in {}m", mins)
        } else {
            format!("{}m ago", mins)
        }
    } else if secs.abs() < 86400 {
        let hours = secs.abs() / 3600;
        if secs >= 0 {
            format!("in {}h", hours)
        } else {
            format!("{}h ago", hours)
        }
    } else {
        let days = secs.abs() / 86400;
        if secs >= 0 {
            format!("in {}d", days)
        } else {
            format!("{}d ago", days)
        }
    }
}

/// Truncate a string to a maximum character length.
fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars.saturating_sub(2)).collect();
        format!("{}..", truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_relative_future() {
        let future = Utc::now() + chrono::Duration::hours(2);
        let result = format_relative(future);
        assert!(
            result.starts_with("in "),
            "expected 'in ...' for future time, got: {result}"
        );
    }

    #[test]
    fn format_relative_past() {
        let past = Utc::now() - chrono::Duration::minutes(30);
        let result = format_relative(past);
        assert!(
            result.ends_with(" ago"),
            "expected '... ago' for past time, got: {result}"
        );
    }

    #[test]
    fn format_relative_days() {
        let far_future = Utc::now() + chrono::Duration::days(3);
        let result = format_relative(far_future);
        assert!(result.contains('d'), "expected days in: {result}");
    }

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string() {
        let result = truncate("hello world", 7);
        assert_eq!(result, "hello..");
    }

    #[test]
    fn truncate_multibyte_safe() {
        // Ensure no panic on multi-byte characters.
        let cjk = "你好世界测试";
        let result = truncate(cjk, 4);
        assert!(result.ends_with(".."), "got: {result}");
        // Must be valid UTF-8 (would have panicked otherwise).
        assert!(result.is_char_boundary(result.len()));
    }

    #[test]
    fn cli_notify_config_defaults_to_runtime_target_resolution() {
        let notify = cli_notify_config(Some("xmpp".to_string()));
        assert_eq!(notify.channel.as_deref(), Some("xmpp")); // safety: test-only assertion
        assert_eq!(notify.user, None); // safety: test-only assertion
        assert!(notify.on_attention); // safety: test-only assertion
        assert!(notify.on_failure); // safety: test-only assertion
        assert!(!notify.on_success); // safety: test-only assertion
    }

    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn cli_create_defaults_lightweight_routines_to_tools_enabled() {
        let harness = crate::testing::TestHarnessBuilder::new().build().await;
        let db = harness.db.clone();

        run_routines_command(
            RoutinesCommand::Create {
                name: "cli-digest".to_string(),
                schedule: "0 0 9 * * *".to_string(),
                prompt: "Prepare the morning digest.".to_string(),
                description: "CLI created routine".to_string(),
                timezone: Some("UTC".to_string()),
                cooldown: 300,
                notify_channel: None,
            },
            db.clone(),
            "user1",
        )
        .await
        .expect("create routine");

        let routine = db
            .get_routine_by_name("user1", "cli-digest")
            .await
            .expect("get routine by name")
            .expect("cli-digest should exist");

        match routine.action {
            RoutineAction::Lightweight {
                use_tools,
                max_tool_rounds,
                ..
            } => {
                assert!(
                    use_tools,
                    "CLI-created lightweight routines should default to tools"
                );
                assert_eq!(max_tool_rounds, 3);
            }
            other => panic!("expected lightweight action, got {other:?}"),
        }
    }

    #[test]
    fn max_tool_rounds_notes_flag_settings_that_make_the_budget_moot() {
        let config = |cap: u32, tools: bool| RoutineConfig {
            lightweight_max_iterations: cap,
            lightweight_tools_enabled: tools,
            ..RoutineConfig::default()
        };

        let capped = max_tool_rounds_notes(6, true, Some(&config(3, true)));
        assert_eq!(capped.len(), 1, "{capped:?}");
        assert!(
            capped
                .iter()
                .all(|note| note.contains("ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS=3")),
            "{capped:?}"
        );

        let fits = max_tool_rounds_notes(3, true, Some(&config(6, true)));
        assert!(fits.is_empty(), "{fits:?}");

        let no_tools = max_tool_rounds_notes(5, false, None);
        assert_eq!(no_tools.len(), 1, "{no_tools:?}");
        assert!(
            no_tools.iter().all(|note| note.contains("use_tools=false")),
            "{no_tools:?}"
        );

        let tools_off = max_tool_rounds_notes(5, true, Some(&config(6, false)));
        assert_eq!(tools_off.len(), 1, "{tools_off:?}");
        assert!(
            tools_off
                .iter()
                .all(|note| note.contains("ROUTINES_LIGHTWEIGHT_TOOLS=false")),
            "{tools_off:?}"
        );
    }

    /// `routines edit --max-tool-rounds` is the per-routine budget surface
    /// that works on every engine (WI#291 review R2).
    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn cli_edit_sets_lightweight_max_tool_rounds() {
        let harness = crate::testing::TestHarnessBuilder::new().build().await;
        let db = harness.db.clone();
        run_routines_command(
            RoutinesCommand::Create {
                name: "cli-digest".to_string(),
                schedule: "0 0 9 * * *".to_string(),
                prompt: "Prepare the morning digest.".to_string(),
                description: "CLI created routine".to_string(),
                timezone: Some("UTC".to_string()),
                cooldown: 300,
                notify_channel: None,
            },
            db.clone(),
            "user1",
        )
        .await
        .expect("create routine");

        let edit_rounds = |name: &str, rounds: u32| RoutinesCommand::Edit {
            name: name.to_string(),
            schedule: None,
            prompt: None,
            description: None,
            timezone: None,
            cooldown: None,
            max_tool_rounds: Some(rounds),
        };
        let stored_rounds = |routine: Option<Routine>| match routine.map(|r| r.action) {
            Some(RoutineAction::Lightweight {
                max_tool_rounds, ..
            }) => max_tool_rounds,
            other => panic!("expected the lightweight cli-digest, got {other:?}"),
        };

        run_routines_command(edit_rounds("cli-digest", 6), db.clone(), "user1")
            .await
            .expect("edit max_tool_rounds");
        let routine = db.get_routine_by_name("user1", "cli-digest").await;
        assert_eq!(stored_rounds(routine.expect("get routine")), 6);

        for out_of_range in [11, 0] {
            let err =
                run_routines_command(edit_rounds("cli-digest", out_of_range), db.clone(), "user1")
                    .await
                    .expect_err("out-of-range budgets are refused, not clamped");
            assert!(err.to_string().contains("between 1 and 10"), "{err}");
        }
        let routine = db.get_routine_by_name("user1", "cli-digest").await;
        assert_eq!(
            stored_rounds(routine.expect("get routine")),
            6,
            "a refused value must not be saved"
        );

        let now = Utc::now();
        let full_job = Routine {
            id: Uuid::new_v4(),
            name: "cli-full-job".to_string(),
            description: "Full-job routine".to_string(),
            user_id: "user1".to_string(),
            enabled: true,
            trigger: Trigger::Manual,
            action: RoutineAction::FullJob {
                title: "Review".to_string(),
                description: "Review pending changes".to_string(),
                max_iterations: 5,
            },
            guardrails: RoutineGuardrails {
                cooldown: std::time::Duration::from_secs(0),
                max_concurrent: 1,
                dedup_window: None,
                retry: Default::default(),
            },
            notify: NotifyConfig::default(),
            last_run_at: None,
            next_fire_at: None,
            run_count: 0,
            consecutive_failures: 0,
            state: serde_json::json!({}),
            created_at: now,
            updated_at: now,
        };
        db.create_routine(&full_job).await.expect("create full job");
        let err = run_routines_command(edit_rounds("cli-full-job", 5), db.clone(), "user1")
            .await
            .expect_err("full_job routines have no tool-round budget");
        assert!(
            err.to_string().contains("only to lightweight routines"),
            "{err}"
        );
    }
}
