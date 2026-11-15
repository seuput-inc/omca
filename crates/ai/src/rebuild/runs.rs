//! Several rebuilds at once. A [`RunSet`] keeps every project's run by folder: up to its limit
//! work at the same time, the rest wait in order and start as others finish. Each run owns an
//! independent tool session and feed, regardless of backend or which project is on screen.

use super::*;
use std::collections::{HashMap, VecDeque};

/// How a run is started; [`RebuildRun::start`] unless a fork or a test swaps it.
pub type Starter = Box<dyn FnMut(&AiHub, Workspace, RunOptions) -> Result<RebuildRun, ToolError> + Send>;

/// What [`RunSet::start`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Started {
    Running,
    /// Waiting for a free place; 1 starts next.
    Queued {
        position: usize,
    },
}

/// What [`RunSet::stop`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// Asked to stop; its `Finished` event follows.
    Stopping,
    /// Taken out of the queue before it started.
    Unqueued,
    Nothing,
}

pub struct RunSet {
    limit: usize,
    /// Running and finished runs (a finished one keeps its preview and unsent notes).
    runs: HashMap<PathBuf, RebuildRun>,
    /// Paths observed at start, resolved to the OS's canonical project identity.
    aliases: HashMap<PathBuf, PathBuf>,
    /// Waiting runs, in arrival order.
    queue: VecDeque<(Workspace, RunOptions)>,
    starter: Starter,
    /// A [`keep_queue_moving`] thread is looking after the queue.
    watched: bool,
    managed_only: bool,
}

impl std::fmt::Debug for RunSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunSet").field("limit", &self.limit).field("running", &self.active()).field("queued", &self.queue.len()).finish()
    }
}

impl RunSet {
    /// At most `limit` runs work at once (at least one).
    pub fn new(limit: usize) -> RunSet {
        RunSet {
            limit: limit.max(1),
            runs: HashMap::new(),
            aliases: HashMap::new(),
            queue: VecDeque::new(),
            starter: Box::new(RebuildRun::start),
            watched: false,
            managed_only: false,
        }
    }

    fn has_room(&self) -> bool {
        self.active() < self.limit
    }

    /// Start runs another way (a fork's runner, or a test's).
    pub fn with_starter(mut self, starter: Starter) -> RunSet {
        self.starter = starter;
        self
    }

    /// Change how many may work at once; runs already working go on.
    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit.max(1);
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Start a run on `ws`, or queue it when `limit` are already working. Nothing overtakes the
    /// queue. A project that is working or queued is refused.
    pub fn start(&mut self, hub: &AiHub, mut ws: Workspace, opts: RunOptions) -> Result<Started, ToolError> {
        if let Ok(root) = std::fs::canonicalize(&ws.root) {
            self.aliases.insert(ws.root.clone(), root.clone());
            ws.root = root;
        }
        self.managed_only |= hub.managed_only;
        let mut scoped = AiHub::with_stores(hub.engine.clone(), hub.settings_path(), hub.keys.clone());
        scoped.managed_only = self.managed_only;
        let hub = if self.managed_only { &scoped } else { hub };
        if hub.managed_only {
            hub.selected_backend(&hub.config(), &ws, opts.backend.as_deref())?;
        }
        if self.is_running(&ws.root) || self.position(&ws.root).is_some() {
            return Err(
                ToolError::new(ErrorCode::BadInput, "This project is already building.").with_remediation("Add to it while it works, or stop it first.")
            );
        }
        if self.queue.is_empty() && self.has_room() {
            let root = ws.root.clone();
            let run = (self.starter)(hub, ws, opts)?;
            self.runs.insert(root, run);
            return Ok(Started::Running);
        }
        if !hub.managed_only {
            hub.selected_backend(&hub.config(), &ws, opts.backend.as_deref())?;
        }
        self.queue.push_back((ws, opts));
        Ok(Started::Queued { position: self.queue.len() })
    }

