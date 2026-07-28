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

fn bind_module(
    source: &mut String,
    manifest: &str,
    declaration: &str,
    file: &str,
) -> io::Result<()> {
    let path = PathBuf::from(manifest)
        .join("src")
        .join(file)
        .display()
        .to_string()
        .replace('\\', "/");
    replace_once(
        source,
        declaration,
        &format!("#[path = \"{path}\"]\n{declaration}"),
    )
}

fn write_generated_commands(out_dir: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let mut source = fs::read_to_string("src/commands.rs")?.replace("\r\n", "\n");
    replace_once(
        &mut source,
        "pub enum SlashCommand {\n    Help,",
        "pub enum SlashCommand {\n    Help,\n    Review { action: ReviewCommand },",
    )?;
    replace_once(
        &mut source,
        "pub enum ModelCommand {",
        "#[derive(Clone, Debug, Eq, PartialEq)]\npub enum ReviewCommand {\n    Show { filter: Option<String> },\n    AcceptFile { path: String },\n    AcceptTask,\n    RevertFile { path: String },\n    RevertHunk { path: String, hunk_id: String },\n    Export,\n}\n\npub enum ModelCommand {",
    )?;
    replace_once(
        &mut source,
        "    CommandSpec {\n        name: \"help\",",
        "    CommandSpec {\n        name: \"review\",\n        usage: \"/review [show [filter]|accept <path>|accept-all|revert <path>|revert-hunk <path> <hunk-id>|export]\",\n        description: \"inspect, filter, accept, revert, or export repository review state\",\n    },\n    CommandSpec {\n        name: \"help\",",
    )?;
    replace_once(
        &mut source,
        "        \"plan\" => Ok(Some(SlashCommand::Plan {\n            task: (!remainder.is_empty()).then(|| remainder.to_owned()),\n        })),",
        "        \"review\" => {\n            let mut parts = remainder.split_whitespace();\n            let action = match parts.next() {\n                None | Some(\"show\") => ReviewCommand::Show { filter: parts.next().map(str::to_owned) },\n                Some(\"accept\") => ReviewCommand::AcceptFile { path: parts.next().ok_or_else(|| \"/review accept expects a path\".to_owned())?.to_owned() },\n                Some(\"accept-all\") => ReviewCommand::AcceptTask,\n                Some(\"revert\") => ReviewCommand::RevertFile { path: parts.next().ok_or_else(|| \"/review revert expects a path\".to_owned())?.to_owned() },\n                Some(\"revert-hunk\") => ReviewCommand::RevertHunk {\n                    path: parts.next().ok_or_else(|| \"/review revert-hunk expects a path\".to_owned())?.to_owned(),\n                    hunk_id: parts.next().ok_or_else(|| \"/review revert-hunk expects a hunk id\".to_owned())?.to_owned(),\n                },\n                Some(\"export\") => ReviewCommand::Export,\n                Some(other) => return Err(format!(\"unknown /review action: {other}\")),\n            };\n            Ok(Some(SlashCommand::Review { action }))\n        }\n        \"plan\" => Ok(Some(SlashCommand::Plan {\n            task: (!remainder.is_empty()).then(|| remainder.to_owned()),\n        })),",
    )?;
    let output = out_dir.join("commands_generated.rs");
    fs::write(&output, source)?;
    Ok(output)
}