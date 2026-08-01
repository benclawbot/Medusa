use std::{fs, path::Path};

pub fn run() {
    patch_voice();
    patch_webhook();
    patch_mini_app_test();
}

fn patch_voice() {
    let path = Path::new("src/telegram/voice.rs");
    let mut source = read(path);
    replace_if_present(
        &mut source,
        "fn read_bounded(mut response: Response, limit: u64)",
        "fn read_bounded(response: Response, limit: u64)",
    );
    write(path, source);
}

fn patch_webhook() {
    let path = Path::new("src/telegram/webhook.rs");
    let mut source = read(path);
    replace_if_present(
        &mut source,
        "    #[must_use]\n    pub fn local_addr",
        "    pub fn local_addr",
    );
    write(path, source);
}

fn patch_mini_app_test() {
    let path = Path::new("src/telegram/mini_app.rs");
    let mut source = read(path);
    replace_if_present(
        &mut source,
        "            user_id: 11,\n        };",
        "            user_id: 11,\n            chat_kind: super::super::TelegramChatKind::Private,\n            bot_mentioned: false,\n        };",
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
