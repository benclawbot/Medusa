from pathlib import Path

path = Path("crates/medusa-provider/src/openai.rs")
text = path.read_text()
text = text.replace(
    '.request_body(&request_with_image(ImageSource::Base64 {\n                media_type: "image/png".to_owned(),\n                data: "AAEC".to_owned(),\n            }))',
    '.request_body(\n                &request_with_image(ImageSource::Base64 {\n                    media_type: "image/png".to_owned(),\n                    data: "AAEC".to_owned(),\n                }),\n                false,\n            )',
)
text = text.replace(
    '.request_body(&request_with_image(ImageSource::AttachmentRef {\n                attachment_id: "attachment-1".to_owned(),\n            }))',
    '.request_body(\n                &request_with_image(ImageSource::AttachmentRef {\n                    attachment_id: "attachment-1".to_owned(),\n                }),\n                false,\n            )',
)
path.write_text(text)