    /// Every run's new events, by project folder, then start queued runs in the places that came
    /// free. A queued run that can't start gives a `Finished { ok: false }` with the reason.
    pub fn poll(&mut self, hub: &AiHub) -> Vec<(PathBuf, RunEvent)> {
        self.managed_only |= hub.managed_only;
        let mut scoped = AiHub::with_stores(hub.engine.clone(), hub.settings_path(), hub.keys.clone());
        scoped.managed_only = self.managed_only;
        let hub = if self.managed_only { &scoped } else { hub };
        let mut out: Vec<(PathBuf, RunEvent)> = Vec::new();
        for (root, run) in &mut self.runs {
            out.extend(run.poll().into_iter().map(|e| (root.clone(), e)));
        }
        while !self.queue.is_empty() && self.has_room() {
            let Some((ws, opts)) = self.queue.pop_front() else { break };
            // Keep the workspace's saved conversation current; the run takes its own settings
            // and backend snapshot before it starts.
            let ws = Workspace::open(&ws.root).unwrap_or(ws);
            let root = ws.root.clone();
            let mut keep = ws.clone();
            let allowed = if hub.managed_only { hub.selected_backend(&hub.config(), &ws, opts.backend.as_deref()).map(|_| ()) } else { Ok(()) };
            match allowed.and_then(|()| (self.starter)(hub, ws, opts)) {
                Ok(run) => {
                    self.runs.insert(root, run);
                }
                Err(e) => {
                    let message = e.for_people();
                    if !(hub.managed_only && e.code == ErrorCode::BadSettings) {
                        let _ = keep.set_status(Status::NeedsYou, message.clone());
                    }
                    out.push((root, RunEvent::Finished { ok: false, message, paused: false }));
                }
            }
        }
        out
    }

    /// The project's run, working or finished.
    pub fn get(&self, root: &Path) -> Option<&RebuildRun> {
        let root = self.aliases.get(root).map_or(root, PathBuf::as_path);
        self.runs.get(root)
    }

    pub fn get_mut(&mut self, root: &Path) -> Option<&mut RebuildRun> {
        let root = self.aliases.get(root).map_or(root, PathBuf::as_path);
        self.runs.get_mut(root)
    }

    /// The project's run is working.
    pub fn is_running(&self, root: &Path) -> bool {
        self.get(root).is_some_and(RebuildRun::is_running)
    }

    /// Its place in the queue (1 starts next).
    pub fn position(&self, root: &Path) -> Option<usize> {
        let root = self.aliases.get(root).map_or(root, PathBuf::as_path);
        self.queue.iter().position(|(ws, _)| ws.root == root).map(|i| i + 1)
    }

    /// How many are waiting.
    pub fn waiting(&self) -> usize {
        self.queue.len()
    }

    /// Working or waiting.
    pub fn is_busy(&self, root: &Path) -> bool {
        self.is_running(root) || self.position(root).is_some()
    }

    /// How many are working.
    pub fn active(&self) -> usize {
        self.runs.values().filter(|r| r.is_running()).count()
    }

    /// Any project is working or waiting.
    pub fn any_busy(&self) -> bool {
        self.active() > 0 || !self.queue.is_empty()
    }

    /// The projects working or waiting.
    pub fn busy(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self.runs.iter().filter(|(_, r)| r.is_running()).map(|(p, _)| p.clone()).collect();
        v.extend(self.queue.iter().map(|(ws, _)| ws.root.clone()));
        v
    }

    /// Stop the project's run, or take it out of the queue.
    pub fn stop(&mut self, root: &Path) -> Stopped {
        let root = self.aliases.get(root).map_or(root, PathBuf::as_path);
        if let Some(i) = self.queue.iter().position(|(ws, _)| ws.root == root) {
            self.queue.remove(i);
            return Stopped::Unqueued;
        }
        match self.runs.get(root).filter(|r| r.is_running()) {
            Some(r) => {
                r.stop();
                Stopped::Stopping
            }
            None => Stopped::Nothing,
        }
    }

    /// Empty the queue and stop every run (closing the app).
    pub fn stop_all(&mut self) {
        self.queue.clear();
        for r in self.runs.values() {
            r.stop();
        }
    }
}

