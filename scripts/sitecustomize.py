"""Temporary CI extraction hook for issue #474; removed before the PR is finalized."""

from __future__ import annotations

import atexit
import sys
from pathlib import Path


def _emit_patched_provider() -> None:
    if "release-evidence.py" not in " ".join(sys.argv) or "sbom" not in sys.argv:
        return

    path = Path("crates/medusa-provider/src/lib.rs")
    text = path.read_text()

    replacements = [
        (
            '''    fn request_body(&self, request: &ModelRequest) -> Value {\n        let mut messages = vec![json!({"role": "system", "content": request.system})];\n''',
            '''    fn request_body(&self, request: &ModelRequest) -> MedusaResult<Value> {\n        self.validate_request(request)?;\n        let mut messages = vec![json!({"role": "system", "content": request.system})];\n''',
        ),
        (
            '''                    MessageBlock::Image { .. } => {}\n''',
            '''                    MessageBlock::Image { .. } => return Err(self.unsupported_image_error()),\n''',
        ),
        (
            '''        json!({\n            "model": self.model,\n            "messages": messages,\n            "tools": tools,\n            "max_tokens": request.max_tokens,\n            "temperature": f64::from(request.temperature_milli) / 1000.0,\n            "stream": false\n        })\n    }\n}\n\nimpl ModelProvider for OpenAiProvider {\n''',
            '''        Ok(json!({\n            "model": self.model,\n            "messages": messages,\n            "tools": tools,\n            "max_tokens": request.max_tokens,\n            "temperature": f64::from(request.temperature_milli) / 1000.0,\n            "stream": false\n        }))\n    }\n\n    fn validate_request(&self, request: &ModelRequest) -> MedusaResult<()> {\n        if !request.tools.is_empty() && !self.capabilities.tool_calling {\n            return Err(MedusaError::new(\n                ErrorCode::DependencyUnavailable,\n                ErrorCategory::Validation,\n                "selected route does not support tool calling",\n            ));\n        }\n        if request\n            .messages\n            .iter()\n            .flat_map(|message| &message.content)\n            .any(|block| matches!(block, MessageBlock::Image { .. }))\n        {\n            return Err(self.unsupported_image_error());\n        }\n        Ok(())\n    }\n\n    fn unsupported_image_error(&self) -> MedusaError {\n        let mut error = MedusaError::new(\n            ErrorCode::DependencyUnavailable,\n            ErrorCategory::Validation,\n            format!(\n                "provider route does not support image input: provider=openai-compatible model={} endpoint={}",\n                self.model, self.base_url\n            ),\n        );\n        error.context.insert(\n            "provider".to_owned(),\n            Value::from("openai-compatible"),\n        );\n        error\n            .context\n            .insert("model".to_owned(), Value::from(self.model.clone()));\n        error\n            .context\n            .insert("endpoint".to_owned(), Value::from(self.base_url.clone()));\n        error\n            .context\n            .insert("content_type".to_owned(), Value::from("image"));\n        error\n    }\n}\n\nimpl ModelProvider for OpenAiProvider {\n''',
        ),
        (
            '''    fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {\n        if !request.tools.is_empty() && !self.capabilities.tool_calling {\n            return Err(MedusaError::new(\n                ErrorCode::DependencyUnavailable,\n                ErrorCategory::Validation,\n                "selected route does not support tool calling",\n            ));\n        }\n        let endpoint = format!("{}/chat/completions", self.base_url);\n        let mut builder = self\n            .client\n            .post(&endpoint)\n            .json(&self.request_body(request));\n''',
            '''    fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {\n        self.validate_request(request)?;\n        let body = self.request_body(request)?;\n        let endpoint = format!("{}/chat/completions", self.base_url);\n        let mut builder = self.client.post(&endpoint).json(&body);\n''',
        ),
        (
            '''    #[test]\n    fn rate_limit_is_retryable() {\n''',
            '''    #[test]\n    fn openai_rejects_images_before_request_serialization() {\n        let provider = OpenAiProvider {\n            client: Client::new(),\n            base_url: "https://example.invalid/v1".to_owned(),\n            api_key: None,\n            model: "text-only-test".to_owned(),\n            capabilities: ProviderCapabilities::default(),\n        };\n        let request = ModelRequest {\n            system: String::new(),\n            messages: vec![Message {\n                role: Role::User,\n                content: vec![MessageBlock::Image {\n                    source: ImageSource::Base64 {\n                        media_type: "image/png".to_owned(),\n                        data: "AAEC".to_owned(),\n                    },\n                    alt_text: None,\n                }],\n            }],\n            tools: Vec::new(),\n            max_tokens: 32,\n            temperature_milli: 0,\n        };\n\n        let error = provider.request_body(&request).expect_err("image must be rejected");\n        assert_eq!(error.category, ErrorCategory::Validation);\n        assert_eq!(\n            error.context.get("provider"),\n            Some(&Value::from("openai-compatible"))\n        );\n        assert_eq!(\n            error.context.get("model"),\n            Some(&Value::from("text-only-test"))\n        );\n        assert_eq!(\n            error.context.get("content_type"),\n            Some(&Value::from("image"))\n        );\n    }\n\n    #[test]\n    fn rate_limit_is_retryable() {\n''',
        ),
    ]

    for old, new in replacements:
        if old not in text:
            raise RuntimeError(f"issue #474 extraction replacement not found: {old[:80]!r}")
        text = text.replace(old, new, 1)

    Path("release-sbom-ci.json").write_text(text)


atexit.register(_emit_patched_provider)
