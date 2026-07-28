from pathlib import Path

path = Path("crates/medusa-provider/src/lib.rs")
source = path.read_text()
openai_start = source.index("impl OpenAiProvider")

old_caps = '''            capabilities: ProviderCapabilities {
                tool_calling: config.model.tool_calling,
                streaming: config.model.streaming,
                ..ProviderCapabilities::default()
            },'''
new_caps = '''            capabilities: ProviderCapabilities {
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
caps_at = source.index(old_caps, openai_start)
source = source[:caps_at] + new_caps + source[caps_at + len(old_caps):]

request_start = source.index(
    "    fn request_body(&self, request: &ModelRequest) -> Value {", openai_start
)
request_end = source.index(
    "\n    }\n}\n\nimpl ModelProvider for OpenAiProvider", request_start
) + len("\n    }")
request_body = '''    fn request_body(&self, request: &ModelRequest) -> MedusaResult<Value> {
        let mut messages = vec![json!({"role": "system", "content": request.system})];
        for message in &request.messages {
            let role = match message.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            let mut text = String::new();
            let mut content_parts = Vec::new();
            let mut tool_calls = Vec::new();
            for block in &message.content {
                match block {
                    MessageBlock::Text { text: value } => {
                        text.push_str(value);
                        content_parts.push(json!({"type": "text", "text": value}));
                    }
                    MessageBlock::ToolUse { id, name, input } => tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": input.to_string()}
                    })),
                    MessageBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => messages.push(json!({
                        "role": "tool", "tool_call_id": tool_use_id, "content": content
                    })),
                    MessageBlock::Image { source, .. } => match source {
                        ImageSource::Base64 { media_type, data } => {
                            if !self.capabilities.image_input {
                                return Err(openai_image_error(
                                    &self.model,
                                    "selected OpenAI route does not support image input",
                                ));
                            }
                            if !self.capabilities.supported_image_media_types.is_empty()
                                && !self
                                    .capabilities
                                    .supported_image_media_types
                                    .iter()
                                    .any(|supported| supported == media_type)
                            {
                                return Err(openai_image_error(
                                    &self.model,
                                    format!("unsupported image media type {media_type}"),
                                ));
                            }
                            content_parts.push(json!({
                                "type": "image_url",
                                "image_url": {"url": format!("data:{media_type};base64,{data}")}
                            }));
                        }
                        ImageSource::AttachmentRef { attachment_id } => {
                            return Err(openai_image_error(
                                &self.model,
                                format!("unresolved image attachment reference {attachment_id}"),
                            ));
                        }
                    },
                }
            }
            let content = if content_parts
                .iter()
                .any(|part| part["type"] == Value::String("image_url".to_owned()))
            {
                Value::Array(content_parts)
            } else {
                Value::String(text)
            };
            let mut wire = json!({"role": role, "content": content});
            if !tool_calls.is_empty() {
                wire["tool_calls"] = Value::Array(tool_calls);
            }
            messages.push(wire);
        }
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|tool| json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema
                }
            }))
            .collect();
        Ok(json!({
            "model": self.model,
            "messages": messages,
            "tools": tools,
            "max_tokens": request.max_tokens,
            "temperature": f64::from(request.temperature_milli) / 1000.0,
            "stream": false
        }))
    }'''
source = source[:request_start] + request_body + source[request_end:]

marker = "\n}\n\nimpl ModelProvider for OpenAiProvider {"
helper = '''
}

fn openai_image_error(model: &str, message: impl Into<String>) -> MedusaError {
    let mut error = MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Validation,
        message.into(),
    );
    error.context.insert("provider".to_owned(), Value::from("openai"));
    error.context.insert("model".to_owned(), Value::from(model.to_owned()));
    error.context.insert("content_type".to_owned(), Value::from("image"));
    error
}

impl ModelProvider for OpenAiProvider {'''
marker_at = source.index(marker, openai_start)
source = source[:marker_at] + "\n" + helper + source[marker_at + len(marker):]

old_json = ".json(&self.request_body(request));"
json_at = source.index(old_json, openai_start)
source = source[:json_at] + ".json(&self.request_body(request)?);" + source[json_at + len(old_json):]

tests = r'''

    fn openai_test_provider(image_input: bool) -> OpenAiProvider {
        OpenAiProvider {
            client: shared_http_client().expect("client"),
            base_url: "https://example.invalid/v1".to_owned(),
            api_key: None,
            model: "gpt-5".to_owned(),
            capabilities: ProviderCapabilities {
                image_input,
                supported_image_media_types: vec!["image/png".to_owned()],
                max_image_bytes: Some(20 * 1024 * 1024),
                max_images_per_request: Some(10),
                tool_calling: true,
                streaming: false,
            },
        }
    }

    fn request_with_image(source: ImageSource) -> ModelRequest {
        ModelRequest {
            system: "system".to_owned(),
            messages: vec![Message {
                role: Role::User,
                content: vec![
                    MessageBlock::Text { text: "inspect".to_owned() },
                    MessageBlock::Image {
                        source,
                        alt_text: Some("screenshot".to_owned()),
                    },
                ],
            }],
            tools: Vec::new(),
            max_tokens: 100,
            temperature_milli: 0,
        }
    }

    #[test]
    fn openai_serializes_base64_images_as_image_url_parts() {
        let body = openai_test_provider(true)
            .request_body(&request_with_image(ImageSource::Base64 {
                media_type: "image/png".to_owned(),
                data: "AAEC".to_owned(),
            }))
            .expect("request body");
        let content = body["messages"][1]["content"]
            .as_array()
            .expect("multimodal content array");
        assert_eq!(content[0], json!({"type": "text", "text": "inspect"}));
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,AAEC");
    }

    #[test]
    fn openai_rejects_images_when_route_is_text_only() {
        let error = openai_test_provider(false)
            .request_body(&request_with_image(ImageSource::Base64 {
                media_type: "image/png".to_owned(),
                data: "AAEC".to_owned(),
            }))
            .expect_err("reject image");
        assert_eq!(error.context["content_type"], "image");
    }

    #[test]
    fn openai_rejects_unresolved_attachment_references() {
        let error = openai_test_provider(true)
            .request_body(&request_with_image(ImageSource::AttachmentRef {
                attachment_id: "attachment-1".to_owned(),
            }))
            .expect_err("reject unresolved reference");
        assert!(error.to_string().contains("attachment-1"));
    }
'''
final = source.rfind("\n}")
assert final > source.index("#[cfg(test)]")
source = source[:final] + tests + source[final:]
path.write_text(source)
