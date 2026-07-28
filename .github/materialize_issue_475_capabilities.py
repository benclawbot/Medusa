from pathlib import Path

path = Path("crates/medusa-provider/src/lib.rs")
source = path.read_text()
old = '''            capabilities: ProviderCapabilities {
                image_input: config.model.provider.eq_ignore_ascii_case("openai")
                    || config.model.auth.eq_ignore_ascii_case("chatgpt-oauth"),
                supported_image_media_types: vec![
                    "image/png".to_owned(),
                    "image/jpeg".to_owned(),
                    "image/webp".to_owned(),
                    "image/gif".to_owned(),
                ],
                max_image_bytes: Some(20 * 1024 * 1024),
                max_images_per_request: Some(10),
                tool_calling: config.model.tool_calling,
                streaming: config.model.streaming,
            },'''
new = '''            capabilities: {
                let image_input = config.model.provider.eq_ignore_ascii_case("openai")
                    || config.model.auth.eq_ignore_ascii_case("chatgpt-oauth");
                ProviderCapabilities {
                    image_input,
                    supported_image_media_types: if image_input {
                        vec![
                            "image/png".to_owned(),
                            "image/jpeg".to_owned(),
                            "image/webp".to_owned(),
                            "image/gif".to_owned(),
                        ]
                    } else {
                        Vec::new()
                    },
                    max_image_bytes: image_input.then_some(20 * 1024 * 1024),
                    max_images_per_request: image_input.then_some(10),
                    tool_calling: config.model.tool_calling,
                    streaming: config.model.streaming,
                }
            },'''
assert source.count(old) == 1
path.write_text(source.replace(old, new))
