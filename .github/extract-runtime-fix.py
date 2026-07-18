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

adapter = Path("crates/medusa-tui/src/runtime.rs")
text = adapter.read_text().replace(
    "medusa_runtime::AgentPlanStep {",
    "medusa_runtime::RuntimePlanStep {",
    1,
)
adapter.write_text(text)
