//! Spike: a babashka pod that runs Claude Code or Codex turns.
//!
//! See issue #41. Each agent is one provider and one conversation; each turn
//! is one cancellable streaming run of the provider's CLI through
//! claude-wrapper or codex-wrapper, resuming the conversation after the first
//! turn. Turns run as tasks on a tokio runtime the pod owns, so `tell` returns
//! at once and the script polls, waits on, watches or cancels the turn.
//!
//! The binary calls [`shutdown`] after [`Pod::run`](babashka_pod::Pod::run)
//! returns, because background turns are not pod calls and pod shutdown does
//! not reach them.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use babashka_pod::serde_json::{self, Value, json};
use babashka_pod::transit::Keyword;
use babashka_pod::{Emitter, Handle, Handles, Namespace, PodError};
use serde::Deserialize;
use tokio::sync::{oneshot, watch};

/// The namespace this pod defines.
pub const NAMESPACE: &str = "pod.joshrotenberg.agent";

/// `ask` and `result` are stream vars underneath, so their handlers get an
/// [`Emitter`] and see pod shutdown. These wrappers return the one value.
const ASK: &str = r#"(defn ask
  "Runs one turn of agent with prompt and blocks until it ends. Returns the
  result map. opts: :output-schema (a JSON schema map; the parsed answer is
  under :data), :timeout-ms."
  ([agent prompt] (ask agent prompt nil))
  ([agent prompt opts]
   (let [r (volatile! nil)]
     (-ask agent prompt opts #(vreset! r %))
     @r)))"#;

const TELL: &str = r#"(defn tell
  "Starts one turn of agent with prompt and returns its handle at once. opts
  as for ask."
  ([agent prompt] (-tell agent prompt nil))
  ([agent prompt opts] (-tell agent prompt opts)))"#;

const RESULT: &str = r#"(defn result
  "Blocks until turn ends and returns its result map, or :timeout after
  timeout-ms."
  ([turn] (result turn nil))
  ([turn timeout-ms]
   (let [r (volatile! nil)]
     (-result turn timeout-ms #(vreset! r %))
     @r)))"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Provider {
    Claude,
    Codex,
}

impl Provider {
    fn keyword(self) -> Keyword {
        Keyword::new(match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
        })
    }
}

/// `create`'s options. Keywords arrive as their names.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Opts {
    provider: Provider,
    model: Option<String>,
    cwd: Option<String>,
    /// Claude only.
    system_prompt: Option<String>,
    /// Claude only.
    append_system_prompt: Option<String>,
    /// Claude `acceptEdits`, Codex `workspace-write`. Read-only otherwise.
    #[serde(default)]
    allow_edits: bool,
    /// Claude only; Codex reports no cost.
    max_budget_usd: Option<f64>,
    timeout_ms: Option<u64>,
    #[serde(default)]
    env: HashMap<String, String>,
    /// An existing session or thread id to continue.
    session: Option<String>,
}

/// Options for one turn, given to `ask` or `tell`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct TurnOpts {
    /// A JSON schema; the parsed answer goes under `:data`.
    output_schema: Option<Value>,
    /// Overrides the agent's `:timeout-ms` for this turn.
    timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Running,
    Done,
    Failed,
    Cancelled,
}

impl Status {
    fn keyword(self) -> Keyword {
        Keyword::new(match self {
            Status::Running => "running",
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Cancelled => "cancelled",
        })
    }
}

struct TurnState {
    status: Status,
    events: Vec<Value>,
    result: Option<Value>,
}

/// One turn: its events as they arrive, and its result once it ends.
struct Turn {
    state: Mutex<TurnState>,
    changed: Condvar,
    cancel: Mutex<Option<oneshot::Sender<()>>>,
}

