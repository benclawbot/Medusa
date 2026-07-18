from pathlib import Path

runtime = Path("crates/medusa-runtime/src/lib.rs")
text = runtime.read_text()
text = text.replace(
    "pub use commands::{Effort, ModelCommand, ModelConfiguration, SlashCommand};\n",
    "",
    1,
)
text = text.replace(
    "pub use medusa_agent::{AgentPlanStepStatus, AgentQuestionItem, AgentQuestionOption};",
    "pub use medusa_agent::{\n"
    "    AgentPlanStep as RuntimePlanStep, AgentPlanStepStatus, AgentQuestionItem,\n"
    "    AgentQuestionOption,\n"
    "};",
    1,
)
text = text.replace(
    "pub use prompt::{\n"
    "    ClipboardContent, ClipboardError, ClipboardImage, FileAttachment, ImageAttachment,\n"
    "    PromptAttachment, PromptDraft, TextAttachment,\n"
    "};\n",
    "",
    1,
)
runtime.write_text(text)

tests = Path("crates/medusa-runtime/src/tests.rs")
text = tests.read_text()
text = text.replace(
    "use std::sync::mpsc;\n",
    "use std::sync::{atomic::AtomicBool, mpsc};\n",
    1,
)
needle = "use super::support::{UpdateState, forward_update, message_blocks};\n"
imports = "use crate::prompt::{ImageAttachment, PromptAttachment};\n\n"
assert needle in text
if imports not in text:
    text = text.replace(needle, imports + needle, 1)
slash_test = '''
#[test]
fn effort_command_updates_the_runtime_turn_budget() {
    let directory = tempdir().expect("temporary directory");
    let mut state = RuntimeState::load(directory.path().to_path_buf()).expect("runtime state");
    let (sender, receiver) = mpsc::channel();

    execute_slash_command(
        &mut state,
        SlashCommand::Effort {
            effort: Some(Effort::Medium),
        },
        &sender,
        &AtomicBool::new(false),
    )
    .expect("set effort");

    assert_eq!(state.config.agent.max_turns, 200);
    assert!(matches!(
        receiver.recv().expect("settings update"),
        RuntimeEvent::Settings { effort, .. } if effort == "effort:medium"
    ));
}
'''
if "fn effort_command_updates_the_runtime_turn_budget()" not in text:
    text += slash_test
tests.write_text(text)

adapter = Path("crates/medusa-tui/src/runtime.rs")
text = adapter.read_text().replace(
    "medusa_runtime::AgentPlanStep {",
    "medusa_runtime::RuntimePlanStep {",
    1,
)
adapter.write_text(text)