/// Poll `set` every `every` on a background thread while runs wait, so queued runs start as
/// soon as a place comes free: for holders that poll only when called (the rebuild tools, which
/// an MCP client may not call again for a while). `settings_path` is the session's config file,
/// read when the session is busy. One thread per set at a time; it ends when the queue is empty
/// or the engine is gone, and never waits on a busy session.
pub fn keep_queue_moving(set: &Arc<Mutex<RunSet>>, engine: &Arc<Engine>, settings_path: PathBuf, every: Duration) {
    {
        let mut s = set.lock().unwrap_or_else(PoisonError::into_inner);
        if s.watched || s.queue.is_empty() {
            return;
        }
        s.watched = true;
    }
    let (set_ref, engine_ref) = (Arc::downgrade(set), Arc::downgrade(engine));
    let spawned = std::thread::Builder::new().name("rebuild-queue".into()).spawn(move || {
        loop {
            std::thread::sleep(every);
            let Some(set) = set_ref.upgrade() else { return };
            let mut s = set.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(engine) = engine_ref.upgrade() else {
                s.watched = false;
                return;
            };
            if let Some(session) = engine.try_session() {
                s.set_limit(session.settings.build.parallel);
            }
            // Each run keeps its own project's feed, so the events aren't needed here.
            let hub = AiHub::with_stores(engine, settings_path.clone(), crate::KeyStore::system());
            let _ = s.poll(&hub);
            if s.queue.is_empty() {
                s.watched = false;
                return;
            }
        }
    });
    if spawned.is_err() {
        set.lock().unwrap_or_else(PoisonError::into_inner).watched = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rebuild::StandIns;

    #[cfg(unix)]
    #[test]
    fn project_aliases_cannot_start_twice_and_control_the_same_run() {
        let dir = tempfile::tempdir().unwrap();
        let a = Workspace::create(dir.path(), "Alpha", "A website", TargetRef::None).unwrap();
        let b = Workspace::create(dir.path(), "Beta", "A website", TargetRef::None).unwrap();
        let alias_a = dir.path().join("alias-alpha");
        let alias_b = dir.path().join("alias-beta");
        std::os::unix::fs::symlink(&a.root, &alias_a).unwrap();
        std::os::unix::fs::symlink(&b.root, &alias_b).unwrap();
        let fake = StandIns::new();
        let hub = hub();
        let mut set = RunSet::new(1).with_starter(fake.starter());
        assert_eq!(set.start(&hub, a.clone(), RunOptions::default()).unwrap(), Started::Running);
        let mut aliased_a = a.clone();
        aliased_a.root = alias_a.clone();
        assert!(set.start(&hub, aliased_a, RunOptions::default()).is_err());
        assert!(set.is_running(&alias_a));
        assert_eq!(set.start(&hub, b.clone(), RunOptions::default()).unwrap(), Started::Queued { position: 1 });
        let mut aliased_b = b.clone();
        aliased_b.root = alias_b.clone();
        assert!(set.start(&hub, aliased_b, RunOptions::default()).is_err());
        assert_eq!(set.position(&alias_b), Some(1));
        assert_eq!(Workspace::open(&alias_a).unwrap().root, a.root);
        assert_eq!(fake.started().as_slice(), std::slice::from_ref(&a.root));
        assert_eq!(set.stop(&alias_a), Stopped::Stopping);
        fake.finish(&a.root);
        set.poll(&hub);
        assert_eq!(fake.started(), [a.root, b.root]);
        assert!(set.is_running(&alias_b));
    }

    #[test]
    fn run_settings_changes_preserve_newer_global_values_and_language() {
        use craft_anything_engine::{CallOptions, Caller};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut session = craft_anything_engine::Session::isolated(dir.path());
        session.settings.deep_mode = true;
        session.settings.language = "en".into();
        session.settings.save_to(&path).unwrap();
        let engine = Engine::shared(session, Vec::new());
        let hub = AiHub::with_stores(engine.clone(), path.clone(), crate::KeyStore::file_only(dir.path().join("keys.toml")));
        let mut opts = RunOptions { response_language: "ko".into(), ..RunOptions::default() };
        let run = super::super::project_hub(&hub, &mut opts);
        engine.call("settings_set", serde_json::json!({"key":"build.parallel","value":4}), CallOptions::new(Caller::Cli)).unwrap();
        run.engine.call("settings_set", serde_json::json!({"key":"auto_mode","value":false}), CallOptions::new(Caller::Cli)).unwrap();
        let saved = craft_anything_engine::Settings::load_from(&path);
        assert_eq!((saved.build.parallel, saved.language.as_str(), saved.auto_mode), (4, "en", false));
        assert_eq!(opts.response_language, "ko");
        let ws = Workspace::create(dir.path(), "Remember", "A website", TargetRef::None).unwrap();
        engine.call("settings_set", serde_json::json!({"key":"theme","value":"studio-light"}), CallOptions::new(Caller::Cli)).unwrap();
        run.engine.session().remember_project(&ws.root).unwrap();
        let saved = craft_anything_engine::Settings::load_from(&path);
        assert_eq!(saved.theme, "studio-light");
        assert_eq!(saved.language, "en");
        assert!(saved.project_folders.contains(&ws.root));
    }

    #[test]
    fn custom_asset_providers_remain_available_in_an_isolated_project() {
        use craft_anything_engines::assets::{AssetProvider, AssetProviders, AssetRequest, Catalogue, Generated, Shape};
        struct Custom;
        impl AssetProvider for Custom {
            fn id(&self) -> &'static str {
                "custom"
            }
            fn name(&self) -> &'static str {
                "Custom"
            }
            fn generate(&self, _: &str, _: &AssetRequest) -> Result<Generated, ToolError> {
                Ok(Generated { url: String::new(), bytes: b"custom-picture".to_vec(), cost_usd: 0.0 })
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let mut session = craft_anything_engine::Session::isolated(dir.path());
        let mut providers = AssetProviders::built_in();
        providers.register(Arc::new(Custom));
        session.ext.insert(Arc::new(providers));
        let engine = Engine::shared(session, Vec::new());
        let hub = AiHub::with_stores(engine, dir.path().join("config.toml"), crate::KeyStore::file_only(dir.path().join("keys.toml")));
        let scoped = super::super::project_hub(&hub, &mut RunOptions::default());
        let parent = hub.engine.session();
        let mut opts = RunOptions { services: Some(craft_anything_engine::session::ToolServices::capture(&parent)), ..RunOptions::default() };
        let scoped_while_locked = super::super::project_hub(&hub, &mut opts);
        drop(parent);
        for scoped in [scoped, scoped_while_locked] {
            let providers = scoped.engine.session().ext.get::<AssetProviders>().map(|p| p.as_ref().clone()).unwrap_or_default();
            let mut model = Catalogue::built_in().image_model(None).unwrap().clone();
            model.provider = "custom".into();
            let request = AssetRequest::Image { model: &model, prompt: "an original shape", shape: Shape::Square };
            let picture = providers.for_request(&request).unwrap().generate("sk-test", &request).unwrap();
            assert_eq!(picture.bytes, b"custom-picture");
        }
    }

    #[test]
    fn overlapping_project_tools_keep_their_roots_sessions_and_cancellation() {
        use craft_anything_engine::{Ctx, Effects, Requires, ToolSpec, Toolset, tool};
        use std::sync::mpsc::{Receiver, SyncSender};
        struct Gate {
            entered: SyncSender<PathBuf>,
            release: Mutex<Receiver<()>>,
        }
        fn overlap(ctx: &mut Ctx<'_>, _: Value) -> Result<ToolOutput, ToolError> {
            let gate = ctx.session.ext.get::<Gate>().ok_or_else(|| ToolError::internal("Missing test gate"))?;
            let root = ctx.session.project.as_ref().ok_or_else(|| ToolError::internal("Missing project"))?.root.clone();
            gate.entered.send(root.clone()).map_err(|e| ToolError::internal(e.to_string()))?;
            gate.release.lock().unwrap().recv_timeout(Duration::from_secs(5)).map_err(|e| ToolError::internal(e.to_string()))?;
            Ok(ToolOutput::Value(serde_json::json!({"project": root})))
        }
        let dir = tempfile::tempdir().unwrap();
        let mut session = craft_anything_engine::Session::isolated(dir.path());
        session.settings.deep_mode = true;
        let engine = Engine::shared(
            session,
            vec![ToolSpec {
                id: "test_overlap",
                title: "Overlap",
                summary: "Test",
                beginner: "Test",
                toolset: Toolset::Build,
                effects: Effects::READ_ONLY,
                requires: Requires::Nothing,
                input_schema: tool::empty_schema,
                run: overlap,
            }],
        );
        let hub = AiHub::with_stores(engine.clone(), dir.path().join("config.toml"), crate::KeyStore::file_only(dir.path().join("keys.toml"))).managed_only();
        let a = Workspace::create(dir.path(), "Alpha", "A website", TargetRef::None).unwrap();
        let b = Workspace::create(dir.path(), "Beta", "A website", TargetRef::None).unwrap();
        let (entered, waiting) = std::sync::mpsc::sync_channel(2);
        let mut releases = Vec::new();
        let mut threads = Vec::new();
        for ws in [a.clone(), b.clone()] {
            let mut opts = RunOptions::default();
            let scoped = super::super::project_hub(&hub, &mut opts);
            let mut worker = super::super::tests::check_worker(&ws, false);
            worker.engine = scoped.engine;
            worker.attach(&ws).unwrap();
            let (release, wait) = std::sync::mpsc::channel();
            worker.engine.session().ext.insert(Arc::new(Gate { entered: entered.clone(), release: Mutex::new(wait) }));
            releases.push(release);
            threads.push(std::thread::spawn(move || {
                let out = worker.call("test_overlap", Value::Null).unwrap().to_json();
                assert_eq!(out["result"]["project"], serde_json::json!(ws.investigation_dir()));
                worker.call("build_file_write", serde_json::json!({"path":"app/owner.txt", "content": ws.manifest.name})).unwrap();
                worker
            }));
        }
        let mut roots = vec![waiting.recv_timeout(Duration::from_secs(5)).unwrap(), waiting.recv_timeout(Duration::from_secs(5)).unwrap()];
        roots.sort();
        let mut expected = vec![a.investigation_dir(), b.investigation_dir()];
        expected.sort();
        assert_eq!(roots, expected, "both tool calls overlap while holding different session locks");
        for release in releases {
            release.send(()).unwrap();
        }
        let mut workers: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(std::fs::read_to_string(a.app_dir().join("owner.txt")).unwrap(), "Alpha");
        assert_eq!(std::fs::read_to_string(b.app_dir().join("owner.txt")).unwrap(), "Beta");
        let a_worker = workers.remove(0);
        let b_worker = workers.remove(0);
        a_worker.stop.cancel();
        assert_eq!(a_worker.call("build_file_write", serde_json::json!({"path":"app/after.txt","content":"A"})).unwrap_err().code, ErrorCode::Cancelled);
        b_worker.call("build_file_write", serde_json::json!({"path":"app/after.txt","content":"B"})).unwrap();
        assert!(!a.app_dir().join("after.txt").exists());
        assert_eq!(std::fs::read_to_string(b.app_dir().join("after.txt")).unwrap(), "B");
        assert!(engine.session().project.is_none(), "the app's project was never replaced");
        assert!(!Arc::ptr_eq(&a_worker.engine, &b_worker.engine));
        assert!(a_worker.engine.session().ext.get::<crate::ManagedExecution>().is_some());
    }

    /// A hub whose Builder is an agent app, so queued runs pass the up-front check.
    fn hub() -> AiHub {
        let mut s = craft_anything_engine::testing::session();
        s.settings.ai = toml::from_str("[[backends]]\nname = \"Agent\"\nkind = \"agent_cli\"\nmodel = \"claude-code\"\n").unwrap();
        assert!(builder_backend(&crate::AiConfig::from_settings(&s.settings), None).is_ok());
        AiHub::new(Arc::new(Engine::new(s)))
    }

    #[test]
    fn queued_managed_run_cannot_be_downgraded_by_a_reconstructed_hub() {
        let d = tempfile::tempdir().unwrap();
        let mut session = craft_anything_engine::testing::session();
        session.settings.ai = toml::from_str(
            "[[backends]]\nname='Codex'\nkind='managed'\nmodel='chosen'\nprovider_id='openai'\n[[backends]]\nname='Gemini'\nkind='agent_cli'\nmodel='gemini-cli'\n",
        ).unwrap();
        let engine = Arc::new(Engine::new(session));
        let hub = AiHub::new(engine.clone()).managed_only();
        let unscoped = AiHub::new(engine);
        let a = Workspace::create(d.path(), "A", "A game", TargetRef::None).unwrap();
        let mut b = Workspace::create(d.path(), "B", "A game", TargetRef::None).unwrap();
        let fake = StandIns::new();
        let mut set = RunSet::new(1).with_starter(fake.starter());
        assert_eq!(set.start(&hub, a.clone(), RunOptions::default()).unwrap(), Started::Running);
        assert_eq!(set.start(&hub, b.clone(), RunOptions::default()).unwrap(), Started::Queued { position: 1 });
        b.set_person(|m| m.builder = Some("Gemini".into())).unwrap();
        fake.finish(&a.root);
        let events = set.poll(&unscoped);
        assert_eq!(fake.started(), [a.root]);
        assert!(events.iter().any(|(root, event)| *root == b.root && matches!(event, RunEvent::Finished { ok: false, .. })));
        assert_eq!(Workspace::open(&b.root).unwrap().manifest.builder.as_deref(), Some("Gemini"));
    }

    #[test]
    fn runs_past_the_limit_wait_and_start_in_order() {
        let d = tempfile::tempdir().unwrap();
        let ws = |n: &str| Workspace::create(d.path(), n, "A game", TargetRef::None).unwrap();
        let (a, b, c) = (ws("A"), ws("B"), ws("C"));
        let fake = StandIns::new();
        let hub = hub();
        let mut set = RunSet::new(2).with_starter(fake.starter());
        assert_eq!(set.start(&hub, a.clone(), RunOptions::default()).unwrap(), Started::Running);
        assert_eq!(set.start(&hub, b.clone(), RunOptions::default()).unwrap(), Started::Running);
        assert_eq!(set.start(&hub, c.clone(), RunOptions::default()).unwrap(), Started::Queued { position: 1 });
        assert!(set.start(&hub, c.clone(), RunOptions::default()).is_err(), "a queued project isn't queued twice");
        assert!(set.start(&hub, a.clone(), RunOptions::default()).is_err(), "nor a working one started twice");
        assert_eq!((set.active(), set.position(&c.root), set.busy().len()), (2, Some(1), 3));

        // A finishes: its event is reported under its folder, and C takes its place.
        fake.finish(&a.root);
        let events = set.poll(&hub);
        assert_eq!(events, [(a.root.clone(), RunEvent::Finished { ok: true, message: "Done.".into(), paused: false })]);
        assert_eq!(fake.started(), [a.root.clone(), b.root.clone(), c.root.clone()]);
        assert!(set.is_running(&c.root) && !set.is_running(&a.root) && set.get(&a.root).is_some(), "a finished run is kept");
        // A can build again, and waits behind the two working.
        assert_eq!(set.start(&hub, a.clone(), RunOptions::default()).unwrap(), Started::Queued { position: 1 });
        assert_eq!(set.stop(&a.root), Stopped::Unqueued);
        assert_eq!(set.stop(&b.root), Stopped::Stopping);
        assert_eq!(set.stop(&a.root), Stopped::Nothing);
        set.stop_all();
        assert!(set.position(&a.root).is_none());
    }

    #[test]
    fn managed_builds_run_together_with_the_same_queue_and_duplicate_guards() {
        let d = tempfile::tempdir().unwrap();
        let mut session = craft_anything_engine::testing::session();
        session.settings.ai = toml::from_str("[[backends]]\nname='Connected'\nkind='managed'\nmodel='chosen'\nprovider_id='openai'\n").unwrap();
        let hub = AiHub::new(Arc::new(Engine::new(session))).managed_only();
        let ws = |name| Workspace::create(d.path(), name, "A website", TargetRef::None).unwrap();
        let (a, b, c) = (ws("A"), ws("B"), ws("C"));
        let fake = StandIns::new();
        let mut set = RunSet::new(2).with_starter(fake.starter());
        assert_eq!(set.start(&hub, a.clone(), RunOptions::default()).unwrap(), Started::Running);
        assert_eq!(set.start(&hub, b.clone(), RunOptions::default()).unwrap(), Started::Running);
        assert_eq!(set.start(&hub, c.clone(), RunOptions::default()).unwrap(), Started::Queued { position: 1 });
        assert!(set.start(&hub, a.clone(), RunOptions::default()).is_err());
        assert!(set.start(&hub, c.clone(), RunOptions::default()).is_err());
        assert_eq!(set.stop(&a.root), Stopped::Stopping);
        assert!(set.is_running(&b.root), "stopping A never stops B");
        fake.finish(&a.root);
        set.poll(&hub);
        assert!(set.is_running(&b.root) && set.is_running(&c.root));
        assert_eq!(fake.started(), [a.root, b.root, c.root]);
        set.stop_all();
    }

    #[test]
    fn a_queued_run_that_cannot_start_says_why() {
        let d = tempfile::tempdir().unwrap();
        let a = Workspace::create(d.path(), "A", "A game", TargetRef::None).unwrap();
        let b = Workspace::create(d.path(), "B", "A game", TargetRef::None).unwrap();
        let fake = StandIns::new().failing("B");
        let hub = hub();
        let mut set = RunSet::new(1).with_starter(fake.starter());
        set.start(&hub, a.clone(), RunOptions::default()).unwrap();
        assert_eq!(set.start(&hub, b.clone(), RunOptions::default()).unwrap(), Started::Queued { position: 1 });
        fake.finish(&a.root);
        let events = set.poll(&hub);
        assert!(events.contains(&(b.root.clone(), RunEvent::Finished { ok: false, message: "No coding agent.".into(), paused: false })), "{events:?}");
        assert!(!set.any_busy());
        assert_eq!(Workspace::open(&b.root).unwrap().manifest.status, Status::NeedsYou);
    }

    #[test]
    fn a_queued_run_starts_without_anyone_polling() {
        let d = tempfile::tempdir().unwrap();
        let a = Workspace::create(d.path(), "A", "A game", TargetRef::None).unwrap();
        let b = Workspace::create(d.path(), "B", "A game", TargetRef::None).unwrap();
        let fake = StandIns::new();
        let hub = hub();
        let settings_path = hub.engine.session().config_dir().join("config.toml");
        let set = Arc::new(Mutex::new(RunSet::new(1).with_starter(fake.starter())));
        set.lock().unwrap().start(&hub, a.clone(), RunOptions::default()).unwrap();
        // Nothing waits yet, so no thread is started.
        keep_queue_moving(&set, &hub.engine, settings_path.clone(), Duration::from_millis(5));
        assert!(!set.lock().unwrap().watched);
        set.lock().unwrap().start(&hub, b.clone(), RunOptions::default()).unwrap();
        keep_queue_moving(&set, &hub.engine, settings_path.clone(), Duration::from_millis(5));
        keep_queue_moving(&set, &hub.engine, settings_path, Duration::from_millis(5));
        assert!(set.lock().unwrap().watched);
        fake.finish(&a.root);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while set.lock().unwrap().watched && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(fake.started(), [a.root.clone(), b.root.clone()], "one thread started B once");
        let s = set.lock().unwrap();
        assert!(s.is_running(&b.root) && s.waiting() == 0 && !s.watched);
    }

    #[test]
    fn nothing_is_queued_without_a_coding_agent() {
        let d = tempfile::tempdir().unwrap();
        let a = Workspace::create(d.path(), "A", "A game", TargetRef::None).unwrap();
        let b = Workspace::create(d.path(), "B", "A game", TargetRef::None).unwrap();
        let hub = AiHub::new(Arc::new(Engine::new(craft_anything_engine::testing::session())));
        let mut set = RunSet::new(1).with_starter(StandIns::new().starter());
        set.start(&hub, a, RunOptions::default()).unwrap();
        let root = b.root.clone();
        assert!(set.start(&hub, b, RunOptions::default()).is_err());
        assert_eq!(set.position(&root), None);
    }
}
