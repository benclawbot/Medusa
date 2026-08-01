use std::{fs, path::Path};

pub fn run() {
    let path = Path::new("src/telegram/mini_app.rs");
    let mut source = read(path);
    replace_if_present(
        &mut source,
        "const tg = window.Telegram.WebApp; tg.ready();\nlet pc, stream, muted = false, ticket;",
        "const tg = window.Telegram.WebApp; tg.ready();\nconst launchTicket = new URLSearchParams(window.location.search).get('ticket');\nlet pc, stream, muted = false, ticket;",
    );
    replace_if_present(
        &mut source,
        "  const auth = await fetch('/telegram/mini-app/auth', {method:'POST', headers:{'content-type':'application/json'}, body:JSON.stringify({initData:tg.initData})});",
        "  if (!launchTicket) throw new Error('Missing signed launch ticket');\n  const auth = await fetch('/telegram/mini-app/auth', {method:'POST', headers:{'content-type':'application/json'}, body:JSON.stringify({ticket:launchTicket, initData:tg.initData})});",
    );
    replace_if_present(
        &mut source,
        "  const channel = pc.createDataChannel('oai-events'); channel.onmessage = event => { const data = JSON.parse(event.data); if (data.type && data.type.includes('transcript')) transcript.textContent += data.delta || data.transcript || ''; };",
        "  const channel = pc.createDataChannel('oai-events'); channel.onmessage = async event => { const data = JSON.parse(event.data); if (data.type && data.type.includes('transcript')) transcript.textContent += data.delta || data.transcript || ''; if (data.type === 'conversation.item.input_audio_transcription.completed' && data.transcript) { await fetch('/telegram/mini-app/transcript', {method:'POST', headers:{'content-type':'application/json','authorization':`Bearer ${ticket}`}, body:JSON.stringify({transcript:data.transcript})}); } };",
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
