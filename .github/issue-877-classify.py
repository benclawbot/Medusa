from pathlib import Path

path = Path('crates/medusa-runtime/src/roadblock_recovery.rs')
text = path.read_text()
old = '''    if contains_any(&text, &["permission denied", "not permitted", "forbidden", "policy", "approval required"]) {
        return Some(RoadblockClass::PermissionPolicy);
    }
    if contains_any(&text, &["command not found", "not installed", "unsupported platform", "unavailable tool", "missing capability"]) {
        return Some(RoadblockClass::MissingCapability);
    }
    if contains_any(&text, &["connection refused", "service unavailable", "dependency unavailable", "offline", "dns"]) {
        return Some(RoadblockClass::DependencyUnavailable);
    }
    if contains_any(&text, &["breaking change", "public api", "architecture", "compatibility", "forbidden dependency"]) {
        return Some(RoadblockClass::ArchitectureCompatibility);
    }
'''
new = '''    if contains_any(&text, &["command not found", "not installed", "unsupported platform", "unavailable tool", "missing capability"]) {
        return Some(RoadblockClass::MissingCapability);
    }
    if contains_any(&text, &["connection refused", "service unavailable", "dependency unavailable", "offline", "dns"]) {
        return Some(RoadblockClass::DependencyUnavailable);
    }
    if contains_any(&text, &["breaking change", "public api", "architecture", "compatibility", "forbidden dependency"]) {
        return Some(RoadblockClass::ArchitectureCompatibility);
    }
    if contains_any(&text, &["permission denied", "not permitted", "forbidden", "policy", "approval required"]) {
        return Some(RoadblockClass::PermissionPolicy);
    }
'''
assert text.count(old) == 1
path.write_text(text.replace(old, new))
