from pathlib import Path

path = Path("crates/medusa-agent/src/lib.rs")
text = path.read_text()
old = "    AgentPlanStep, AgentPlanStepStatus, AgentQuestion, AgentQuestionItem, AgentQuestionOption,\n    AgentSession, bootstrap,\n"
new = "    AgentPlanStep, AgentPlanStepStatus, AgentQuestion, AgentQuestionItem, AgentQuestionOption,\n    AgentSession, SessionUsage, TurnUsage, UsageProvenance, bootstrap, session_usage,\n"
if text.count(old) != 1:
    raise SystemExit(f"expected one session re-export block, found {text.count(old)}")
path.write_text(text.replace(old, new, 1))
Path("scripts/apply-usage-export-fix.py").unlink()
Path(".github/workflows/apply-usage-export-fix.yml").unlink()
