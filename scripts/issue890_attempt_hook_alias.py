from pathlib import Path

path = Path("crates/medusa-provider/src/manager.rs")
text = path.read_text()
old = '''enum RetryDisposition {\n    Retry,\n    Failover,\n    Permanent,\n}\n\n#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]\n'''
new = '''enum RetryDisposition {\n    Retry,\n    Failover,\n    Permanent,\n}\n\ntype ProviderAttemptHook<'a> =\n    &'a mut dyn FnMut(&ProviderAttemptDescriptor) -> MedusaResult<()>;\n\n#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]\n'''
if text.count(old) != 1:
    raise SystemExit(f"expected retry disposition anchor once, found {text.count(old)}")
text = text.replace(old, new, 1)
old_param = '''        mut before_attempt: Option<\n            &mut dyn FnMut(&ProviderAttemptDescriptor) -> MedusaResult<()>,\n        >,\n'''
new_param = '''        mut before_attempt: Option<ProviderAttemptHook<'_>>,\n'''
if text.count(old_param) != 1:
    raise SystemExit(f"expected complex callback parameter once, found {text.count(old_param)}")
path.write_text(text.replace(old_param, new_param, 1))
