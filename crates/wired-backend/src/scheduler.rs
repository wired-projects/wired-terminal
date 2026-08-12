//! Running schedules, and getting their results somewhere they will be seen.
//!
//! A scheduled task is an ordinary message into the ordinary session — the same
//! path a phone message takes. What is different is the delivery: the live
//! relay is held back for the duration of the run and one labelled summary is
//! sent at the end, so a task that fires at 3am arrives as a single readable
//! message rather than forty streamed fragments. A run that has nothing to
//! report sends nothing at all.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{Local, TimeZone};
use tokio::sync::broadcast::error::RecvError;

use crate::agent_io;
use crate::gateway::{Hub, SILENT};
use crate::paths;
use crate::recorder::EventKind;
use crate::schedule::Schedule;

/// How often the runner looks for work. Schedules are minute-grained, so this
/// is fine enough, and a sleeping laptop wakes to a late run rather than a
/// missed one.
const TICK: Duration = Duration::from_secs(20);
/// A run is given this long to produce something before we stop listening.
const RUN_TIMEOUT: Duration = Duration::from_secs(300);
/// …and is considered finished after this much quiet.
const RUN_IDLE: Duration = Duration::from_secs(6);
/// Enough to read on a phone; the full exchange is in the transcript.
const RESULT_CHARS: usize = 3000;

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn as_local(ts: f64) -> chrono::DateTime<Local> {
    Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .unwrap_or_else(Local::now)
}

#[derive(Clone)]
pub struct Scheduler {
    inner: Arc<Inner>,
}

struct Inner {
    schedules: Mutex<Vec<Schedule>>,
    persist: bool,
    /// The id currently executing. One PTY means one task at a time.
    active: Mutex<Option<String>>,
}

impl Scheduler {
    pub fn new(persist: bool) -> Self {
        let schedules = if persist {
            read_from_disk()
        } else {
            Vec::new()
        };
        Self {
            inner: Arc::new(Inner {
                schedules: Mutex::new(schedules),
                persist,
                active: Mutex::new(None),
            }),
        }
    }

    pub fn list(&self) -> Vec<Schedule> {
        self.inner.schedules.lock().unwrap().clone()
    }

    pub fn active(&self) -> Option<String> {
        self.inner.active.lock().unwrap().clone()
    }

    /// Create or replace a schedule. Rejects a `when` it cannot parse, with a
    /// message that shows what would have worked.
    pub fn upsert(&self, mut schedule: Schedule) -> Result<Schedule, String> {
        if schedule.task.trim().is_empty() {
            return Err("Say what the assistant should do.".into());
        }
        let trigger = schedule.trigger()?;
        if schedule.name.trim().is_empty() {
            schedule.name = summarise(&schedule.task);
        }
        if schedule.id.trim().is_empty() {
            schedule.id = new_id();
        }
        schedule.next_run = schedule
            .enabled
            .then(|| trigger.next_after(Local::now()))
            .flatten()
            .map(|at| at.timestamp() as f64);

        let mut schedules = self.inner.schedules.lock().unwrap();
        match schedules.iter().position(|s| s.id == schedule.id) {
            Some(index) => {
                // Keep the history: editing the wording of a task should not
                // erase what it reported yesterday.
                schedule.last_run = schedules[index].last_run;
                schedule.last_result = schedules[index].last_result.clone();
                schedules[index] = schedule.clone();
            }
            None => schedules.push(schedule.clone()),
        }
        let snapshot = schedules.clone();
        drop(schedules);
        self.save(&snapshot);
        Ok(schedule)
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        let mut schedules = self.inner.schedules.lock().unwrap();
        let before = schedules.len();
        schedules.retain(|s| s.id != id);
        if schedules.len() == before {
            return Err("No schedule with that id.".into());
        }
        let snapshot = schedules.clone();
        drop(schedules);
        self.save(&snapshot);
        Ok(())
    }

    fn save(&self, schedules: &[Schedule]) {
        if !self.inner.persist {
            return;
        }
        let Ok(body) = serde_json::to_string_pretty(schedules) else {
            return;
        };
        if let Err(e) = paths::write_private(&paths::schedules_file(), &body) {
            tracing::warn!("could not save schedules: {e}");
        }
    }

    fn record_result(&self, id: &str, result: String) {
        let mut schedules = self.inner.schedules.lock().unwrap();
        if let Some(schedule) = schedules.iter_mut().find(|s| s.id == id) {
            schedule.last_run = Some(now());
            schedule.last_result = Some(result);
            schedule.next_run = schedule
                .enabled
                .then(|| {
                    schedule
                        .trigger()
                        .ok()
                        .and_then(|t| t.next_after(Local::now()))
                })
                .flatten()
                .map(|at| at.timestamp() as f64);
        }
        let snapshot = schedules.clone();
        drop(schedules);
        self.save(&snapshot);
    }

    /// Fire one schedule immediately, whatever its timetable says. This is the
    /// "try it" button — a schedule you cannot test is a schedule you do not
    /// trust.
    pub async fn run_once(&self, hub: &Hub, id: &str) -> Result<String, String> {
        let schedule = self
            .list()
            .into_iter()
            .find(|s| s.id == id)
            .ok_or("No schedule with that id.")?;
        let result = self.execute(hub, &schedule).await?;
        Ok(result)
    }