impl Turn {
    fn new(cancel: oneshot::Sender<()>) -> Self {
        Turn {
            state: Mutex::new(TurnState {
                status: Status::Running,
                events: Vec::new(),
                result: None,
            }),
            changed: Condvar::new(),
            cancel: Mutex::new(Some(cancel)),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TurnState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn status(&self) -> Status {
        self.lock().status
    }

    fn push(&self, event: Value) {
        self.lock().events.push(event);
        self.changed.notify_all();
    }

    fn finish(&self, status: Status, result: Value) {
        {
            let mut s = self.lock();
            s.status = status;
            s.result = Some(result);
        }
        self.changed.notify_all();
    }

    fn cancel(&self) {
        let tx = self.cancel.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(tx) = tx {
            let _ = tx.send(());
        }
    }

    /// Wakes anyone waiting, so they can check their own stop condition.
    fn poke(&self) {
        self.changed.notify_all();
    }

    /// The result once the turn ends, or `None` at `deadline` or when `stop`
    /// says so.
    fn wait(&self, deadline: Option<Instant>, stop: &dyn Fn() -> bool) -> Option<Value> {
        let mut s = self.lock();
        loop {
            if let Some(r) = &s.result {
                return Some(r.clone());
            }
            if stop() {
                return None;
            }
            let wait = match deadline {
                Some(d) => d.checked_duration_since(Instant::now())?,
                None => Duration::from_secs(3600),
            };
            s = self
                .changed
                .wait_timeout(s, wait)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

struct AgentState {
    session: Option<String>,
    current: Option<Arc<Turn>>,
}

struct Agent {
    opts: Opts,
    state: Arc<Mutex<AgentState>>,
}

// ---------------------------------------------------------------------------
// Runtime and shutdown
// ---------------------------------------------------------------------------

fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("pod-agent")
            .build()
            .expect("tokio runtime")
    })
}

fn shutdown_signal() -> &'static watch::Sender<bool> {
    static TX: OnceLock<watch::Sender<bool>> = OnceLock::new();
    TX.get_or_init(|| watch::channel(false).0)
}

/// Turns whose task has not finished, so [`shutdown`] can wait for their
/// processes to be cleaned up.
static LIVE: Mutex<usize> = Mutex::new(0);
static IDLE: Condvar = Condvar::new();

struct LiveGuard;

impl LiveGuard {
    fn new() -> Self {
        *LIVE.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        LiveGuard
    }
}

impl Drop for LiveGuard {
    fn drop(&mut self) {
        *LIVE.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        IDLE.notify_all();
    }
}

/// Cancels every running turn and waits up to `grace` for their processes to
/// exit. Call it after `Pod::run` returns.
pub fn shutdown(grace: Duration) {
    shutdown_signal().send_replace(true);
    let live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
    let _ = IDLE
        .wait_timeout_while(live, grace, |n| *n > 0)
        .unwrap_or_else(|e| e.into_inner());
}

// ---------------------------------------------------------------------------
// Running a turn
// ---------------------------------------------------------------------------

#[derive(Default)]
struct TurnOutput {
    text: Option<String>,
    session_id: Option<String>,
    cost_usd: Option<f64>,
    duration_ms: Option<u64>,
    turns: Option<u64>,
    usage: Option<Value>,
    /// The parsed answer when the turn had an output schema.
    data: Option<Value>,
    /// A failure the provider reported in its events.
    error: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    Cancelled,
    TimedOut,
}

async fn run_turn(
    opts: Opts,
    turn_opts: TurnOpts,
    session: Option<String>,
    prompt: String,
    turn: Arc<Turn>,
    cancel_rx: oneshot::Receiver<()>,
    agent: Arc<Mutex<AgentState>>,
) {
    let _live = LiveGuard::new();
    let started = Instant::now();
    let stopped: Arc<Mutex<Option<Stop>>> = Arc::new(Mutex::new(None));
    let cancel = {
        let stopped = stopped.clone();
        let mut shutdown = shutdown_signal().subscribe();
        let timeout = turn_opts
            .timeout_ms
            .or(opts.timeout_ms)
            .map(Duration::from_millis);
        async move {
            let why = tokio::select! {
                _ = cancel_rx => Stop::Cancelled,
                _ = shutdown.wait_for(|down| *down) => Stop::Cancelled,
                _ = async {
                    match timeout {
                        Some(t) => tokio::time::sleep(t).await,
                        None => std::future::pending().await,
                    }
                } => Stop::TimedOut,
            };
            *stopped.lock().unwrap_or_else(|e| e.into_inner()) = Some(why);
        }
    };

    let outcome = match opts.provider {
        Provider::Claude => {
            claude_turn(
                &opts,
                &turn_opts,
                session.as_deref(),
                &prompt,
                &turn,
                cancel,
            )
            .await
        }
        Provider::Codex => {
            codex_turn(
                &opts,
                &turn_opts,
                session.as_deref(),
                &prompt,
                &turn,
                cancel,
            )
            .await
        }
    };
    let stop = *stopped.lock().unwrap_or_else(|e| e.into_inner());

    let (out, failed) = outcome;
    let (status, error) = match (stop, failed, out.error.clone()) {
        (Some(Stop::Cancelled), _, _) => (Status::Cancelled, None),
        (Some(Stop::TimedOut), _, _) => (
            Status::Failed,
            Some(json!({ "kind": Keyword::new("timeout"), "message": "turn timed out" })),
        ),
        (None, Some(message), _) => (
            Status::Failed,
            Some(json!({ "kind": Keyword::new("process"), "message": message })),
        ),
        (None, None, Some(message)) => (
            Status::Failed,
            Some(json!({ "kind": Keyword::new("provider"), "message": message })),
        ),
        (None, None, None) => (Status::Done, None),
    };

    if let Some(id) = &out.session_id {
        agent.lock().unwrap_or_else(|e| e.into_inner()).session = Some(id.clone());
    }
    let result = json!({
        "provider": opts.provider.keyword(),
        "status": status.keyword(),
        "text": out.text,
        "session-id": out.session_id,
        "cost-usd": out.cost_usd,
        "duration-ms": out.duration_ms.unwrap_or(started.elapsed().as_millis() as u64),
        "turns": out.turns,
        "usage": out.usage,
        "data": out.data,
        "error": error,
    });
    turn.finish(status, result);
}

/// What a turn produced, and the error if running the CLI failed.
type TurnResult = (TurnOutput, Option<String>);

async fn claude_turn<C>(
    opts: &Opts,
    turn_opts: &TurnOpts,
    session: Option<&str>,
    prompt: &str,
    turn: &Turn,
    cancel: C,
) -> TurnResult
where
    C: std::future::Future<Output = ()> + Send,
{
    use claude_wrapper::streaming::{StreamEvent, stream_query_cancellable};
    use claude_wrapper::{Claude, OutputFormat, PermissionMode, QueryCommand};

    let mut out = TurnOutput::default();
    let mut builder = Claude::builder();
    if let Some(cwd) = &opts.cwd {
        builder = builder.working_dir(cwd);
    }
    for (k, v) in &opts.env {
        builder = builder.env(k, v);
    }
    let claude = match builder.build() {
        Ok(c) => c,
        Err(e) => return (out, Some(e.to_string())),
    };
    let mut cmd = QueryCommand::new(prompt)
        .output_format(OutputFormat::StreamJson)
        .permission_mode(if opts.allow_edits {
            PermissionMode::AcceptEdits
        } else {
            PermissionMode::Default
        });
    if let Some(m) = &opts.model {
        cmd = cmd.model(m);
    }
    if let Some(p) = &opts.system_prompt {
        cmd = cmd.system_prompt(p);
    }
    if let Some(p) = &opts.append_system_prompt {
        cmd = cmd.append_system_prompt(p);
    }
    if let Some(b) = opts.max_budget_usd {
        cmd = cmd.max_budget_usd(b);
    }
    if let Some(id) = session {
        cmd = cmd.resume(id);
    }
    if let Some(schema) = &turn_opts.output_schema {
        cmd = cmd.json_schema(schema.to_string());
    }

    let ran = stream_query_cancellable(&claude, &cmd, cancel, |ev: StreamEvent| {
        if let Some(id) = ev.session_id() {
            out.session_id = Some(id.to_string());
        }
        if ev.is_result() {
            out.text = ev.result_text().map(str::to_string);
            out.cost_usd = ev.cost_usd();
            out.duration_ms = ev.data.get("duration_ms").and_then(Value::as_u64);
            out.turns = ev.data.get("num_turns").and_then(Value::as_u64);
            out.usage = ev.data.get("usage").cloned();
            out.data = ev.data.get("structured_output").cloned();
            if ev.data.get("is_error").and_then(Value::as_bool) == Some(true) {
                out.error = Some(
                    out.text
                        .clone()
                        .unwrap_or_else(|| "claude reported an error".into()),
                );
            }
        }
        turn.push(ev.data);
    })
    .await;
    let failed = ran.err().map(|e| e.to_string());
    (out, failed)
}

async fn codex_turn<C>(
    opts: &Opts,
    turn_opts: &TurnOpts,
    session: Option<&str>,
    prompt: &str,
    turn: &Turn,
    cancel: C,
) -> TurnResult
where
    C: std::future::Future<Output = ()> + Send,
{
    use codex_wrapper::streaming::{stream_exec_cancellable, stream_exec_resume_cancellable};
    use codex_wrapper::types::JsonLineEvent;
    use codex_wrapper::{Codex, ExecCommand, ExecResumeCommand, SandboxMode};

    let mut out = TurnOutput::default();
    let mut builder = Codex::builder();
    if let Some(cwd) = &opts.cwd {
        builder = builder.working_dir(cwd);
    }
    for (k, v) in &opts.env {
        builder = builder.env(k, v);
    }
    let codex = match builder.build() {
        Ok(c) => c,
        Err(e) => return (out, Some(e.to_string())),
    };
    // codex reads the schema from a file.
    let schema_file = match &turn_opts.output_schema {
        Some(schema) => match SchemaFile::write(schema) {
            Ok(f) => Some(f),
            Err(e) => return (out, Some(format!("could not write the output schema: {e}"))),
        },
        None => None,
    };
    let (sandbox, sandbox_name) = if opts.allow_edits {
        (SandboxMode::WorkspaceWrite, "workspace-write")
    } else {
        (SandboxMode::ReadOnly, "read-only")
    };

    let handler = |ev: JsonLineEvent| {
        match ev.event_type.as_str() {
            "thread.started" => {
                if let Some(id) = ev.extra.get("thread_id").and_then(Value::as_str) {
                    out.session_id = Some(id.to_string());
                }
            }
            "item.completed" => {
                if let Some(item) = ev.extra.get("item") {
                    let kind = item
                        .get("type")
                        .or_else(|| item.get("item_type"))
                        .and_then(Value::as_str);
                    if kind == Some("agent_message") {
                        out.text = item.get("text").and_then(Value::as_str).map(str::to_string);
                    }
                }
            }
            "turn.completed" => out.usage = ev.extra.get("usage").cloned(),
            "turn.failed" | "error" => {
                let message = ev
                    .extra
                    .get("error")
                    .and_then(|e| e.get("message").or(Some(e)))
                    .or_else(|| ev.extra.get("message"))
                    .map(|m| {
                        m.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| m.to_string())
                    });
                out.error = message.or_else(|| Some(ev.event_type.clone()));
            }
            _ => {}
        }
        turn.push(serde_json::to_value(&ev).unwrap_or(Value::Null));
    };

