from pathlib import Path

path = Path("crates/medusa-cli/src/main.rs")
text = path.read_text()
replacements = [
    ("mod config_command;\n", "mod config_command;\nmod headless_approval;\n"),
    (
        "use medusa_hardening::{CURRENT_SCHEMA_VERSION, Migrator};\n",
        "use medusa_hardening::{CURRENT_SCHEMA_VERSION, Migrator};\nuse headless_approval::{ApprovalMatch, HeadlessApprovalPolicy};\n",
    ),
    (
        "    Run {\n        objective: String,\n    },",
        '''    Run {
        objective: String,
        /// Answer only allowlisted approval prompts without opening the interactive terminal.
        #[arg(long, requires = "approve_allowlist")]
        non_interactive: bool,
        /// File containing one exact shell command per line.
        #[arg(long, value_name = "PATH", requires = "non_interactive")]
        approve_allowlist: Option<PathBuf>,
    },''',
    ),
    (
        "        CommandKind::Run { objective } => {\n",
        '''        CommandKind::Run {
            objective,
            non_interactive,
            approve_allowlist,
        } => {
''',
    ),
    (
        "            let runtime = RuntimeController::start_with_config(repo, config);\n",
        '''            let approval_policy = HeadlessApprovalPolicy::load(
                non_interactive,
                approve_allowlist.as_deref(),
            )?;
            let runtime = RuntimeController::start_with_config(repo, config);
''',
    ),
    (
        "            drain_headless_runtime(&runtime)\n        }\n        CommandKind::Resume",
        "            drain_headless_runtime(&runtime, approval_policy.as_ref())\n        }\n        CommandKind::Resume",
    ),
    (
        "            drain_headless_runtime(&runtime)\n        }\n        CommandKind::Config",
        "            drain_headless_runtime(&runtime, None)\n        }\n        CommandKind::Config",
    ),
    (
        "fn drain_headless_runtime(runtime: &RuntimeController) -> MedusaResult<()> {",
        "fn drain_headless_runtime(\n    runtime: &RuntimeController,\n    approval_policy: Option<&HeadlessApprovalPolicy>,\n) -> MedusaResult<()> {",
    ),
    (
        '''            Some(RuntimeEvent::Question(question)) => {
                return Err(MedusaError::new(
                    ErrorCode::DependencyUnavailable,
                    ErrorCategory::Execution,
                    format!(
                        "agent is waiting for user input, which headless execution cannot provide: {}. \\
                         Approval prompts can only be answered in the interactive terminal, so rerun \\
                         this objective with `medusa` instead of `medusa run`.",
                        question
                            .prompts()
                            .first()
                            .map(|item| item.question.as_str())
                            .unwrap_or("question details unavailable")
                    ),
                ));
            }
''',
        '''            Some(RuntimeEvent::Question(question)) => {
                if let Some(policy) = approval_policy {
                    match policy.matches(&question) {
                        ApprovalMatch::Approved(command) => {
                            println!("approved allowlisted command: {command}");
                            runtime
                                .submit(PromptDraft {
                                    text: "approve".to_owned(),
                                    ..PromptDraft::default()
                                })
                                .map_err(runtime_error)?;
                            continue;
                        }
                        ApprovalMatch::Missing(command) => {
                            return Err(MedusaError::new(
                                ErrorCode::PolicyDenied,
                                ErrorCategory::Policy,
                                format!(
                                    "headless approval denied for `{command}` because it is not listed in {}. Add the exact command and rerun with `medusa run --non-interactive --approve-allowlist {} <objective>`.",
                                    policy.source().display(),
                                    policy.source().display()
                                ),
                            ));
                        }
                        ApprovalMatch::NotApproval => {}
                    }
                }
                return Err(MedusaError::new(
                    ErrorCode::DependencyUnavailable,
                    ErrorCategory::Execution,
                    format!(
                        "agent is waiting for user input, which headless execution cannot provide: {}. For an approval prompt, create an allowlist and rerun with `medusa run --non-interactive --approve-allowlist .medusa/approve.txt <objective>`; otherwise use the interactive terminal.",
                        question
                            .prompts()
                            .first()
                            .map(|item| item.question.as_str())
                            .unwrap_or("question details unavailable")
                    ),
                ));
            }
''',
    ),
    (
        'matches!(cli.command, Some(CommandKind::Run { objective }) if objective == "fix tests")',
        'matches!(cli.command, Some(CommandKind::Run { objective, .. }) if objective == "fix tests")',
    ),
]
for old, new in replacements:
    if text.count(old) != 1:
        raise SystemExit(f"expected one main.rs match, found {text.count(old)} for {old[:80]!r}")
    text = text.replace(old, new, 1)
marker = '''    #[test]
    fn interactive_resume_flags_are_parsed() {
'''
extra = '''    #[test]
    fn headless_run_accepts_explicit_approval_allowlist() {
        let cli = Cli::try_parse_from([
            "medusa",
            "run",
            "--non-interactive",
            "--approve-allowlist",
            ".medusa/approve.txt",
            "fix tests",
        ])
        .expect("parse allowlisted headless run");
        assert!(matches!(
            cli.command,
            Some(CommandKind::Run {
                objective,
                non_interactive: true,
                approve_allowlist: Some(path),
            }) if objective == "fix tests" && path == PathBuf::from(".medusa/approve.txt")
        ));
    }

'''
if text.count(marker) != 1:
    raise SystemExit("test insertion marker missing")
path.write_text(text.replace(marker, extra + marker, 1))

policy = Path("crates/medusa-cli/src/headless_approval.rs")
policy_text = policy.read_text()
policy_text = policy_text.replace("    use time::OffsetDateTime;\n", "")
policy_text = policy_text.replace(
    "    use medusa_agent::{ApprovalGrant, AgentPlanStep, AgentPlanStepStatus};\n", ""
)
policy_text = policy_text.replace(
    '''        let plan = vec![AgentPlanStep {
            title: "Verify the fix".to_owned(),
            status: AgentPlanStepStatus::InProgress,
        }];
''',
    "",
)
old = '''                "grant": ApprovalGrant::exact_action(
                    "shell_run",
                    &json!({"program": program, "args": args}),
                    &plan,
                    OffsetDateTime::now_utc()
                )
'''
new = '''                "grant": {
                    "scope": {
                        "tool": "shell_run",
                        "action_fingerprint": "fixture-action",
                        "plan_fingerprint": "fixture-plan"
                    },
                    "approved_at": "2026-07-27T12:00:00Z",
                    "expires_at": "2026-07-27T12:05:00Z"
                }
'''
if policy_text.count(old) != 1:
    raise SystemExit("approval grant fixture match missing")
policy.write_text(policy_text.replace(old, new, 1))
