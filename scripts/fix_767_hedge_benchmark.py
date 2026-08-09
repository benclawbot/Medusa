from pathlib import Path

path = Path("crates/medusa-provider/src/hedge_runtime.rs")
text = path.read_text(encoding="utf-8")

old_import = "    use crate::{ProviderCapabilities, ResponseBlock, RouteRetryPolicy, Usage};\n"
new_import = "    use crate::{\n        ProviderCapabilities, ResponseBlock, RouteRetryPolicy, Usage,\n        assess_hedge_latency_acceptance,\n    };\n"
if text.count(old_import) != 1:
    raise SystemExit("expected one hedge runtime test import")
text = text.replace(old_import, new_import, 1)

anchor = """    #[test]\n    fn primary_completion_before_threshold_never_launches_secondary() {\n"""
benchmark = """    #[test]\n    fn injected_tail_latency_benchmark_enforces_p95_improvement() {\n        let primary_delays = [\n            40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40,\n            160, 180,\n        ];\n        let mut baseline_ms = Vec::with_capacity(primary_delays.len());\n        let mut hedged_ms = Vec::with_capacity(primary_delays.len());\n\n        for primary_delay_ms in primary_delays {\n            let baseline_provider = provider(\"baseline-primary\", primary_delay_ms);\n            let baseline_started = Instant::now();\n            baseline_provider\n                .complete(&request())\n                .expect(\"baseline provider completion\");\n            baseline_ms.push(elapsed_ms(baseline_started));\n\n            let providers = vec![\n                provider(\"hedged-primary\", primary_delay_ms),\n                provider(\"hedged-secondary\", 5),\n            ];\n            let profiles = vec![profile(\"hedged-primary\"), profile(\"hedged-secondary\")];\n            let mut sink: Option<&mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>> = None;\n            let hedged_started = Instant::now();\n            race_provider_candidates(\n                &providers,\n                &profiles,\n                &request(),\n                HedgeRacePlan {\n                    primary_index: 0,\n                    secondary_index: 1,\n                    launch_after_ms: 15,\n                },\n                None,\n                &mut sink,\n            )\n            .expect(\"hedged provider completion\");\n            hedged_ms.push(elapsed_ms(hedged_started));\n        }\n\n        let assessment = assess_hedge_latency_acceptance(&baseline_ms, &hedged_ms);\n        assert!(\n            assessment.passed,\n            \"measured hedge p95 acceptance failed: {assessment:?}; baseline={baseline_ms:?}; hedged={hedged_ms:?}\"\n        );\n        eprintln!(\n            \"measured hedge acceptance: baseline_p95_ms={:?} hedged_p95_ms={:?} improvement_bps={:?}\",\n            assessment.baseline_p95_ms, assessment.hedged_p95_ms, assessment.improvement_bps\n        );\n    }\n\n"""
if text.count(anchor) != 1:
    raise SystemExit("expected one benchmark insertion anchor")
text = text.replace(anchor, benchmark + anchor, 1)
path.write_text(text, encoding="utf-8")