    let ran = match session {
        None => {
            let mut cmd = ExecCommand::new(prompt)
                .sandbox(sandbox)
                .skip_git_repo_check();
            if let Some(m) = &opts.model {
                cmd = cmd.model(m);
            }
            if let Some(f) = &schema_file {
                cmd = cmd.output_schema(f.path());
            }
            stream_exec_cancellable(&codex, &cmd, cancel, handler).await
        }
        Some(id) => {
            // exec resume has no --sandbox; set it through config.
            let mut cmd = ExecResumeCommand::new()
                .session_id(id)
                .prompt(prompt)
                .skip_git_repo_check()
                .config(format!("sandbox_mode=\"{sandbox_name}\""));
            if let Some(m) = &opts.model {
                cmd = cmd.model(m);
            }
            if let Some(f) = &schema_file {
                cmd = cmd.output_schema(f.path());
            }
            stream_exec_resume_cancellable(&codex, &cmd, cancel, handler).await
        }
    };
    drop(schema_file);
    if turn_opts.output_schema.is_some() {
        out.data = out
            .text
            .as_deref()
            .and_then(|t| serde_json::from_str(t).ok());
    }
    let failed = ran.err().map(|e| e.to_string());
    (out, failed)
}

/// A schema written to a temporary file for codex, removed on drop.
struct SchemaFile(std::path::PathBuf);

