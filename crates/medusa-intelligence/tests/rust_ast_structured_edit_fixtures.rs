use medusa_intelligence::RustStructuredEditPlanner;

#[test]
fn representative_rust_edits_generate_reviewable_plans() {
    let source = r#"
use std::fmt::Debug;

pub mod api;

#[derive(Clone)]
pub struct Item { pub value: u8 }

pub fn answer(value: u8) -> u8 { value + 1 }
"#;

    let planner = RustStructuredEditPlanner::parse("src/lib.rs", source).expect("planner");
    let plans = [
        planner
            .replace_function_signature("answer", "pub fn answer(value: u16) -> u16")
            .expect("signature"),
        planner
            .replace_function_body("answer", "{ value + 2 }")
            .expect("body"),
        planner
            .set_visibility("struct_item", "Item", "pub(crate)")
            .expect("visibility"),
        planner
            .add_import("std::collections::BTreeMap")
            .expect("import"),
        planner.add_module("domain", false).expect("module"),
    ];

    assert!(plans.iter().all(|plan| !plan.text_edits.is_empty()));
    assert!(plans.iter().flat_map(|plan| &plan.text_edits).all(|edit| {
        edit.preconditions
            .expected_ast_node
            .as_ref()
            .is_some_and(|identity| identity.contains('@'))
    }));
}
