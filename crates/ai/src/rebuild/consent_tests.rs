use super::*;
use crate::agent::tests::{Scripted, turn};
use crate::agent::{ApiConversation, EngineHost, LoopConfig, ModelSlot};
use crate::model::{Block, StopReason};
use craft_anything_engine::Toolset;
use std::collections::{BTreeSet, VecDeque};
use std::sync::mpsc;

#[test]
fn runtime_tool_is_automatically_allowed_to_reach_validation() {
    let dir = tempfile::tempdir().unwrap();
    let mut ws = Workspace::create(dir.path(), "x", "Inspect the original", TargetRef::None).unwrap();
    let mut worker = super::tests::check_worker(&ws, true);
    worker.engine.session().settings.ask_before_running = true;
    worker.attach(&ws).unwrap();
    let program = dir.path().join("not-a-program");
    let input = json!({"program": program});
    let script = Scripted {
        turns: VecDeque::from([
            turn(vec![Block::ToolUse { id: "capture-1".into(), name: "native_window_capture".into(), input: input.clone() }], StopReason::ToolUse),
            turn(vec![Block::Text { text: "done".into() }], StopReason::EndTurn),
        ]),
        seen: Arc::default(),
    };
    let slot = ModelSlot {
        name: "Scripted".into(),
        model: Box::new(script),
        local: false,
        effort: None,
        max_turns: 10,
        context_tokens: 0,
        tools: Default::default(),
    };
    let enabled = BTreeSet::from([Toolset::Core, Toolset::Build, Toolset::Runtime]);
    let mut api = ApiConversation::start_with(Arc::new(EngineHost(worker.engine.clone())), vec![slot], LoopConfig::default(), enabled).unwrap();
    let (tx, rx) = mpsc::channel();
    worker.tx = tx;
    let stop = worker.stop.clone();
    let handle = std::thread::spawn(move || worker.converse_turn(&mut ws, &mut api, "Inspect the original", false));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut started = None;
    let mut finished = Vec::new();
    let mut asked = false;
    let mut done = false;
    while let Ok(event) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        match event {
            RunEvent::Agent(AgentEvent::ToolStarted { call_id, tool, input }) if call_id == "capture-1" => started = Some((tool, input)),
            RunEvent::Agent(AgentEvent::ToolFinished { call_id, ok, summary, .. }) if call_id == "capture-1" => finished.push((ok, summary)),
            RunEvent::Ask(_) => asked = true,
            RunEvent::Agent(AgentEvent::TurnDone { .. }) => {
                done = true;
                break;
            }
            _ => {}
        }
    }
    if !done {
        stop.cancel();
    }
    let result = handle.join().unwrap();
    assert!(done, "the runtime tool should not wait for a per-action approval");
    assert_eq!(result.unwrap(), "done");
    assert_eq!(started, Some(("native_window_capture".into(), input)));
    assert!(!asked, "automatic execution must not request per-action approval");
    assert_eq!(finished.len(), 1);
    assert!(!finished[0].0);
    assert!(finished[0].1.contains("isn't a program file"), "{:?}", finished[0]);
}