impl SchemaFile {
    fn write(schema: &Value) -> std::io::Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("pod-agent-schema-{}-{n}.json", std::process::id()));
        std::fs::write(&path, schema.to_string())?;
        Ok(SchemaFile(path))
    }

    fn path(&self) -> String {
        self.0.display().to_string()
    }
}

impl Drop for SchemaFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

// ---------------------------------------------------------------------------
// Namespace
// ---------------------------------------------------------------------------

fn start_turn(
    agents: &Handles<Agent>,
    turns: &Handles<Arc<Turn>>,
    agent: &Handle,
    prompt: String,
    turn_opts: TurnOpts,
) -> Result<(Handle, Arc<Turn>), PodError> {
    let (opts, state) = agents.with(agent, |a| Ok((a.opts.clone(), a.state.clone())))?;
    let (turn, cancel_rx, session) = {
        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
        if s.current
            .as_ref()
            .is_some_and(|t| t.status() == Status::Running)
        {
            return Err(
                PodError::new("agent is running another turn").with_data(json!({ "agent": agent }))
            );
        }
        let (tx, rx) = oneshot::channel();
        let turn = Arc::new(Turn::new(tx));
        s.current = Some(turn.clone());
        (turn, rx, s.session.clone())
    };
    let handle = turns.insert(turn.clone());
    runtime().spawn(run_turn(
        opts,
        turn_opts,
        session,
        prompt,
        turn.clone(),
        cancel_rx,
        state,
    ));
    Ok((handle, turn))
}

