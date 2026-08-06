use crate::db;
use crate::error::{AppError, AppResult};
use crate::import_util;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{info, warn};
use uuid::Uuid;

const MAX_LOG_LINES: usize = 2000;
const MAX_JOB_HISTORY: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Succeeded => "succeeded",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FarmEvent {
    pub seq: u64,
    pub ts: String,
    pub line: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FarmJob {
    pub id: String,
    pub provider: String,
    pub status: JobStatus,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub accounts_count: usize,
    pub inject: bool,
    pub headless: bool,
    pub device_auth: bool,
    pub concurrency: u32,
    pub settle_secs: Option<u64>,
    pub auto_import: bool,
    pub skip_existing: bool,
    pub account_delay: f64,
    pub output_path: String,
    pub work_dir: String,
    pub ok: u32,
    pub fail: u32,
    pub total: u32,
    pub current_step: Option<String>,
    pub current_email: Option<String>,
    pub import_result: Option<Value>,
    pub log_count: usize,
}

#[derive(Debug, Deserialize)]
pub struct StartFarmRequest {
    /// One account per line: email|password, email:password, or bare email (+ default_password).
    pub accounts: String,
    #[serde(default)]
    pub default_password: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default = "default_true")]
    pub inject: bool,
    #[serde(default)]
    pub headless: bool,
    #[serde(default)]
    pub device_auth: bool,
    #[serde(default)]
    pub skip_exchange: bool,
    pub settle_secs: Option<u64>,
    #[serde(default = "default_true")]
    pub auto_import: bool,
    pub concurrency: Option<u32>,
    pub account_retries: Option<u32>,
    #[serde(default)]
    pub skip_existing: bool,
    #[serde(default)]
    pub account_delay: Option<f64>,
    pub proxy_file: Option<String>,
    #[serde(default)]
    pub imap_host: Option<String>,
    #[serde(default)]
    pub imap_user: Option<String>,
    #[serde(default)]
    pub imap_pass: Option<String>,
    #[serde(default)]
    pub captcha_mode: Option<String>,
    #[serde(default)]
    pub mail_mode: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RetryFarmRequest {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default = "default_true")]
    pub inject: bool,
    #[serde(default)]
    pub headless: bool,
    #[serde(default)]
    pub device_auth: bool,
    #[serde(default)]
    pub skip_exchange: bool,
    #[serde(default)]
    pub settle_secs: Option<u64>,
    #[serde(default = "default_true")]
    pub auto_import: bool,
    #[serde(default)]
    pub concurrency: Option<u32>,
    #[serde(default)]
    pub account_retries: Option<u32>,
    #[serde(default)]
    pub skip_existing: bool,
    #[serde(default)]
    pub account_delay: Option<f64>,
    pub proxy_file: Option<String>,
}

impl Default for RetryFarmRequest {
    fn default() -> Self {
        Self {
            provider: None,
            inject: true,
            headless: false,
            device_auth: false,
            skip_exchange: false,
            settle_secs: None,
            auto_import: true,
            concurrency: None,
            account_retries: Some(2),
            skip_existing: false,
            account_delay: None,
            proxy_file: None,
        }
    }
}

fn default_true() -> bool {
    true
}

const DEFAULT_MAX_CONCURRENCY: u32 = 8;

fn max_concurrency_cap() -> u32 {
    std::env::var("MARIONETTE_FARM_MAX_CONCURRENCY")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(DEFAULT_MAX_CONCURRENCY)
}

fn clamp_concurrency(requested: Option<u32>) -> u32 {
    let n = requested.unwrap_or(1).max(1);
    n.min(max_concurrency_cap())
}

struct LiveJob {
    meta: FarmJob,
    events: VecDeque<FarmEvent>,
    next_seq: u64,
    child: Option<Child>,
    kill_tx: Option<tokio::sync::oneshot::Sender<()>>,
    auto_import: bool,
    pool: Option<SqlitePool>,
    imported_emails: std::collections::HashSet<String>,
    import_inserted: u32,
    import_failed: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct InjectJob {
    pub id: String,
    pub kind: &'static str,
    pub status: JobStatus,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub account_id: String,
    pub account_ids: Vec<String>,
    pub bulk: bool,
    pub bulk_total: u32,
    pub bulk_ok: u32,
    pub bulk_fail: u32,
    pub email: Option<String>,
    pub headless: bool,
    pub refresh: bool,
    pub work_dir: String,
    pub log_path: String,
    pub current_step: Option<String>,
    pub inject_result: Option<Value>,
    pub log_count: usize,
}

#[derive(Debug, Clone)]
pub struct InjectPatItem {
    pub account_id: String,
    pub pat: String,
    pub email: Option<String>,
}

struct LiveInjectJob {
    meta: InjectJob,
    events: VecDeque<FarmEvent>,
    next_seq: u64,
    child: Option<Child>,
    kill_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

#[derive(Clone)]
pub struct FarmManager {
    inner: Arc<Mutex<FarmState>>,
    inject: Arc<Mutex<InjectState>>,
    python: PathBuf,
    package_parent: PathBuf,
    package_dir: PathBuf,
    grok_package_dir: PathBuf,
    grok_package_parent: PathBuf,
    data_dir: PathBuf,
}

struct FarmState {
    current: Option<LiveJob>,
    history: VecDeque<FarmJob>,
}

struct InjectState {
    current: Option<LiveInjectJob>,
    history: VecDeque<InjectJob>,
}

impl FarmManager {
    pub fn from_env(db_path: &Path) -> Self {
        let python = std::env::var("MARIONETTE_FARM_PYTHON")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("python"));

        // Subprocess cwd is the per-job work dir, so PYTHONPATH and package
        // paths must be absolute or `python -m qoder_farm` fails with
        // ModuleNotFoundError.
        let package_dir = resolve_path(
            std::env::var("MARIONETTE_FARM_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("scripts/automation/qoder_farm")),
        );

        let package_parent = package_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| resolve_path(PathBuf::from("scripts/automation")));

        let grok_package_dir = resolve_path(
            std::env::var("MARIONETTE_GROK_FARM_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("scripts/automation/grok_farm")),
        );
        let grok_package_parent = grok_package_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| package_parent.clone());

        let data_dir = resolve_path(
            std::env::var("MARIONETTE_FARM_DATA")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    db_path
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join("farm")
                }),
        );

