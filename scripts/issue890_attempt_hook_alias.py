from pathlib import Path

path = Path("crates/medusa-provider/src/manager.rs")
text = path.read_text()
anchor = '''enum RetryDisposition {
    Retry,
    Failover,
    Permanent,
}

'''
alias = '''enum RetryDisposition {
    Retry,
    Failover,
    Permanent,
}

type ProviderAttemptHook<'a> =
    &'a mut dyn FnMut(&ProviderAttemptDescriptor) -> MedusaResult<()>;

'''
if text.count(anchor) != 1:
    raise SystemExit(f"expected retry disposition anchor once, found {text.count(anchor)}")
text = text.replace(anchor, alias, 1)
old = "        mut before_attempt: Option<&mut dyn FnMut(&ProviderAttemptDescriptor) -> MedusaResult<()>>,\n"
new = "        mut before_attempt: Option<ProviderAttemptHook<'_>>,\n"
if text.count(old) != 1:
    raise SystemExit(f"expected formatted callback parameter once, found {text.count(old)}")
path.write_text(text.replace(old, new, 1))