/// Waits for `turn` on the calling thread, giving up when the pod shuts down.
fn wait_for(turn: &Arc<Turn>, timeout_ms: Option<u64>, tx: &Emitter) -> Option<Value> {
    let poke = turn.clone();
    tx.on_cancel(move || poke.poke());
    let deadline = timeout_ms.map(|ms| Instant::now() + Duration::from_millis(ms));
    turn.wait(deadline, &|| tx.is_cancelled())
}

/// The agent pod's namespace.
pub fn namespace() -> Namespace {
    let agents: Handles<Agent> = Handles::new("agent");
    let turns: Handles<Arc<Turn>> = Handles::new("turn");

    let a_create = agents.clone();
    let (a_ask, t_ask) = (agents.clone(), turns.clone());
    let (a_tell, t_tell) = (agents.clone(), turns.clone());
    let a_session = agents.clone();
    let a_close = agents;
    let (t_status, t_result, t_events, t_cancel) =
        (turns.clone(), turns.clone(), turns.clone(), turns);

    Namespace::new(NAMESPACE)
        .function("create", move |(opts,): (Opts,)| {
            let agent = Agent {
                state: Arc::new(Mutex::new(AgentState {
                    session: opts.session.clone(),
                    current: None,
                })),
                opts,
            };
            Ok::<_, PodError>(a_create.insert(agent))
        })
        .doc("create", "Configures an agent. Starts no process.")
        .arglists("create", ["[opts]"])
        .stream_fn(
            "-ask",
            move |(agent, prompt, turn_opts): (Handle, String, Option<TurnOpts>), tx: Emitter| {
            let (_handle, turn) =
                start_turn(&a_ask, &t_ask, &agent, prompt, turn_opts.unwrap_or_default())?;
            // babashka exiting mid-ask cancels the turn and kills the CLI.
            let cancel = turn.clone();
            tx.on_cancel(move || cancel.cancel());
            if let Some(result) = wait_for(&turn, None, &tx) {
                tx.send(&result)?;
            }
            Ok(())
        })
        .code("ask", ASK)
        .function(
            "-tell",
            move |(agent, prompt, turn_opts): (Handle, String, Option<TurnOpts>)| {
                start_turn(&a_tell, &t_tell, &agent, prompt, turn_opts.unwrap_or_default())
                    .map(|(h, _)| h)
            },
        )
        .code("tell", TELL)
        .function("status", move |(turn,): (Handle,)| {
            t_status.with(&turn, |t| Ok(t.status().keyword()))
        })
        .doc("status", "The turn's status: :running, :done, :failed or :cancelled.")
        .arglists("status", ["[turn]"])
        .stream_fn(
            "-result",
            move |(turn, timeout_ms): (Handle, Option<u64>), tx: Emitter| {
                let turn = t_result.with(&turn, |t| Ok(t.clone()))?;
                match wait_for(&turn, timeout_ms, &tx) {
                    Some(result) => tx.send(&result),
                    None => tx.send(&Keyword::new("timeout")),
                }
            },
        )
        .code("result", RESULT)
        .stream_fn("events", move |(turn,): (Handle,), tx: Emitter| {
            let turn = t_events.with(&turn, |t| Ok(t.clone()))?;
            let poke = turn.clone();
            tx.on_cancel(move || poke.poke());
            let mut sent = 0;
            loop {
                let (batch, done) = {
                    let mut s = turn.lock();
                    while s.events.len() == sent && s.result.is_none() && !tx.is_cancelled() {
                        s = turn
                            .changed
                            .wait_timeout(s, Duration::from_secs(3600))
                            .unwrap_or_else(|e| e.into_inner())
                            .0;
                    }
                    (s.events[sent..].to_vec(), s.result.is_some() || tx.is_cancelled())
                };
                sent += batch.len();
                for event in &batch {
                    tx.send(event)?;
                }
                if done {
                    return Ok(());
                }
            }
        })
        .doc(
            "events",
            "Calls f with each raw provider event of turn, replaying ones already seen, and returns when the turn ends.",
        )
        .arglists("events", ["[turn f]"])
        .function("cancel", move |(turn,): (Handle,)| {
            t_cancel.with(&turn, |t| {
                t.cancel();
                Ok(())
            })?;
            Ok::<_, PodError>(turn)
        })
        .doc("cancel", "Stops the turn and kills its process. Returns the turn.")
        .arglists("cancel", ["[turn]"])
        .function("session-id", move |(agent,): (Handle,)| {
            a_session.with(&agent, |a| {
                Ok(a.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .session
                    .clone())
            })
        })
        .doc("session-id", "The provider's session or thread id, or nil before the first turn.")
        .arglists("session-id", ["[agent]"])
        .function("close", move |(agent,): (Handle,)| {
            a_close.remove_with(&agent, |a| {
                if let Some(t) = &a.state.lock().unwrap_or_else(|e| e.into_inner()).current {
                    t.cancel();
                }
                Ok(())
            })
        })
        .doc("close", "Cancels a running turn and forgets the agent.")
        .arglists("close", ["[agent]"])
}
