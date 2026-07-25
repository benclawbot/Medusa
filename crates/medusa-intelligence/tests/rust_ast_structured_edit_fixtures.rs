use medusa_intelligence::{RustAstEdit, RustEditTarget, RustStructuredEditPlanner};

#[test]
fn representative_rust_edits_generate_reviewable_plans() {
    let source = r#"
use std::fmt::Debug;

pub mod api;

#[derive(Clone)]
pub struct Item { pub value: u8 }

pub fn answer(value: u8) -> u8 { value + 1 }
"#;

    let mut planner = RustStructuredEditPlanner::new("fixture-plan", "src/lib.rs", source)
        .expect("planner");
    planner
        .push(RustAstEdit::ReplaceFunctionSignature {
            function: "answer".to_owned(),
            signature: "pub fn answer(value: u16) -> u16".to_owned(),
        })
        .expect("signature");
    planner
        .push(RustAstEdit::ReplaceFunctionBody {
            function: "answer".to_owned(),
            body: "{ value + 2 }".to_owned(),
        })
        .expect("body");
    planner
        .push(RustAstEdit::SetVisibility {
            target: RustEditTarget::named("struct_item", "Item"),
            visibility: "pub(crate)".to_owned(),
        })
        .expect("visibility");

    let plan = planner.finish().expect("plan");
    assert_eq!(plan.text_edits.len(), 3);
    assert!(plan
        .text_edits
        .iter()
        .all(|edit| edit.preconditions.expected_ast_node.is_some()));
}
