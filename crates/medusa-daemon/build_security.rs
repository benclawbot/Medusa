use std::{fs, path::Path};

pub fn run() {
    let path = Path::new("src/telegram/mini_app.rs");
    let mut source = read(path);
    replace_if_present(
        &mut source,
        "    user_id: i64,\n    session_id: String,",
        "    user_id: i64,\n    chat_kind: TelegramChatKind,\n    session_id: String,",
    );
    replace_if_present(
        &mut source,
        "            user_id: identity.user_id,\n            session_id: session_id.to_owned(),",
        "            user_id: identity.user_id,\n            chat_kind: identity.chat_kind,\n            session_id: session_id.to_owned(),",
    );
    replace_if_present(
        &mut source,
        "                chat_kind: TelegramChatKind::Private,\n                bot_mentioned: false,",
        "                chat_kind: claims.chat_kind,\n                bot_mentioned: false,",
    );
    replace_if_present(
        &mut source,
        "            || claims.user_id != expected.user_id\n            || claims.expires_at < now.unix_timestamp()",
        "            || claims.user_id != expected.user_id\n            || claims.chat_kind != expected.chat_kind\n            || claims.expires_at < now.unix_timestamp()",
    );
    write(path, source);
}

fn replace_if_present(source: &mut String, old: &str, new: &str) {
    if source.contains(old) {
        *source = source.replacen(old, new, 1);
    }
}

fn read(path: &Path) -> String {
    match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => fail(&format!("cannot read {}: {error}", path.display())),
    }
}

fn write(path: &Path, source: String) {
    if let Err(error) = fs::write(path, source) {
        fail(&format!("cannot write {}: {error}", path.display()));
    }
}

fn fail(message: &str) -> ! {
    eprintln!("cargo:warning={message}");
    std::process::exit(1)
}