        Self {
            inner: Arc::new(Mutex::new(FarmState {
                current: None,
                history: VecDeque::new(),
            })),
            inject: Arc::new(Mutex::new(InjectState {
                current: None,
                history: VecDeque::new(),
            })),
            python,
            package_parent,
            package_dir,
            grok_package_dir,
            grok_package_parent,
            data_dir,
        }
    }

    /// PIDs of every running automation subprocess (farm job + inject jobs).
    /// Used by the usage sampler to account for the whole automation tree
    /// (python + the browsers it spawns), not just the Rust process.
    pub async fn automation_pids(&self) -> Vec<u32> {
        let mut pids = Vec::new();
        {
            let st = self.inner.lock().await;
            if let Some(cur) = &st.current {
                if let Some(child) = &cur.child {
                    if let Some(pid) = child.id() {
                        pids.push(pid);
                    }
                }
            }
        }
        {
            let st = self.inject.lock().await;
            if let Some(cur) = &st.current {
                if let Some(child) = &cur.child {
                    if let Some(pid) = child.id() {
                        pids.push(pid);
                    }
                }
            }
        }
        pids
    }

    fn package_paths_for(&self, provider: &str) -> (&Path, &Path, &'static str, &'static str) {
        match provider {
            "grok-cli" => (
                self.grok_package_dir.as_path(),
                self.grok_package_parent.as_path(),
                "grok_farm",
                "grok-accounts.json",
            ),
            _ => (
                self.package_dir.as_path(),
                self.package_parent.as_path(),
                "qoder_farm",
                "qoder-accounts.json",
            ),
        }
    }

    pub fn info_json(&self) -> Value {
        let qoder_exists = self.package_dir.join("__main__.py").is_file()
            || self.package_dir.join("__init__.py").is_file();
        let grok_exists = self.grok_package_dir.join("__main__.py").is_file()
            || self.grok_package_dir.join("__init__.py").is_file();
        let qoder_hint = format!(
            "PYTHONPATH={} {} -m qoder_farm -f accounts.txt --json-progress",
            path_for_display(&self.package_parent),
            path_for_display(&self.python)
        );
        let grok_hint = format!(
            "PYTHONPATH={} {} -m grok_farm -f accounts.txt --json-progress",
            path_for_display(&self.grok_package_parent),
            path_for_display(&self.python)
        );
        json!({
            "provider": "qoder",
            "package_dir": path_for_display(&self.package_dir),
            "package_parent": path_for_display(&self.package_parent),
            "python": path_for_display(&self.python),
            "data_dir": path_for_display(&self.data_dir),
            "package_present": qoder_exists,
            "max_concurrency": max_concurrency_cap(),
            "run_hint": qoder_hint,
            "packages": {
                "qoder": {
                    "package_dir": path_for_display(&self.package_dir),
                    "package_parent": path_for_display(&self.package_parent),
                    "package_present": qoder_exists,
                    "module": "qoder_farm",
                    "run_hint": format!(
                        "PYTHONPATH={} {} -m qoder_farm -f accounts.txt --json-progress",
                        path_for_display(&self.package_parent),
                        path_for_display(&self.python)
                    ),
                },
                "grok-cli": {
                    "package_dir": path_for_display(&self.grok_package_dir),
                    "package_parent": path_for_display(&self.grok_package_parent),
                    "package_present": grok_exists,
                    "module": "grok_farm",
                    "run_hint": grok_hint,
                },
            },
        })
    }

    pub async fn snapshot(&self) -> Value {
        let st = self.inner.lock().await;
        let current = st.current.as_ref().map(|j| job_public(&j.meta, j.events.len()));
        let history: Vec<Value> = st
            .history
            .iter()
            .map(|j| serde_json::to_value(j).unwrap_or(json!({})))
            .collect();
        json!({
            "info": self.info_json(),
            "current": current,
            "history": history,
            "busy": st.current.as_ref().map(|j| !j.meta.status.is_terminal()).unwrap_or(false),
        })
    }

    pub async fn start_inject(
        &self,
        account_id: &str,
        pat: &str,
        email: Option<&str>,
        headless: bool,
        refresh: bool,
        pool: SqlitePool,
    ) -> AppResult<Value> {
        self.start_inject_items(
            vec![InjectPatItem {
                account_id: account_id.to_string(),
                pat: pat.to_string(),
                email: email.map(|s| s.to_string()),
            }],
            headless,
            refresh,
            pool,
        )
        .await
    }

    pub async fn start_inject_bulk(
        &self,
        items: Vec<InjectPatItem>,
        headless: bool,
        refresh: bool,
        pool: SqlitePool,
    ) -> AppResult<Value> {
        self.start_inject_items(items, headless, refresh, pool).await
    }

    async fn start_inject_items(
        &self,
        items: Vec<InjectPatItem>,
        headless: bool,
        refresh: bool,
        pool: SqlitePool,
    ) -> AppResult<Value> {
        if items.is_empty() {
            return Err(AppError::BadRequest(
                "no qoder accounts with personalToken to inject".into(),
            ));
        }
        for it in &items {
            if it.pat.trim().is_empty() {
                return Err(AppError::BadRequest(format!(
                    "account {} has empty personalToken",
                    it.account_id
                )));
            }
        }
        if !self.package_dir.join("__main__.py").is_file() {
            return Err(AppError::Internal(format!(
                "qoder_farm package missing at {}",
                self.package_dir.display()
            )));
        }

        {
            let st = self.inject.lock().await;
            if let Some(cur) = &st.current {
                if !cur.meta.status.is_terminal() {
                    return Err(AppError::BadRequest(format!(
                        "inject job {} still {}",
                        cur.meta.id,
                        cur.meta.status.as_str()
                    )));
                }
            }
        }

        let bulk = items.len() > 1;
        let id = Uuid::new_v4().to_string();
        let work = self.data_dir.join("inject").join(&id);
        std::fs::create_dir_all(&work)?;
        let log_path = work.join("inject.log");
        let pats_path = work.join("pats.json");
        let pats_json: Vec<Value> = items
            .iter()
            .map(|it| {
                json!({
                    "account_id": it.account_id,
                    "pat": it.pat.trim(),
                    "email": it.email.clone().unwrap_or_default(),
                })
            })
            .collect();
        std::fs::write(
            &pats_path,
            serde_json::to_string_pretty(&pats_json).unwrap_or_else(|_| "[]".into()),
        )?;

        let account_ids: Vec<String> = items.iter().map(|it| it.account_id.clone()).collect();
        let primary_id = account_ids[0].clone();
        let email_label = if bulk {
            Some(format!("{} accounts", items.len()))
        } else {
            items[0].email.clone()
        };

        let now = chrono::Utc::now().to_rfc3339();
        let meta = InjectJob {
            id: id.clone(),
            kind: if bulk { "inject_bulk" } else { "inject" },
            status: JobStatus::Running,
            created_at: now.clone(),
            started_at: Some(now),
            finished_at: None,
            exit_code: None,
            error: None,
            account_id: primary_id,
            account_ids: account_ids.clone(),
            bulk,
            bulk_total: items.len() as u32,
            bulk_ok: 0,
            bulk_fail: 0,
            email: email_label,
            headless,
            refresh,
            work_dir: path_for_display(&work),
            log_path: path_for_display(&log_path),
            current_step: Some("start".into()),
            inject_result: None,
            log_count: 0,
        };

        let pythonpath = resolve_path(self.package_parent.clone());
        let mut cmd = Command::new(&self.python);
        cmd.current_dir(&work)
            .env("PYTHONPATH", &pythonpath)
            .env("PYTHONUNBUFFERED", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUTF8", "1");
        if let Ok(k) = std::env::var("QODER_DUDUL_ACCESS_KEY") {
            if !k.trim().is_empty() {
                cmd.env("QODER_DUDUL_ACCESS_KEY", k);
            }
        } else if let Ok(k) = std::env::var("MARIONETTE_DUDUL_ACCESS_KEY") {
            if !k.trim().is_empty() {
                cmd.env("QODER_DUDUL_ACCESS_KEY", k);
            }
        }
        cmd.arg("-m")
            .arg("qoder_farm")
            .arg("--inject-only")
            .arg("--pats-file")
            .arg(&pats_path)
            .arg("--json-progress");
        if headless {
            cmd.arg("--headless");
        } else {
            cmd.arg("--no-headless");
        }
        let automation_proxy_on = db::get_proxy_settings(&pool)
            .await
            .map(|s| s.automation_mode != "off")
            .unwrap_or(false);
        let mut inject_proxy_plan = plan_farm_proxy(automation_proxy_on, None);
        if inject_proxy_plan == FarmProxyPlan::DbPool {
            inject_proxy_plan = match write_db_proxies(&pool, &work).await {
                Some(path) => FarmProxyPlan::File(path),
                None => FarmProxyPlan::DbPool,
            };
        }
        apply_farm_proxy(&mut cmd, &inject_proxy_plan);
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        info!(
            job = %id,
            bulk,
            total = items.len(),
            headless,
            work = %path_for_display(&work),
            "starting dudul inject job"
        );

        let mut child = cmd.spawn().map_err(|e| {
            AppError::Internal(format!(
                "failed to spawn inject ({}): {e}. Set MARIONETTE_FARM_PYTHON if needed.",
                self.python.display()
            ))
        })?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();

        {
            let mut st = self.inject.lock().await;
            if let Some(prev) = st.current.take() {
                push_inject_history(&mut st.history, prev.meta);
            }
            st.current = Some(LiveInjectJob {
                meta: meta.clone(),
                events: VecDeque::new(),
                next_seq: 1,
                child: Some(child),
                kill_tx: Some(kill_tx),
            });
        }

        let mgr = self.clone();
        let job_id = id.clone();
        let log_path_c = log_path.clone();

        tokio::spawn(async move {
            let _ = append_log_file(
                &log_path_c,
                &format!("inject job {job_id} started (bulk={bulk})\n"),
            )
            .await;

            let mut join_stdout = None;
            let mut join_stderr = None;

            if let Some(out) = stdout {
                let mgr2 = mgr.clone();
                let jid = job_id.clone();
                let lp = log_path_c.clone();
                join_stdout = Some(tokio::spawn(async move {
                    let mut lines = BufReader::new(out).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let _ = append_log_file(&lp, &format!("{line}\n")).await;
                        mgr2.push_inject_line(&jid, line).await;
                    }
                }));
            }
            if let Some(err) = stderr {
                let mgr2 = mgr.clone();
                let jid = job_id.clone();
                let lp = log_path_c.clone();
                join_stderr = Some(tokio::spawn(async move {
                    let mut lines = BufReader::new(err).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let _ = append_log_file(&lp, &format!("[stderr] {line}\n")).await;
                        mgr2.push_inject_line(&jid, format!("[stderr] {line}")).await;
                    }
                }));
            }

            let cancelled = tokio::select! {
                _ = kill_rx => {
                    mgr.kill_inject_child(&job_id).await;
                    true
                }
                code = async {
                    loop {
                        let done = {
                            let mut st = mgr.inject.lock().await;
                            if let Some(cur) = st.current.as_mut() {
                                if cur.meta.id != job_id {
                                    return (true, None);
                                }
                                if let Some(child) = cur.child.as_mut() {
                                    match child.try_wait() {
                                        Ok(Some(s)) => {
                                            cur.child = None;
                                            return (false, s.code());
                                        }
                                        Ok(None) => false,
                                        Err(_) => {
                                            cur.child = None;
                                            return (false, None);
                                        }
                                    }
                                } else {
                                    return (true, None);
                                }
                            } else {
                                return (true, None);
                            }
                        };
                        let _ = done;
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    }
                } => {
                    let (was_cancel, _code) = code;
                    was_cancel
                }
            };

            let exit_code = {
                let mut st = mgr.inject.lock().await;
                if let Some(cur) = st.current.as_mut() {
                    if cur.meta.id == job_id {
                        if let Some(mut child) = cur.child.take() {
                            match child.try_wait() {
                                Ok(Some(s)) => s.code(),
                                _ => {
                                    let _ = child.kill().await;
                                    match child.wait().await {
                                        Ok(s) => s.code(),
                                        Err(_) => None,
                                    }
                                }
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            };

            if let Some(h) = join_stdout {
                let _ = h.await;
            }
            if let Some(h) = join_stderr {
                let _ = h.await;
            }

            mgr.finish_inject_job(&job_id, exit_code, cancelled).await;
        });

        Ok(json!({
            "ok": true,
            "job": inject_job_public(&meta, 0),
        }))
    }

    async fn push_inject_line(&self, job_id: &str, line: String) {
        let mut st = self.inject.lock().await;
        let Some(cur) = st.current.as_mut() else {
            return;
        };
        if cur.meta.id != job_id {
            return;
        }
        let parsed = parse_farm_line(&line);
        if let Some(ref v) = parsed {
            if let Some(step) = v.get("step").and_then(|x| x.as_str()) {
                if !step.is_empty() {
                    cur.meta.current_step = Some(step.to_string());
                }
            }
            let ty = v.get("type").and_then(|x| x.as_str());
            if ty == Some("inject_result") {
                cur.meta.inject_result = Some(v.clone());
                if let Some(n) = v.get("ok_count").and_then(|x| x.as_u64()) {
                    cur.meta.bulk_ok = n as u32;
                }
                if let Some(n) = v.get("fail_count").and_then(|x| x.as_u64()) {
                    cur.meta.bulk_fail = n as u32;
                }
                if let Some(n) = v.get("total").and_then(|x| x.as_u64()) {
                    cur.meta.bulk_total = n as u32;
                }
                if let Some(ok) = v.get("ok").and_then(|x| x.as_bool()) {
                    if !ok {
                        if let Some(reason) = v.get("reason").and_then(|x| x.as_str()) {
                            cur.meta.error = Some(reason.to_string());
                        }
                    }
                }
            } else if ty == Some("inject_account_result") {
                if let Some(n) = v.get("ok_count").and_then(|x| x.as_u64()) {
                    cur.meta.bulk_ok = n as u32;
                }
                if let Some(n) = v.get("fail_count").and_then(|x| x.as_u64()) {
                    cur.meta.bulk_fail = n as u32;
                }
                if let Some(n) = v.get("bulk_total").and_then(|x| x.as_u64()) {
                    cur.meta.bulk_total = n as u32;
                }
                if let Some(idx) = v.get("bulk_index").and_then(|x| x.as_u64()) {
                    cur.meta.current_step = Some(format!("inject {idx}/{}", cur.meta.bulk_total));
                }
            }
        }
        let seq = cur.next_seq;
        cur.next_seq += 1;
        cur.events.push_back(FarmEvent {
            seq,
            ts: chrono::Utc::now().to_rfc3339(),
            line,
            parsed,
        });
        while cur.events.len() > MAX_LOG_LINES {
            cur.events.pop_front();
        }
        cur.meta.log_count = cur.events.len();
    }

    async fn kill_inject_child(&self, job_id: &str) {
        let mut st = self.inject.lock().await;
        if let Some(cur) = st.current.as_mut() {
            if cur.meta.id == job_id {
                if let Some(mut child) = cur.child.take() {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
            }
        }
    }

    async fn finish_inject_job(
        &self,
        job_id: &str,
        exit_code: Option<i32>,
        cancelled: bool,
    ) {
        let mut st = self.inject.lock().await;
        let Some(cur) = st.current.as_mut() else {
            return;
        };
        if cur.meta.id != job_id {
            return;
        }
        cur.child = None;
        cur.kill_tx = None;
        cur.meta.finished_at = Some(chrono::Utc::now().to_rfc3339());
        if cancelled || cur.meta.status == JobStatus::Cancelled {
            cur.meta.status = JobStatus::Cancelled;
            if cur.meta.error.is_none() {
                cur.meta.error = Some("cancelled".into());
            }
        } else {
            let code = exit_code.unwrap_or(-1);
            cur.meta.exit_code = Some(code);
            let result_ok = cur
                .meta
                .inject_result
                .as_ref()
                .and_then(|v| v.get("ok"))
                .and_then(|v| v.as_bool());
            let bulk_any_ok = cur.meta.bulk && cur.meta.bulk_ok > 0;
            if result_ok == Some(true)
                || bulk_any_ok
                || (result_ok.is_none() && code == 0)
            {
                cur.meta.status = JobStatus::Succeeded;
                if bulk_any_ok && result_ok != Some(true) && cur.meta.bulk_fail > 0 {
                    cur.meta.error = Some(format!(
                        "partial: {} ok, {} failed",
                        cur.meta.bulk_ok, cur.meta.bulk_fail
                    ));
                }
            } else {
                cur.meta.status = JobStatus::Failed;
                if cur.meta.error.is_none() {
                    let reason = cur
                        .meta
                        .inject_result
                        .as_ref()
                        .and_then(|v| v.get("reason"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("exit code {code}"));
                    cur.meta.error = Some(reason);
                }
            }
        }
        cur.meta.log_count = cur.events.len();
        info!(
            job = %job_id,
            status = cur.meta.status.as_str(),
            "inject job finished"
        );
    }

    pub async fn cancel_inject(&self, job_id: &str) -> AppResult<Value> {
        let mut st = self.inject.lock().await;
        let Some(cur) = st.current.as_mut() else {
            return Err(AppError::NotFound("no inject job".into()));
        };
        if cur.meta.id != job_id {
            return Err(AppError::NotFound(format!("inject job {job_id} not current")));
        }
        if cur.meta.status.is_terminal() {
            return Err(AppError::BadRequest("job already finished".into()));
        }
        if let Some(tx) = cur.kill_tx.take() {
            let _ = tx.send(());
        }
        if let Some(mut child) = cur.child.take() {
            let _ = child.kill().await;
        }
        cur.meta.status = JobStatus::Cancelled;
        cur.meta.finished_at = Some(chrono::Utc::now().to_rfc3339());
        cur.meta.error = Some("cancelled by admin".into());
        Ok(json!({
            "ok": true,
            "job": inject_job_public(&cur.meta, cur.events.len()),
        }))
    }

    pub async fn get_inject_job(&self, job_id: &str) -> AppResult<Value> {
        let st = self.inject.lock().await;
        if let Some(cur) = &st.current {
            if cur.meta.id == job_id {
                return Ok(json!({
                    "job": inject_job_public(&cur.meta, cur.events.len()),
                    "events": cur.events.iter().collect::<Vec<_>>(),
                }));
            }
        }
        let history_job = st.history.iter().find(|j| j.id == job_id).cloned();
        drop(st);

        let log_path = self.data_dir.join("inject").join(job_id).join("inject.log");
        let events_from_log = if log_path.is_file() {
            let text = std::fs::read_to_string(&log_path).unwrap_or_default();
            text.lines()
                .enumerate()
                .map(|(i, line)| {
                    json!({
                        "seq": i + 1,
                        "ts": "",
                        "line": line,
                        "parsed": parse_farm_line(line),
                    })
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        if let Some(h) = history_job {
            let mut job = serde_json::to_value(&h).unwrap_or(json!({}));
            if let Some(obj) = job.as_object_mut() {
                obj.insert("log_count".into(), json!(events_from_log.len().max(h.log_count)));
                obj.insert("status".into(), json!(h.status.as_str()));
            }
            return Ok(json!({
                "job": job,
                "events": events_from_log,
            }));
        }

        if !events_from_log.is_empty() {
            return Ok(json!({
                "job": {
                    "id": job_id,
                    "kind": "inject",
                    "status": "unknown",
                    "log_count": events_from_log.len(),
                },
                "events": events_from_log,
            }));
        }
        Err(AppError::NotFound(format!("inject job {job_id}")))
    }

    pub async fn inject_events_after(&self, job_id: &str, after: u64) -> AppResult<Value> {
        let st = self.inject.lock().await;
        if let Some(cur) = &st.current {
            if cur.meta.id == job_id {
                let ev: Vec<&FarmEvent> = cur.events.iter().filter(|e| e.seq > after).collect();
                return Ok(json!({
                    "job": inject_job_public(&cur.meta, cur.events.len()),
                    "events": ev,
                    "after": after,
                }));
            }
        }
        let history_job = st.history.iter().find(|j| j.id == job_id).cloned();
        drop(st);

        if let Some(h) = history_job {
            let mut job = serde_json::to_value(&h).unwrap_or(json!({}));
            if let Some(obj) = job.as_object_mut() {
                obj.insert("status".into(), json!(h.status.as_str()));
            }
            let events = if after == 0 {
                let log_path = self.data_dir.join("inject").join(job_id).join("inject.log");
                if log_path.is_file() {
                    let text = std::fs::read_to_string(&log_path).unwrap_or_default();
                    text.lines()
                        .enumerate()
                        .map(|(i, line)| {
                            json!({
                                "seq": i + 1,
                                "ts": "",
                                "line": line,
                                "parsed": parse_farm_line(line),
                            })
                        })
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            return Ok(json!({
                "job": job,
                "events": events,
                "after": after,
            }));
        }
        Err(AppError::NotFound(format!("inject job {job_id}")))
    }

    pub async fn inject_snapshot(&self) -> Value {
        let st = self.inject.lock().await;
        let current = st
            .current
            .as_ref()
            .map(|j| inject_job_public(&j.meta, j.events.len()));
        let history: Vec<Value> = st
            .history
            .iter()
            .map(|j| serde_json::to_value(j).unwrap_or(json!({})))
            .collect();
        json!({
            "current": current,
            "history": history,
            "busy": st
                .current
                .as_ref()
                .map(|j| !j.meta.status.is_terminal())
                .unwrap_or(false),
        })
    }

    pub async fn start(&self, req: StartFarmRequest, pool: SqlitePool) -> AppResult<Value> {
        let provider = normalize_farm_provider(req.provider.as_deref());
        let is_grok = provider == "grok-cli";
        let (pkg_dir, pkg_parent, module, output_name) = self.package_paths_for(provider);

        let is_register_mode = req.accounts.trim_start().starts_with("register:");
        let default_pw = req
            .default_password
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let mut accounts = if is_register_mode {
            vec![("__register__".to_string(), default_pw.unwrap_or("").to_string())]
        } else {
            parse_accounts_text(&req.accounts, default_pw)
        };
        if accounts.is_empty() {
            return Err(AppError::BadRequest(
                "no accounts: use email|password lines (or bare email + default_password)"
                    .into(),
            ));
        }

        {
            let st = self.inner.lock().await;
            if let Some(cur) = &st.current {
                if !cur.meta.status.is_terminal() {
                    return Err(AppError::BadRequest(format!(
                        "farm job {} still {}",
                        cur.meta.id,
                        cur.meta.status.as_str()
                    )));
                }
            }
        }

        if !pkg_dir.join("__main__.py").is_file() {
            return Err(AppError::BadRequest(format!(
                "{module} package missing at {} — see scripts/automation/{module}",
                pkg_dir.display()
            )));
        }

        let mut skip_emails_path: Option<PathBuf> = None;
        if req.skip_existing {
            let existing = db_provider_emails(&pool, provider).await?;
            if !existing.is_empty() {
                let before = accounts.len();
                accounts.retain(|(email, _)| !existing.contains(&email.to_lowercase()));
                let dropped = before.saturating_sub(accounts.len());
                if dropped > 0 {
                    info!(
                        skipped = dropped,
                        remaining = accounts.len(),
                        provider = provider,
                        "skip_existing: filtered accounts already in DB"
                    );
                }
            }
            if accounts.is_empty() {
                return Err(AppError::BadRequest(format!(
                    "skip_existing: nothing left to farm (all emails already in {provider} accounts)"
                )));
            }
        }

        let id = Uuid::new_v4().to_string();
        let work = self.data_dir.join("jobs").join(&id);
        std::fs::create_dir_all(&work)?;
        let accounts_path = work.join("accounts.txt");
        let output_path = work.join(output_name);
        let log_path = work.join("farm.log");

        if is_register_mode {
            std::fs::write(&accounts_path, format!("{}\n", req.accounts.trim()))?;
        } else {
            let mut body = String::new();
            for (email, pass) in &accounts {
                body.push_str(email);
                body.push('|');
                body.push_str(pass);
                body.push('\n');
            }
            std::fs::write(&accounts_path, body)?;
        }

        let automation_proxy_on = db::get_proxy_settings(&pool)
            .await
            .map(|s| s.automation_mode != "off")
            .unwrap_or(false);
        let mut proxy_plan = plan_farm_proxy(automation_proxy_on, req.proxy_file.as_deref());
        if proxy_plan == FarmProxyPlan::DbPool {
            proxy_plan = match write_db_proxies(&pool, &work).await {
                Some(path) => FarmProxyPlan::File(path),
                None => FarmProxyPlan::DbPool,
            };
        }

        if req.skip_existing {
            let path = work.join("skip-emails.txt");
            let mut skip_body = String::new();
            for email in db_provider_emails(&pool, provider).await? {
                skip_body.push_str(&email);
                skip_body.push('\n');
            }
            std::fs::write(&path, skip_body)?;
            skip_emails_path = Some(path);
        }

        let workers = clamp_concurrency(req.concurrency);
        let account_delay = req.account_delay.unwrap_or(0.0).max(0.0);
        let inject = if is_grok { false } else { req.inject };
        let device_auth = if is_grok { false } else { req.device_auth };
        let settle_secs = if is_grok { None } else { req.settle_secs };
        let now = chrono::Utc::now().to_rfc3339();
        let meta = FarmJob {
            id: id.clone(),
            provider: provider.to_string(),
            status: JobStatus::Running,
            created_at: now.clone(),
            started_at: Some(now),
            finished_at: None,
            exit_code: None,
            error: None,
            accounts_count: accounts.len(),
            inject,
            headless: req.headless,
            device_auth,
            concurrency: workers,
            settle_secs,
            auto_import: req.auto_import,
            skip_existing: req.skip_existing,
            account_delay,
            output_path: path_for_display(&output_path),
            work_dir: path_for_display(&work),
            ok: 0,
            fail: 0,
            total: accounts.len() as u32,
            current_step: Some("start".into()),
            current_email: None,
            import_result: None,
            log_count: 0,
        };

        let pythonpath = resolve_path(pkg_parent.to_path_buf());
        let mut cmd = Command::new(&self.python);
        cmd.current_dir(&work)
            .env("PYTHONPATH", &pythonpath)
            .env("PYTHONUNBUFFERED", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUTF8", "1");
        if is_register_mode {
            let imap_prefix = if is_grok { "GROK" } else { "QODER" };
            if let Some(pw) = default_pw {
                cmd.env(format!("{imap_prefix}_PASSWORD"), pw);
                if !is_grok {
                    cmd.env("QODER_REGISTER_PASSWORD", pw);
                }
            }
            if let Some(h) = req.imap_host.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                cmd.env(format!("{imap_prefix}_IMAP_HOST"), h);
            }
            if let Some(u) = req.imap_user.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                cmd.env(format!("{imap_prefix}_IMAP_USER"), u);
            }
            if let Some(p) = req.imap_pass.as_deref().filter(|s| !s.trim().is_empty()) {
                cmd.env(format!("{imap_prefix}_IMAP_PASS"), p);
            }
            if let Some(m) = req
                .mail_mode
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                cmd.env(format!("{imap_prefix}_MAIL_MODE"), m);
            }
            // Temp-mail worker config lives in the DB (admin-editable via
            // /admin/mail-settings) and takes precedence over the package .env
            // (Python _load_dotenv uses setdefault, so cmd.env wins). When the
            // DB row is not configured we inject nothing and fall back to .env.
            if is_grok {
                if let Ok(mail) = db::get_mail_settings(&pool).await {
                    if !mail.base_url.trim().is_empty()
                        && !mail.domain.trim().is_empty()
                        && !mail.admin_password.is_empty()
                    {
                        cmd.env("GROK_MAIL_MODE", "cf");
                        cmd.env("GROK_CF_MAIL_BASE_URL", mail.base_url.trim());
                        cmd.env("GROK_CF_MAIL_DOMAIN", mail.domain.trim());
                        cmd.env("GROK_CF_MAIL_ADMIN_PASSWORD", &mail.admin_password);
                        if !mail.site_password.is_empty() {
                            cmd.env("GROK_CF_MAIL_SITE_PASSWORD", &mail.site_password);
                        }
                    }
                }
            }
        }
        if !is_grok {
            if let Some(mode) = req
                .captcha_mode
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                cmd.env("QODER_CAPTCHA_MODE", mode);
            }
        }
        if !is_grok {
            if let Ok(k) = std::env::var("QODER_DUDUL_ACCESS_KEY") {
                if !k.trim().is_empty() {
                    cmd.env("QODER_DUDUL_ACCESS_KEY", k);
                }
            } else if let Ok(k) = std::env::var("MARIONETTE_DUDUL_ACCESS_KEY") {
                if !k.trim().is_empty() {
                    cmd.env("QODER_DUDUL_ACCESS_KEY", k);
                }
            }
        }
        cmd.arg("-m")
            .arg(module)
            .arg("-f")
            .arg(&accounts_path)
            .arg("-o")
            .arg(&output_path)
            .arg("--json-progress")
            .arg("--concurrency")
            .arg(workers.to_string());

        if is_grok {
            if req.headless {
                cmd.arg("--headless");
            } else {
                cmd.arg("--no-headless");
            }
        } else {
            if inject {
                cmd.arg("--inject");
            } else {
                cmd.arg("--no-inject");
            }
            if req.headless {
                cmd.arg("--headless");
            } else {
                cmd.arg("--no-headless");
            }
            if device_auth {
                cmd.arg("--device-auth");
            }
            if req.skip_exchange {
                cmd.arg("--skip-exchange");
            }
            if let Some(s) = settle_secs {
                cmd.arg("--settle").arg(s.to_string());
            }
        }
        let account_retries = req.account_retries.unwrap_or(2).max(1);
        cmd.arg("--account-retries")
            .arg(account_retries.to_string());
        if account_delay > 0.0 {
            cmd.arg("--account-delay").arg(account_delay.to_string());
        }
        if req.skip_existing {
            cmd.arg("--skip-existing");
            if let Some(ref sp) = skip_emails_path {
                cmd.arg("--skip-emails-file").arg(sp);
            }
        }
        apply_farm_proxy(&mut cmd, &proxy_plan);

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);

        info!(job = %id, provider = provider, module = module, "starting farm subprocess");
        let mut child = cmd.spawn().map_err(|e| {
            AppError::Internal(format!(
                "failed to spawn farm ({}): {e}. Set MARIONETTE_FARM_PYTHON if needed.",
                self.python.display()
            ))
        })?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();

        {
            let mut st = self.inner.lock().await;
            // archive previous terminal job
            if let Some(prev) = st.current.take() {
                push_history(&mut st.history, prev.meta);
            }
            st.current = Some(LiveJob {
                meta: meta.clone(),
                events: VecDeque::new(),
                next_seq: 1,
                child: Some(child),
                kill_tx: Some(kill_tx),
                auto_import: req.auto_import,
                pool: if req.auto_import {
                    Some(pool.clone())
                } else {
                    None
                },
                imported_emails: std::collections::HashSet::new(),
                import_inserted: 0,
                import_failed: 0,
            });
        }

        let mgr = self.clone();
        let job_id = id.clone();
        let log_path_c = log_path.clone();
        let output_path_c = output_path.clone();
        let auto_import = req.auto_import;

        tokio::spawn(async move {
            let _ = append_log_file(&log_path_c, &format!("job {job_id} started\n")).await;

            let mut join_stdout = None;
            let mut join_stderr = None;

            if let Some(out) = stdout {
                let mgr2 = mgr.clone();
                let jid = job_id.clone();
                let lp = log_path_c.clone();
                join_stdout = Some(tokio::spawn(async move {
                    let mut lines = BufReader::new(out).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let _ = append_log_file(&lp, &format!("{line}\n")).await;
                        mgr2.push_line(&jid, line).await;
                    }
                }));
            }
            if let Some(err) = stderr {
                let mgr2 = mgr.clone();
                let jid = job_id.clone();
                let lp = log_path_c.clone();
                join_stderr = Some(tokio::spawn(async move {
                    let mut lines = BufReader::new(err).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if is_stderr_noise(&line) {
                            continue;
                        }
                        let _ = append_log_file(&lp, &format!("[stderr] {line}\n")).await;
                        mgr2.push_line(&jid, format!("[stderr] {line}")).await;
                    }
                }));
            }

            let cancelled = tokio::select! {
                _ = kill_rx => {
                    mgr.kill_child(&job_id).await;
                    true
                }
                code = async {
                    loop {
                        let done = {
                            let mut st = mgr.inner.lock().await;
                            if let Some(cur) = st.current.as_mut() {
                                if cur.meta.id != job_id {
                                    return (true, None);
                                }
                                if let Some(child) = cur.child.as_mut() {
                                    match child.try_wait() {
                                        Ok(Some(s)) => {
                                            cur.child = None;
                                            return (false, s.code());
                                        }
                                        Ok(None) => false,
                                        Err(_) => {
                                            cur.child = None;
                                            return (false, None);
                                        }
                                    }
                                } else {
                                    return (true, None);
                                }
                            } else {
                                return (true, None);
                            }
                        };
                        let _ = done;
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    }
                } => {
                    let (was_cancel, _code) = code;
                    was_cancel
                }
            };

            let exit_code = {
                let mut st = mgr.inner.lock().await;
                if let Some(cur) = st.current.as_mut() {
                    if cur.meta.id == job_id {
                        if let Some(mut child) = cur.child.take() {
                            match child.try_wait() {
                                Ok(Some(s)) => s.code(),
                                _ => {
                                    let _ = child.kill().await;
                                    match child.wait().await {
                                        Ok(s) => s.code(),
                                        Err(_) => None,
                                    }
                                }
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            };

            if let Some(h) = join_stdout {
                let _ = h.await;
            }
            if let Some(h) = join_stderr {
                let _ = h.await;
            }

            mgr.finish_job(
                &job_id,
                exit_code,
                cancelled,
                &output_path_c,
                auto_import,
                pool,
            )
            .await;
        });

        Ok(json!({
            "job": serde_json::to_value(&meta).unwrap_or(json!({})),
            "ok": true,
        }))
    }

    async fn push_line(&self, job_id: &str, line: String) {
        let parsed = parse_farm_line(&line);

        let account_ok_email = parsed.as_ref().and_then(|v| {
            if v.get("event").and_then(|e| e.as_str()) == Some("account_ok") {
                v.get("email")
                    .and_then(|e| e.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty() && s.contains('@'))
            } else {
                None
            }
        });

        let (do_import, output_path, pool) = {
            let mut st = self.inner.lock().await;
            let Some(cur) = st.current.as_mut() else {
                return;
            };
            if cur.meta.id != job_id {
                return;
            }
            if let Some(ref v) = parsed {
                if let Some(ok) = v.get("ok").and_then(|x| x.as_u64()) {
                    cur.meta.ok = ok as u32;
                }
                if let Some(fail) = v.get("fail").and_then(|x| x.as_u64()) {
                    cur.meta.fail = fail as u32;
                }
                if let Some(total) = v.get("total").and_then(|x| x.as_u64()) {
                    cur.meta.total = total as u32;
                }
                if let Some(step) = v.get("step").and_then(|x| x.as_str()) {
                    if !step.is_empty() {
                        cur.meta.current_step = Some(step.to_string());
                    }
                }
                let display_email = v
                    .get("email_masked")
                    .and_then(|e| e.as_str())
                    .filter(|s| !s.is_empty())
                    .or_else(|| v.get("email").and_then(|e| e.as_str()).filter(|s| !s.is_empty()));
                if let Some(email) = display_email {
                    cur.meta.current_email = Some(email.to_string());
                }
            }

            let display_line = redact_farm_line_for_events(&line, &parsed);
            let display_parsed = parse_farm_line(&display_line);
            let seq = cur.next_seq;
            cur.next_seq += 1;
            cur.events.push_back(FarmEvent {
                seq,
                ts: chrono::Utc::now().to_rfc3339(),
                line: display_line,
                parsed: display_parsed,
            });
            while cur.events.len() > MAX_LOG_LINES {
                cur.events.pop_front();
            }
            cur.meta.log_count = cur.events.len();

            let already = account_ok_email
                .as_ref()
                .map(|e| cur.imported_emails.contains(&e.to_lowercase()))
                .unwrap_or(true);
            let do_import = cur.auto_import
                && account_ok_email.is_some()
                && !already
                && cur.pool.is_some();
            (
                do_import,
                PathBuf::from(&cur.meta.output_path),
                cur.pool.clone(),
            )
        };

        if do_import {
            if let (Some(email), Some(pool)) = (account_ok_email.as_ref(), pool.as_ref()) {
                match import_one_email_from_output(&output_path, email, pool).await {
                    Ok(co) => {
                        let mut st = self.inner.lock().await;
                        if let Some(cur) = st.current.as_mut() {
                            if cur.meta.id == job_id {
                                cur.imported_emails.insert(email.to_lowercase());
                                cur.import_inserted += 1;
                                cur.meta.import_result = Some(json!({
                                    "ok": true,
                                    "mode": "incremental",
                                    "inserted": cur.import_inserted,
                                    "failed": cur.import_failed,
                                    "last_email": email,
                                    "last": co,
                                }));
                                let seq = cur.next_seq;
                                cur.next_seq += 1;
                                let msg = format!(
                                    r#"{{"type":"farm","event":"import_ok","email":"{}","msg":"imported to pool","ok":{},"fail":{},"total":{}}}"#,
                                    email.replace('"', ""),
                                    cur.meta.ok,
                                    cur.meta.fail,
                                    cur.meta.total
                                );
                                let masked =
                                    redact_farm_line_for_events(&msg, &parse_farm_line(&msg));
                                let masked_parsed = parse_farm_line(&masked);
                                cur.events.push_back(FarmEvent {
                                    seq,
                                    ts: chrono::Utc::now().to_rfc3339(),
                                    line: masked,
                                    parsed: masked_parsed,
                                });
                                cur.meta.log_count = cur.events.len();
                                cur.meta.current_step = Some("import".into());
                            }
                        }
                        info!(job = %job_id, email = %email, "farm incremental import ok");
                    }
                    Err(e) => {
                        warn!(job = %job_id, email = %email, error = %e, "farm incremental import failed");
                        let mut st = self.inner.lock().await;
                        if let Some(cur) = st.current.as_mut() {
                            if cur.meta.id == job_id {
                                cur.import_failed += 1;
                                cur.meta.import_result = Some(json!({
                                    "ok": false,
                                    "mode": "incremental",
                                    "inserted": cur.import_inserted,
                                    "failed": cur.import_failed,
                                    "last_email": email,
                                    "error": e.to_string(),
                                }));
                                let seq = cur.next_seq;
                                cur.next_seq += 1;
                                let msg = format!(
                                    r#"{{"type":"farm","event":"import_err","email":"{}","msg":"{}","ok":{},"fail":{},"total":{}}}"#,
                                    email.replace('"', ""),
                                    e.to_string().replace('"', "'").chars().take(120).collect::<String>(),
                                    cur.meta.ok,
                                    cur.meta.fail,
                                    cur.meta.total
                                );
                                cur.events.push_back(FarmEvent {
                                    seq,
                                    ts: chrono::Utc::now().to_rfc3339(),
                                    line: msg.clone(),
                                    parsed: parse_farm_line(&msg),
                                });
                                cur.meta.log_count = cur.events.len();
                            }
                        }
                    }
                }
            }
        }
    }

    async fn kill_child(&self, job_id: &str) {
        let mut st = self.inner.lock().await;
        if let Some(cur) = st.current.as_mut() {
            if cur.meta.id == job_id {
                if let Some(mut child) = cur.child.take() {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
            }
        }
    }

    async fn finish_job(
        &self,
        job_id: &str,
        exit_code: Option<i32>,
        cancelled: bool,
        output_path: &Path,
        auto_import: bool,
        pool: SqlitePool,
    ) {
        let (already_inserted, already_failed) = {
            let st = self.inner.lock().await;
            st.current
                .as_ref()
                .filter(|c| c.meta.id == job_id)
                .map(|c| (c.import_inserted, c.import_failed))
                .unwrap_or((0, 0))
        };

        let import_result = if auto_import && output_path.is_file() {
            match import_output_file(output_path, &pool).await {
                Ok(mut v) => {
                    if let Some(obj) = v.as_object_mut() {
                        obj.insert("mode".into(), json!("final"));
                        obj.insert("incremental_inserted".into(), json!(already_inserted));
                        obj.insert("incremental_failed".into(), json!(already_failed));
                    }
                    Some(v)
                }
                Err(e) => {
                    warn!(error = %e, "farm auto-import failed");
                    Some(json!({
                        "ok": false,
                        "error": e.to_string(),
                        "mode": "final",
                        "incremental_inserted": already_inserted,
                        "incremental_failed": already_failed,
                    }))
                }
            }
        } else if already_inserted > 0 || already_failed > 0 {
            Some(json!({
                "ok": already_failed == 0,
                "mode": "incremental_only",
                "inserted": already_inserted,
                "failed": already_failed,
            }))
        } else {
            None
        };

        let mut st = self.inner.lock().await;
        let Some(cur) = st.current.as_mut() else {
            return;
        };
        if cur.meta.id != job_id {
            return;
        }
        cur.child = None;
        cur.kill_tx = None;
        cur.pool = None;
        cur.meta.finished_at = Some(chrono::Utc::now().to_rfc3339());
        if cancelled || cur.meta.status == JobStatus::Cancelled {
            cur.meta.status = JobStatus::Cancelled;
            if cur.meta.error.is_none() {
                cur.meta.error = Some("cancelled".into());
            }
        } else {
            let code = exit_code.unwrap_or(-1);
            cur.meta.exit_code = Some(code);
            if code == 0 {
                cur.meta.status = JobStatus::Succeeded;
            } else {
                cur.meta.status = JobStatus::Failed;
                cur.meta.error = Some(format!("exit code {code}"));
            }
        }
        if import_result.is_some() {
            cur.meta.import_result = import_result;
        }
        cur.meta.log_count = cur.events.len();
        info!(
            job = %job_id,
            status = cur.meta.status.as_str(),
            "farm job finished"
        );
    }

    pub async fn cancel(&self, job_id: &str) -> AppResult<Value> {
        let mut st = self.inner.lock().await;
        let Some(cur) = st.current.as_mut() else {
            return Err(AppError::NotFound("no farm job".into()));
        };
        if cur.meta.id != job_id {
            return Err(AppError::NotFound(format!("job {job_id} not current")));
        }
        if cur.meta.status.is_terminal() {
            return Err(AppError::BadRequest("job already finished".into()));
        }
        if let Some(tx) = cur.kill_tx.take() {
            let _ = tx.send(());
        }
        if let Some(mut child) = cur.child.take() {
            let _ = child.kill().await;
        }
        cur.meta.status = JobStatus::Cancelled;
        cur.meta.finished_at = Some(chrono::Utc::now().to_rfc3339());
        cur.meta.error = Some("cancelled by admin".into());
        Ok(json!({ "ok": true, "job": job_public(&cur.meta, cur.events.len()) }))
    }

    pub async fn get_job(&self, job_id: &str) -> AppResult<Value> {
        let st = self.inner.lock().await;
        if let Some(cur) = &st.current {
            if cur.meta.id == job_id {
                return Ok(json!({
                    "job": job_public(&cur.meta, cur.events.len()),
                    "events": cur.events.iter().collect::<Vec<_>>(),
                }));
            }
        }
        if let Some(h) = st.history.iter().find(|j| j.id == job_id) {
            return Ok(json!({ "job": h, "events": [] }));
        }
        // try reload log file
        let log_path = self.data_dir.join("jobs").join(job_id).join("farm.log");
        if log_path.is_file() {
            let text = std::fs::read_to_string(&log_path).unwrap_or_default();
            let events: Vec<Value> = text
                .lines()
                .enumerate()
                .map(|(i, line)| {
                    json!({
                        "seq": i + 1,
                        "ts": "",
                        "line": line,
                        "parsed": parse_farm_line(line),
                    })
                })
                .collect();
            return Ok(json!({
                "job": {
                    "id": job_id,
                    "status": "unknown",
                    "log_count": events.len(),
                },
                "events": events,
            }));
        }
        Err(AppError::NotFound(format!("job {job_id}")))
    }

    pub async fn events_after(&self, job_id: &str, after: u64) -> AppResult<Value> {
        let st = self.inner.lock().await;
        if let Some(cur) = &st.current {
            if cur.meta.id == job_id {
                let ev: Vec<&FarmEvent> = cur.events.iter().filter(|e| e.seq > after).collect();
                return Ok(json!({
                    "job": job_public(&cur.meta, cur.events.len()),
                    "events": ev,
                    "after": after,
                }));
            }
        }
        // finished — empty poll
        if let Some(h) = st.history.iter().find(|j| j.id == job_id) {
            return Ok(json!({
                "job": h,
                "events": [],
                "after": after,
            }));
        }
        Err(AppError::NotFound(format!("job {job_id}")))
    }

    pub async fn import_job(&self, job_id: &str, pool: &SqlitePool) -> AppResult<Value> {
        let output = {
            let st = self.inner.lock().await;
            if let Some(cur) = &st.current {
                if cur.meta.id == job_id {
                    Some(PathBuf::from(&cur.meta.output_path))
                } else {
                    None
                }
            } else if let Some(h) = st.history.iter().find(|j| j.id == job_id) {
                Some(PathBuf::from(&h.output_path))
            } else {
                None
            }
        };
        let path = output.unwrap_or_else(|| {
            let work = self.data_dir.join("jobs").join(job_id);
            let grok = work.join("grok-accounts.json");
            if grok.is_file() {
                grok
            } else {
                work.join("qoder-accounts.json")
            }
        });
        if !path.is_file() {
            return Err(AppError::NotFound(format!(
                "output not found: {}",
                path.display()
            )));
        }
        let result = import_output_file(&path, pool).await?;
        // attach to job meta if current/history
        {
            let mut st = self.inner.lock().await;
            if let Some(cur) = st.current.as_mut() {
                if cur.meta.id == job_id {
                    cur.meta.import_result = Some(result.clone());
                }
            }
            for h in st.history.iter_mut() {
                if h.id == job_id {
                    h.import_result = Some(result.clone());
                }
            }
        }
        Ok(result)
    }

    pub fn failed_accounts_text(&self, job_id: &str) -> AppResult<(String, usize)> {
        let work = self.data_dir.join("jobs").join(job_id);
        if !work.is_dir() {
            return Err(AppError::NotFound(format!("job work dir {job_id}")));
        }

        let failed_path = work.join("accounts.failed.txt");
        if failed_path.is_file() {
            let text = std::fs::read_to_string(&failed_path).unwrap_or_default();
            let n = parse_accounts_text(&text, None).len();
            if n > 0 {
                return Ok((text, n));
            }
        }

        let accounts_path = work.join("accounts.txt");
        if !accounts_path.is_file() {
            return Err(AppError::NotFound(format!(
                "accounts.txt missing for job {job_id}"
            )));
        }
        let all = parse_accounts_text(
            &std::fs::read_to_string(&accounts_path).unwrap_or_default(),
            None,
        );
        let qoder_out = work.join("qoder-accounts.json");
        let grok_out = work.join("grok-accounts.json");
        let out_path = if grok_out.is_file() {
            grok_out
        } else {
            qoder_out
        };
        let ok_emails = success_emails_from_output(&out_path);
        let mut body = String::new();
        let mut n = 0usize;
        for (email, pass) in &all {
            if ok_emails.contains(&email.to_lowercase()) {
                continue;
            }
            body.push_str(email);
            body.push('|');
            body.push_str(pass);
            body.push('\n');
            n += 1;
        }
        if n == 0 {
            return Err(AppError::BadRequest(
                "no failed accounts to retry (all succeeded or empty job)".into(),
            ));
        }
        Ok((body, n))
    }

    pub async fn retry_failed(
        &self,
        job_id: &str,
        opts: RetryFarmRequest,
        pool: SqlitePool,
    ) -> AppResult<Value> {
        let (accounts, n) = self.failed_accounts_text(job_id)?;
        let provider_from_job = {
            let st = self.inner.lock().await;
            st.current
                .as_ref()
                .filter(|c| c.meta.id == job_id)
                .map(|c| c.meta.provider.clone())
                .or_else(|| {
                    st.history
                        .iter()
                        .find(|j| j.id == job_id)
                        .map(|j| j.provider.clone())
                })
        };
        let provider = opts
            .provider
            .clone()
            .or(provider_from_job)
            .or_else(|| {
                let work = self.data_dir.join("jobs").join(job_id);
                if work.join("grok-accounts.json").is_file() {
                    Some("grok-cli".into())
                } else {
                    Some("qoder".into())
                }
            });
        info!(from_job = %job_id, failed = n, "retrying failed farm accounts");
        let req = StartFarmRequest {
            accounts,
            default_password: None,
            provider,
            inject: opts.inject,
            headless: opts.headless,
            device_auth: opts.device_auth,
            skip_exchange: opts.skip_exchange,
            settle_secs: opts.settle_secs,
            auto_import: opts.auto_import,
            concurrency: opts.concurrency,
            account_retries: opts.account_retries,
            skip_existing: opts.skip_existing,
            account_delay: opts.account_delay,
            proxy_file: opts.proxy_file,
            imap_host: None,
            imap_user: None,
            imap_pass: None,
            captcha_mode: None,
            mail_mode: None,
        };
        let mut out = self.start(req, pool).await?;
        if let Some(obj) = out.as_object_mut() {
            obj.insert("retried_from".into(), json!(job_id));
            obj.insert("failed_count".into(), json!(n));
        }
        Ok(out)
    }
}

/// Where a farm subprocess may get its proxies. Decided authoritatively by the
/// admin "Automation proxy" toggle so the toggle wins over `req.proxy_file` and
/// over any farm-side `.env` (`QODER_/GROK_ PROXY_*`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FarmProxyPlan {
    /// Toggle off: run direct. Subprocess gets `--no-proxy` and its proxy env
    /// is neutralized, so a stray `.env` can never re-enable proxying.
    Disabled,
    /// Toggle on + explicit request proxy_file: use that file verbatim.
    File(String),
    /// Toggle on + no explicit file: build `proxies.txt` from active DB proxies.
    DbPool,
}

/// Pure decision: given the automation toggle and the request's optional
/// proxy_file, choose the proxy plan. No DB or IO so it is unit-testable and is
/// the single source of truth callers must obey.
pub fn plan_farm_proxy(automation_on: bool, req_proxy_file: Option<&str>) -> FarmProxyPlan {
    if !automation_on {
        return FarmProxyPlan::Disabled;
    }
    match req_proxy_file.map(str::trim).filter(|s| !s.is_empty()) {
        Some(f) => FarmProxyPlan::File(f.to_string()),
        None => FarmProxyPlan::DbPool,
    }
}

/// Single choke point every farm subprocess (farm AND dudul inject) must call.
/// Edit here to change proxy behavior for all spawn paths at once. Sets
/// `--proxy-file` when a file is chosen, or forces `--no-proxy` plus neutralized
/// proxy env when disabled so a stray `.env` cannot re-enable proxying behind
/// the operator's toggle. DbPool needs the caller to have written proxies.txt
/// and pass it as a File; on its own it applies nothing.
pub fn apply_farm_proxy(cmd: &mut Command, plan: &FarmProxyPlan) {
    let (args, blank_env) = farm_proxy_cmd_parts(plan);
    for a in args {
        cmd.arg(a);
    }
    for key in blank_env {
        cmd.env(key, "");
    }
}

/// Pure form of [`apply_farm_proxy`]: returns the CLI args to add and the env
/// keys to blank, so the decision is unit-testable without a live Command.
fn farm_proxy_cmd_parts(plan: &FarmProxyPlan) -> (Vec<String>, &'static [&'static str]) {
    match plan {
        FarmProxyPlan::File(f) => {
            let trimmed = f.trim();
            if trimmed.is_empty() {
                (Vec::new(), &[])
            } else {
                (vec!["--proxy-file".to_string(), trimmed.to_string()], &[])
            }
        }
        FarmProxyPlan::DbPool => (Vec::new(), &[]),
        FarmProxyPlan::Disabled => (vec!["--no-proxy".to_string()], FARM_PROXY_ENV_KEYS),
    }
}

/// Proxy-related env vars a farm subprocess reads. Neutralized (set empty) when
/// the automation toggle is off so `.env`/inherited values cannot leak through.
const FARM_PROXY_ENV_KEYS: &[&str] = &[
    "QODER_PROXY_FILE",
    "QODER_PROXY_URL",
    "QODER_PROXY_POOL",
    "QODER_PROXY_SHUFFLE",
    "GROK_PROXY_FILE",
    "GROK_PROXY_URL",
    "GROK_PROXY_POOL",
    "GROK_PROXY_SHUFFLE",
    "BATCHER_PROXY_URL",
];

fn is_stderr_noise(line: &str) -> bool {
    const NOISE: &[&str] = &[
        "LeakWarning",
        "unclosed transport",
        "I/O operation on closed pipe",
        "_ProactorBasePipeTransport.__del__",
        "BaseSubprocessTransport.__del__",
        "proactor_events.py",
        "base_subprocess.py",
        "windows_utils.py",
        "result = self.fn(*self.args, **self.kwargs)",
        "ResourceWarning",
    ];
    NOISE.iter().any(|n| line.contains(n))
}

fn normalize_farm_provider(raw: Option<&str>) -> &'static str {
    match raw.map(|s| s.trim().to_lowercase()).as_deref() {
        Some("grok-cli") | Some("grok") | Some("gcli") => "grok-cli",
        Some("qoder") | None | Some("") => "qoder",
        _ => "qoder",
    }
}

async fn db_provider_emails(
    pool: &SqlitePool,
    provider: &str,
) -> AppResult<std::collections::HashSet<String>> {
    let rows = db::list_accounts(pool, Some(provider), None).await?;
    let mut set = std::collections::HashSet::new();
    for acc in rows {
        if let Some(email) = acc.email.as_ref() {
            let e = email.trim().to_lowercase();
            if !e.is_empty() && e.contains('@') {
                set.insert(e);
            }
        }
    }
    Ok(set)
}

/// Write active, non-dead DB proxies as a `proxies.txt` in the job dir and
/// return its path, or None when there are no usable proxies. Shared by every
/// spawn path that resolves a `DbPool` plan.
async fn write_db_proxies(pool: &SqlitePool, work: &Path) -> Option<String> {
    let proxies = db::list_active_proxies(pool).await.ok()?;
    let live: Vec<_> = proxies.into_iter().filter(|p| p.health != "dead").collect();
    if live.is_empty() {
        return None;
    }
    let mut pbody = String::new();
    for p in &live {
        match (&p.username, &p.password) {
            (Some(u), Some(pw)) if !u.is_empty() => {
                pbody.push_str(&format!("{}:{}:{}:{}\n", p.host, p.port, u, pw));
            }
            _ => pbody.push_str(&format!("{}:{}\n", p.host, p.port)),
        }
    }
    let ppath = work.join("proxies.txt");
    std::fs::write(&ppath, pbody).ok()?;
    Some(ppath.to_string_lossy().to_string())
}

fn success_emails_from_output(path: &Path) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return set;
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return set;
    };
    let items = v
        .get("providerConnections")
        .and_then(|x| x.as_array())
        .cloned()
        .or_else(|| v.as_array().cloned())
        .unwrap_or_default();
    for item in items {
        let email = item
            .get("email")
            .and_then(|e| e.as_str())
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if email.is_empty() || !email.contains('@') {
            continue;
        }
        let provider = item
            .get("provider")
            .and_then(|p| p.as_str())
            .unwrap_or("");
        let has_pat = item
            .pointer("/providerSpecificData/personalToken")
            .and_then(|p| p.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false)
            || item
                .get("personalToken")
                .and_then(|p| p.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false);
        let has_access = item
            .get("accessToken")
            .and_then(|p| p.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false)
            || item
                .get("refreshToken")
                .and_then(|p| p.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false);
        if has_pat || has_access || provider == "grok-cli" || provider == "qoder" {
            if has_pat || has_access {
                set.insert(email);
            }
        }
    }
    set
}

fn job_public(meta: &FarmJob, log_count: usize) -> Value {
    let mut v = serde_json::to_value(meta).unwrap_or(json!({}));
    if let Some(obj) = v.as_object_mut() {
        obj.insert("log_count".into(), json!(log_count));
        obj.insert("status".into(), json!(meta.status.as_str()));
    }
    v
}

fn inject_job_public(meta: &InjectJob, log_count: usize) -> Value {
    let mut v = serde_json::to_value(meta).unwrap_or(json!({}));
    if let Some(obj) = v.as_object_mut() {
        obj.insert("log_count".into(), json!(log_count));
        obj.insert("status".into(), json!(meta.status.as_str()));
    }
    v
}

fn push_history(history: &mut VecDeque<FarmJob>, mut job: FarmJob) {
    job.log_count = 0;
    history.push_front(job);
    while history.len() > MAX_JOB_HISTORY {
        history.pop_back();
    }
}

fn push_inject_history(history: &mut VecDeque<InjectJob>, mut job: InjectJob) {
    job.log_count = 0;
    history.push_front(job);
    while history.len() > MAX_JOB_HISTORY {
        history.pop_back();
    }
}

fn parse_accounts_text(raw: &str, default_password: Option<&str>) -> Vec<(String, String)> {
    let default_password = default_password
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (email, pass) = if let Some((e, p)) = line.split_once('|') {
            let email = e.trim();
            let pass = p.trim();
            if pass.is_empty() {
                match default_password {
                    Some(dp) => (email, dp),
                    None => continue,
                }
            } else {
                (email, pass)
            }
        } else if let Some(at) = line.find('@') {
            if let Some(colon) = line[at..].find(':').map(|i| at + i) {
                let email = line[..colon].trim();
                let pass = line[colon + 1..].trim();
                if pass.is_empty() {
                    match default_password {
                        Some(dp) => (email, dp),
                        None => continue,
                    }
                } else {
                    (email, pass)
                }
            } else if let Some(dp) = default_password {
                (line, dp)
            } else {
                continue;
            }
        } else {
            continue;
        };
        if email.is_empty() || pass.is_empty() || !email.contains('@') {
            continue;
        }
        let key = email.to_lowercase();
        if seen.insert(key) {
            out.push((email.to_string(), pass.to_string()));
        }
    }
    out
}

fn parse_farm_line(line: &str) -> Option<Value> {
    let t = line.trim();
    if !t.starts_with('{') {
        return None;
    }
    serde_json::from_str(t).ok()
}

fn mask_email_for_events(email: &str) -> String {
    let email = email.trim();
    if let Some((local, domain)) = email.split_once('@') {
        if local.is_empty() {
            return format!("***@{domain}");
        }
        if local.len() <= 2 {
            return format!("{}***@{domain}", &local[..1]);
        }
        return format!(
            "{}***{}@{domain}",
            &local[..1],
            &local[local.len() - 1..]
        );
    }
    if email.len() <= 2 {
        return "***".into();
    }
    format!("{}***", &email[..1])
}

fn redact_farm_line_for_events(line: &str, parsed: &Option<Value>) -> String {
    let Some(v) = parsed else {
        return line.to_string();
    };
    let mut owned = v.clone();
    let Some(obj) = owned.as_object_mut() else {
        return line.to_string();
    };
    obj.remove("connection");
    obj.remove("personalToken");
    obj.remove("securityOauthToken");
    obj.remove("accessToken");
    obj.remove("refreshToken");
    if let Some(email) = obj.get("email").and_then(|e| e.as_str()) {
        if email.contains('@') && !email.contains('*') {
            let masked = mask_email_for_events(email);
            obj.insert("email".into(), json!(masked));
            if obj.get("email_masked").is_none() {
                obj.insert("email_masked".into(), json!(masked));
            }
        }
    }
    serde_json::to_string(&owned).unwrap_or_else(|_| line.to_string())
}

async fn import_one_email_from_output(
    path: &Path,
    email: &str,
    pool: &SqlitePool,
) -> AppResult<Value> {
    let want = email.trim().to_lowercase();
    if want.is_empty() || !want.contains('@') {
        return Err(AppError::BadRequest("account_ok missing email".into()));
    }
    let text = tokio::fs::read_to_string(path).await.map_err(|e| {
        AppError::Internal(format!("read farm output {}: {e}", path.display()))
    })?;
    let v: Value = serde_json::from_str(&text)?;
    let accounts = import_util::parse_9router_backup(&v);
    let acc = accounts
        .into_iter()
        .find(|a| {
            a.email
                .as_ref()
                .map(|e| e.trim().eq_ignore_ascii_case(&want))
                .unwrap_or(false)
        })
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "no connection for {want} in {}",
                path.display()
            ))
        })?;
    let (kind, saved) = db::upsert_account_returning(pool, &acc).await?;
    Ok(json!({
        "ok": true,
        "id": saved.id,
        "email": saved.email,
        "provider": saved.provider,
        "upsert": match kind {
            db::UpsertKind::Inserted => "inserted",
            db::UpsertKind::Updated => "updated",
        },
    }))
}