    async fn execute(&self, hub: &Hub, schedule: &Schedule) -> Result<String, String> {
        {
            let mut active = self.inner.active.lock().unwrap();
            if active.is_some() {
                return Err("Another scheduled task is still running.".into());
            }
            *active = Some(schedule.id.clone());
        }

        let outcome = self.execute_inner(hub, schedule).await;
        *self.inner.active.lock().unwrap() = None;

        let text = match &outcome {
            Ok(result) => result.clone(),
            Err(e) => format!("Didn't run: {e}"),
        };
        self.record_result(&schedule.id, text);
        outcome
    }

    async fn execute_inner(&self, hub: &Hub, schedule: &Schedule) -> Result<String, String> {
        tracing::info!(name = schedule.name, "running scheduled task");

        // Everything the agent says during the run stays off the phone; one
        // summary goes out at the end instead. Approval prompts are exempt —
        // suppressing those would leave the agent blocked with nobody asked.
        let hold = hub.gateway.hold_relay();
        let mut events = hub.recorder.subscribe();

        let prompt = if schedule.quiet_when_nothing {
            format!(
                "{}\n\nIf there is nothing worth reporting, reply with exactly {SILENT} \
                 and nothing else.",
                schedule.task
            )
        } else {
            schedule.task.clone()
        };

        agent_io::say(&hub.manager, &hub.assistant, &prompt, true).await?;

        let started = std::time::Instant::now();
        let mut collected = String::new();
        loop {
            if started.elapsed() > RUN_TIMEOUT {
                break;
            }
            match tokio::time::timeout(RUN_IDLE, events.recv()).await {
                Ok(Ok(event)) => match event.kind {
                    EventKind::Text | EventKind::Notice => {
                        if !collected.is_empty() {
                            collected.push('\n');
                        }
                        collected.push_str(&event.text);
                    }
                    // The agent stopped to ask something; it is not going to
                    // finish on its own, and the prompt already went out.
                    EventKind::Prompt => break,
                    _ => continue,
                },
                Ok(Err(RecvError::Lagged(_))) => continue,
                Ok(Err(RecvError::Closed)) => break,
                // Quiet for long enough: the agent has finished.
                Err(_) => break,
            }
        }
        drop(hold);

        let result = trim_result(&collected);
        if result.trim() == SILENT || result.trim().is_empty() {
            tracing::info!(name = schedule.name, "scheduled task had nothing to report");
            return Ok(SILENT.to_string());
        }

        hub.recorder.system(format!("⏰ {} — done", schedule.name));
        if let Err(e) = hub
            .gateway
            .notify_owner(&format!("⏰ {}\n\n{result}", schedule.name))
            .await
        {
            // Not fatal: the result is in the transcript and on the schedule
            // screen either way.
            tracing::debug!("scheduled result not delivered to chat: {e}");
        }
        Ok(result)
    }

    /// The tick loop. One of these per process, started by `serve`.
    pub fn watch(self, hub: Hub) {
        tokio::spawn(async move {
            // Give a schedule whose time passed while the machine was asleep a
            // next_run in the future rather than an instant backlog.
            self.reschedule_stale();

            loop {
                tokio::time::sleep(TICK).await;
                let due: Vec<Schedule> = self
                    .list()
                    .into_iter()
                    .filter(|s| s.enabled && s.next_run.is_some_and(|at| at <= now()))
                    .collect();

                for schedule in due {
                    if let Err(e) = self.execute(&hub, &schedule).await {
                        tracing::warn!(name = schedule.name, "scheduled task failed: {e}");
                    }
                }
            }
        });
    }

    /// After downtime, a missed run fires once — now — rather than once per
    /// interval that elapsed.
    fn reschedule_stale(&self) {
        let mut schedules = self.inner.schedules.lock().unwrap();
        let stale = now() - 3600.0;
        for schedule in schedules.iter_mut() {
            let overdue = schedule.next_run.is_none_or(|at| at < stale);
            if schedule.enabled && overdue {
                schedule.next_run = schedule
                    .trigger()
                    .ok()
                    .and_then(|t| t.next_after(Local::now()))
                    .map(|at| at.timestamp() as f64);
            }
        }
        let snapshot = schedules.clone();
        drop(schedules);
        self.save(&snapshot);
    }
}

fn read_from_disk() -> Vec<Schedule> {
    let path = paths::schedules_file();
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    match serde_json::from_str(&raw) {
        Ok(schedules) => schedules,
        Err(e) => {
            tracing::error!("{} is not valid JSON ({e}); ignoring it", path.display());
            Vec::new()
        }
    }
}

fn new_id() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    (0..10)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect()
}

/// A name for a schedule the user did not name, taken from the task itself.
fn summarise(task: &str) -> String {
    let first_line = task.lines().next().unwrap_or(task).trim();
    let mut name: String = first_line.chars().take(48).collect();
    if first_line.chars().count() > 48 {
        name.push('…');
    }
    name
}

/// Keep the end: the conclusion of a run matters more than its preamble.
fn trim_result(text: &str) -> String {
    let text = text.trim();
    let count = text.chars().count();
    if count <= RESULT_CHARS {
        return text.to_string();
    }
    let tail: String = text.chars().skip(count - RESULT_CHARS).collect();
    format!("…{tail}")
}

/// Human "when" for the schedule list, e.g. "today at 18:00".
pub fn describe_when(ts: f64) -> String {
    let at = as_local(ts);
    let today = Local::now().date_naive();
    let day = at.date_naive();
    if day == today {
        format!("today at {}", at.format("%H:%M"))
    } else if day == today.succ_opt().unwrap_or(today) {
        format!("tomorrow at {}", at.format("%H:%M"))
    } else {
        at.format("%a %-d %b at %H:%M").to_string()
    }
}
