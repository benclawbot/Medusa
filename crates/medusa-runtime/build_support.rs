use std::{env, error::Error, fs, io, path::{Path, PathBuf}};

fn replace_once(source: &mut String, needle: &str, replacement: &str) -> io::Result<()> {
    let position = source.find(needle).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("runtime generation anchor not found: {needle}"),
        )
    })?;
    source.replace_range(position..position + needle.len(), replacement);
    Ok(())
}

fn bind_module(source: &mut String, manifest: &str, declaration: &str, file: &str) -> io::Result<()> {
    let path = PathBuf::from(manifest).join("src").join(file).display().to_string().replace('\\', "/");
    replace_once(source, declaration, &format!("#[path = \"{path}\"]\n{declaration}"))
}

fn write_generated_commands(out_dir: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let mut source = fs::read_to_string("src/commands.rs")?.replace("\r\n", "\n");
    replace_once(&mut source, "pub enum SlashCommand {\n    Help,", "pub enum SlashCommand {\n    Help,\n    Learning { action: LearningCommand },\n    Review { action: ReviewCommand },")?;
    replace_once(
        &mut source,
        "#[derive(Clone, Eq, PartialEq)]\npub enum ModelCommand {",
        "#[derive(Clone, Debug, Eq, PartialEq)]\npub enum LearningCommand {\n    Show { filter: Option<String> },\n    Approve { id: String },\n    Reject { id: String },\n    Defer { id: String },\n    Validate { id: String },\n    Activate { id: String },\n    Suspend { id: String },\n    Rollback { id: String },\n    Delete { id: String },\n    Privacy,\n    Export,\n}\n\n#[derive(Clone, Debug, Eq, PartialEq)]\npub enum ReviewCommand {\n    Show { filter: Option<String> },\n    AcceptFile { path: String },\n    AcceptTask,\n    RevertFile { path: String },\n    RevertHunk { path: String, hunk_id: String },\n    Export,\n}\n\n#[derive(Clone, Eq, PartialEq)]\npub enum ModelCommand {",
    )?;
    replace_once(
        &mut source,
        "    CommandSpec {\n        name: \"help\",",
        "    CommandSpec {\n        name: \"learning\",\n        usage: \"/learning [show [filter]|approve|reject|defer|validate|activate|suspend|rollback|delete <id>|privacy|export]\",\n        description: \"review and control the authoritative learning lifecycle\",\n    },\n    CommandSpec {\n        name: \"review\",\n        usage: \"/review [show [filter]|accept <path>|accept-all|revert <path>|revert-hunk <path> <hunk-id>|export]\",\n        description: \"inspect, filter, accept, revert, or export repository review state\",\n    },\n    CommandSpec {\n        name: \"help\",",
    )?;
    replace_once(
        &mut source,
        "        \"plan\" => Ok(Some(SlashCommand::Plan {\n            task: (!remainder.is_empty()).then(|| remainder.to_owned()),\n        })),",
        "        \"learning\" => {\n            let mut parts = remainder.split_whitespace();\n            let required_id = |value: Option<&str>, action: &str| value.map(str::to_owned).ok_or_else(|| format!(\"/learning {action} expects an item id\"));\n            let action = match parts.next() {\n                None | Some(\"show\") => LearningCommand::Show { filter: parts.next().map(str::to_owned) },\n                Some(\"approve\") => LearningCommand::Approve { id: required_id(parts.next(), \"approve\")? },\n                Some(\"reject\") => LearningCommand::Reject { id: required_id(parts.next(), \"reject\")? },\n                Some(\"defer\") => LearningCommand::Defer { id: required_id(parts.next(), \"defer\")? },\n                Some(\"validate\") => LearningCommand::Validate { id: required_id(parts.next(), \"validate\")? },\n                Some(\"activate\") => LearningCommand::Activate { id: required_id(parts.next(), \"activate\")? },\n                Some(\"suspend\") => LearningCommand::Suspend { id: required_id(parts.next(), \"suspend\")? },\n                Some(\"rollback\") => LearningCommand::Rollback { id: required_id(parts.next(), \"rollback\")? },\n                Some(\"delete\") => LearningCommand::Delete { id: required_id(parts.next(), \"delete\")? },\n                Some(\"privacy\") => LearningCommand::Privacy,\n                Some(\"export\") => LearningCommand::Export,\n                Some(other) => return Err(format!(\"unknown /learning action: {other}\")),\n            };\n            Ok(Some(SlashCommand::Learning { action }))\n        }\n        \"review\" => {\n            let mut parts = remainder.split_whitespace();\n            let action = match parts.next() {\n                None | Some(\"show\") => ReviewCommand::Show { filter: parts.next().map(str::to_owned) },\n                Some(\"accept\") => ReviewCommand::AcceptFile { path: parts.next().ok_or_else(|| \"/review accept expects a path\".to_owned())?.to_owned() },\n                Some(\"accept-all\") => ReviewCommand::AcceptTask,\n                Some(\"revert\") => ReviewCommand::RevertFile { path: parts.next().ok_or_else(|| \"/review revert expects a path\".to_owned())?.to_owned() },\n                Some(\"revert-hunk\") => ReviewCommand::RevertHunk { path: parts.next().ok_or_else(|| \"/review revert-hunk expects a path\".to_owned())?.to_owned(), hunk_id: parts.next().ok_or_else(|| \"/review revert-hunk expects a hunk id\".to_owned())?.to_owned() },\n                Some(\"export\") => ReviewCommand::Export,\n                Some(other) => return Err(format!(\"unknown /review action: {other}\")),\n            };\n            Ok(Some(SlashCommand::Review { action }))\n        }\n        \"plan\" => Ok(Some(SlashCommand::Plan {\n            task: (!remainder.is_empty()).then(|| remainder.to_owned()),\n        })),",
    )?;
    let output = out_dir.join("commands_generated.rs");
    fs::write(&output, source)?;
    Ok(output)
}