async fn append_log_file(path: &Path, text: &str) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    f.write_all(text.as_bytes()).await
}

async fn import_output_file(path: &Path, pool: &SqlitePool) -> AppResult<Value> {
    let text = tokio::fs::read_to_string(path).await?;
    let v: Value = serde_json::from_str(&text)?;
    let accounts = import_util::parse_9router_backup(&v);
    if accounts.is_empty() {
        return Ok(json!({
            "ok": true,
            "inserted": 0,
            "updated": 0,
            "skipped": 0,
            "parsed": 0,
            "path": path.display().to_string(),
            "note": "no supported connections in output",
        }));
    }
    let mut inserted = 0u32;
    let mut updated = 0u32;
    let mut skipped = 0u32;
    let mut source = "farm";
    for acc in &accounts {
        if acc.provider == "grok-cli" {
            source = "grok_farm";
        } else if acc.provider == "qoder" {
            source = "qoder_farm";
        }
    }
    for acc in accounts {
        match db::upsert_account(pool, &acc).await {
            Ok(db::UpsertKind::Inserted) => inserted += 1,
            Ok(db::UpsertKind::Updated) => updated += 1,
            Err(e) => {
                warn!(error = %e, id = %acc.id, "farm import skip");
                skipped += 1;
            }
        }
    }
    Ok(json!({
        "ok": true,
        "inserted": inserted,
        "updated": updated,
        "skipped": skipped,
        "parsed": inserted + updated + skipped,
        "path": path.display().to_string(),
        "source": source,
    }))
}

fn resolve_path(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        return path.canonicalize().unwrap_or(path);
    }
    let joined = std::env::current_dir()
        .map(|cwd| cwd.join(&path))
        .unwrap_or_else(|_| path.clone());
    joined.canonicalize().unwrap_or(joined)
}

fn strip_windows_verbatim(s: &str) -> String {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.to_string()
    }
}

fn path_for_display(path: &Path) -> String {
    let cleaned = PathBuf::from(strip_windows_verbatim(&path.display().to_string()));
    if let Ok(cwd) = std::env::current_dir() {
        let cwd_clean = PathBuf::from(strip_windows_verbatim(
            &cwd.canonicalize()
                .unwrap_or_else(|_| cwd.clone())
                .display()
                .to_string(),
        ));
        if let Ok(rel) = cleaned.strip_prefix(&cwd_clean) {
            let rel_s = rel.display().to_string();
            if !rel_s.is_empty() {
                return rel_s;
            }
        }
    }
    cleaned.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_path_makes_relative_absolute() {
        let p = resolve_path(PathBuf::from("scripts/automation/qoder_farm"));
        assert!(p.is_absolute(), "expected absolute, got {}", p.display());
        assert_eq!(
            p.file_name().and_then(|s| s.to_str()),
            Some("qoder_farm"),
            "unexpected path {}",
            p.display()
        );
    }

    #[test]
    fn resolve_path_keeps_absolute() {
        let abs = std::env::current_dir().unwrap().join("scripts");
        let p = resolve_path(abs);
        assert!(p.is_absolute());
    }

    #[test]
    fn stderr_noise_filters_asyncio_spam() {
        assert!(is_stderr_noise(
            "C:\\python-3.10\\lib\\concurrent\\futures\\thread.py:58: LeakWarning: When using a proxy"
        ));
        assert!(is_stderr_noise("ValueError: I/O operation on closed pipe"));
        assert!(is_stderr_noise(
            "Exception ignored in: <function _ProactorBasePipeTransport.__del__ at 0x0>"
        ));
        assert!(is_stderr_noise("  _warn(f\"unclosed transport {self!r}\", ResourceWarning)"));
    }

    #[test]
    fn stderr_noise_keeps_real_errors() {
        assert!(!is_stderr_noise("BrowserType.launch: Invalid URL"));
        assert!(!is_stderr_noise("camoufox import failed: No module named 'camoufox'"));
        assert!(!is_stderr_noise("Traceback (most recent call last):"));
        assert!(!is_stderr_noise("ConnectionError: proxy refused"));
    }

    #[test]
    fn package_parent_is_automation_dir() {
        let package_dir = resolve_path(PathBuf::from("scripts/automation/qoder_farm"));
        let parent = package_dir.parent().unwrap();
        assert_eq!(
            parent.file_name().and_then(|s| s.to_str()),
            Some("automation"),
            "parent={}",
            parent.display()
        );
        assert!(package_dir.join("__main__.py").is_file());
    }

    #[test]
    fn proxy_plan_off_is_disabled_even_with_file() {
        assert_eq!(plan_farm_proxy(false, None), FarmProxyPlan::Disabled);
        assert_eq!(
            plan_farm_proxy(false, Some("C:/proxies.txt")),
            FarmProxyPlan::Disabled
        );
    }

    #[test]
    fn proxy_plan_on_with_file_uses_file() {
        assert_eq!(
            plan_farm_proxy(true, Some("  C:/proxies.txt  ")),
            FarmProxyPlan::File("C:/proxies.txt".into())
        );
    }

    #[test]
    fn proxy_plan_on_without_file_uses_db_pool() {
        assert_eq!(plan_farm_proxy(true, None), FarmProxyPlan::DbPool);
        assert_eq!(plan_farm_proxy(true, Some("   ")), FarmProxyPlan::DbPool);
    }

    #[test]
    fn farm_proxy_env_keys_cover_both_farms() {
        for k in ["QODER_PROXY_FILE", "GROK_PROXY_FILE", "BATCHER_PROXY_URL"] {
            assert!(FARM_PROXY_ENV_KEYS.contains(&k), "missing {k}");
        }
    }

    #[test]
    fn apply_proxy_disabled_forces_no_proxy_and_blanks_env() {
        let (args, blank) = farm_proxy_cmd_parts(&FarmProxyPlan::Disabled);
        assert!(args.contains(&"--no-proxy".to_string()));
        assert!(!args.iter().any(|a| a == "--proxy-file"));
        assert_eq!(blank, FARM_PROXY_ENV_KEYS);
    }

    #[test]
    fn apply_proxy_file_passes_proxy_file_no_no_proxy() {
        let (args, blank) = farm_proxy_cmd_parts(&FarmProxyPlan::File("  C:/p.txt  ".into()));
        assert_eq!(args, vec!["--proxy-file".to_string(), "C:/p.txt".to_string()]);
        assert!(!args.iter().any(|a| a == "--no-proxy"));
        assert!(blank.is_empty());
    }

    #[test]
    fn apply_proxy_dbpool_applies_nothing() {
        let (args, blank) = farm_proxy_cmd_parts(&FarmProxyPlan::DbPool);
        assert!(args.is_empty());
        assert!(blank.is_empty());
    }

    #[test]
    fn normalize_farm_provider_maps_aliases() {
        assert_eq!(normalize_farm_provider(None), "qoder");
        assert_eq!(normalize_farm_provider(Some("qoder")), "qoder");
        assert_eq!(normalize_farm_provider(Some("grok-cli")), "grok-cli");
        assert_eq!(normalize_farm_provider(Some("grok")), "grok-cli");
        assert_eq!(normalize_farm_provider(Some("GCLI")), "grok-cli");
    }

    #[test]
    fn parse_accounts_pipe_and_colon() {
        let rows = parse_accounts_text(
            "a@x.com|secret1\nb@y.com:secret2\n# skip\n",
            None,
        );
        assert_eq!(
            rows,
            vec![
                ("a@x.com".into(), "secret1".into()),
                ("b@y.com".into(), "secret2".into()),
            ]
        );
    }

    #[test]
    fn parse_accounts_bare_email_needs_default_password() {
        assert!(parse_accounts_text("only@x.com\n", None).is_empty());
        let rows = parse_accounts_text("only@x.com\n", Some("shared"));
        assert_eq!(rows, vec![("only@x.com".into(), "shared".into())]);
    }

    #[test]
    fn parse_accounts_per_line_password_wins_over_default() {
        let rows = parse_accounts_text(
            "a@x.com|own\nb@y.com\nc@z.com|\n",
            Some("shared"),
        );
        assert_eq!(
            rows,
            vec![
                ("a@x.com".into(), "own".into()),
                ("b@y.com".into(), "shared".into()),
                ("c@z.com".into(), "shared".into()),
            ]
        );
    }

    #[test]
    fn parse_accounts_dedupes_case_insensitive() {
        let rows = parse_accounts_text(
            "A@x.com|one\na@x.com|two\n",
            None,
        );
        assert_eq!(rows, vec![("A@x.com".into(), "one".into())]);
    }

    #[test]
    fn grok_package_present_in_tree() {
        let package_dir = resolve_path(PathBuf::from("scripts/automation/grok_farm"));
        assert!(
            package_dir.join("__main__.py").is_file(),
            "missing {}",
            package_dir.display()
        );
    }

    #[test]
    fn clamp_concurrency_defaults_to_one() {
        assert_eq!(clamp_concurrency(None), 1);
        assert_eq!(clamp_concurrency(Some(0)), 1);
    }

    #[test]
    fn clamp_concurrency_respects_cap() {
        let cap = max_concurrency_cap();
        assert_eq!(clamp_concurrency(Some(cap)), cap);
        assert_eq!(clamp_concurrency(Some(cap.saturating_add(50))), cap);
        assert_eq!(clamp_concurrency(Some(3)).min(cap), 3.min(cap));
    }

    #[test]
    fn strip_windows_verbatim_prefix() {
        assert_eq!(
            strip_windows_verbatim(r"\\?\C:\Users\miqba\proj"),
            r"C:\Users\miqba\proj"
        );
        assert_eq!(
            strip_windows_verbatim(r"\\?\UNC\server\share"),
            r"\\server\share"
        );
        assert_eq!(strip_windows_verbatim(r"C:\plain"), r"C:\plain");
    }

    #[test]
    fn path_for_display_relative_under_cwd() {
        let p = resolve_path(PathBuf::from("scripts/automation/qoder_farm"));
        let shown = path_for_display(&p);
        assert!(
            !shown.contains(r"\\?\"),
            "verbatim leaked: {shown}"
        );
        assert!(
            shown.contains("qoder_farm"),
            "unexpected display path: {shown}"
        );
        // Prefer short relative form when package lives under process CWD.
        if shown.contains(':') {
            // absolute fallback still ok if strip_prefix failed
            assert!(PathBuf::from(strip_windows_verbatim(&shown)).is_absolute());
        } else {
            assert!(
                shown.replace('\\', "/").ends_with("scripts/automation/qoder_farm")
                    || shown.ends_with("qoder_farm"),
                "expected relative-ish path, got {shown}"
            );
        }
    }
}
