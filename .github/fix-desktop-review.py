from pathlib import Path

runtime = Path("apps/medusa-desktop/src-tauri/src/runtime.rs")
text = runtime.read_text()
old = """        ClipboardImage, FileAttachment, MAX_CLIPBOARD_TEXT_BYTES, MAX_TOTAL_ATTACHMENT_BYTES,
        PromptAttachment, PromptDraft, TextAttachment,
"""
new = """        ClipboardImage, FileAttachment, MAX_CLIPBOARD_TEXT_BYTES, MAX_IMAGE_BYTES,
        MAX_IMAGE_PIXELS, MAX_TOTAL_ATTACHMENT_BYTES, PromptAttachment, PromptDraft,
        TextAttachment,
"""
assert old in text
text = text.replace(old, new, 1)

marker = """struct RuntimeEntry {
    repo: PathBuf,
    controller: RuntimeController,
}
"""
replacement = marker + """
impl Drop for RuntimeEntry {
    fn drop(&mut self) {
        self.controller.cancel();
    }
}
"""
assert marker in text
text = text.replace(marker, replacement, 1)

start = text.index("fn attach_image(")
end = text.index("\nfn ensure_total(", start)
safe_image = r'''fn attach_image(draft: &mut PromptDraft, name: &str, data_url: &str) -> Result<(), String> {
    let (header, encoded) = data_url
        .split_once(',')
        .ok_or_else(|| format!("image attachment {name} is not a data URL"))?;
    if !header.starts_with("data:image/") || !header.ends_with(";base64") {
        return Err(format!(
            "image attachment {name} must be a base64 image data URL"
        ));
    }
    let max_encoded_bytes = MAX_IMAGE_BYTES
        .saturating_mul(4)
        .div_ceil(3)
        .saturating_add(4);
    if encoded.len() > max_encoded_bytes {
        return Err(format!(
            "encoded image attachment {name} exceeds the {MAX_IMAGE_BYTES}-byte image limit"
        ));
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|error| format!("cannot decode image attachment {name}: {error}"))?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "image attachment {name} is {} bytes; limit is {MAX_IMAGE_BYTES}",
            bytes.len()
        ));
    }
    let dimensions = ImageReader::new(std::io::Cursor::new(bytes.as_slice()))
        .with_guessed_format()
        .map_err(|error| format!("cannot detect image attachment {name}: {error}"))?
        .into_dimensions()
        .map_err(|error| format!("cannot inspect image attachment {name}: {error}"))?;
    validate_image_dimensions(name, dimensions.0, dimensions.1)?;
    let image = ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| format!("cannot detect image attachment {name}: {error}"))?
        .decode()
        .map_err(|error| format!("cannot decode image attachment {name}: {error}"))?;
    let rgba = image.to_rgba8();
    draft
        .add_image(ClipboardImage {
            width: rgba.width(),
            height: rgba.height(),
            rgba: rgba.into_raw(),
            source_format: Some(
                header
                    .trim_start_matches("data:")
                    .trim_end_matches(";base64")
                    .to_owned(),
            ),
        })
        .map_err(|error| error.to_string())?;
    if let Some(PromptAttachment::Image(image)) = draft.attachments.last_mut() {
        image.display_name = name.to_owned();
    }
    Ok(())
}

fn validate_image_dimensions(name: &str, width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err(format!("image attachment {name} has zero dimensions"));
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| format!("image attachment {name} dimensions overflow"))?;
    if pixels > MAX_IMAGE_PIXELS {
        return Err(format!(
            "image attachment {name} has {pixels} pixels; limit is {MAX_IMAGE_PIXELS}"
        ));
    }
    let rgba_bytes = pixels
        .checked_mul(4)
        .ok_or_else(|| format!("image attachment {name} byte count overflow"))?;
    if rgba_bytes > MAX_IMAGE_BYTES as u64 {
        return Err(format!(
            "image attachment {name} requires {rgba_bytes} RGBA bytes; limit is {MAX_IMAGE_BYTES}"
        ));
    }
    Ok(())
}
'''
text = text[:start] + safe_image + text[end:]

test_marker = """    fn repository_file_attachment_keeps_canonical_path_and_size() {
"""
test = """    #[test]
    fn oversized_image_dimensions_are_rejected_before_decode() {
        let error = validate_image_dimensions("bomb.png", 10_000, 10_000)
            .expect_err("oversized dimensions must fail");
        assert!(error.contains("pixels"));
    }

    #[test]
""" + test_marker
assert test_marker in text
text = text.replace(test_marker, test, 1)
runtime.write_text(text)

app = Path("apps/medusa-desktop/src/App.tsx")
text = app.read_text()
old = """  useEffect(() => {
    const previous = window.localStorage.getItem("medusa.desktop.repo");
    if (!previous) return;
    void startRuntime(previous)
      .then((started) => {
        setRuntimeId(started.runtimeId);
        setRepo(started.repo);
      })
      .catch(() => window.localStorage.removeItem("medusa.desktop.repo"));
  }, []);
"""
new = """  useEffect(() => {
    const previous = window.localStorage.getItem("medusa.desktop.repo");
    if (!previous) return;
    let disposed = false;
    void startRuntime(previous)
      .then((started) => {
        if (disposed) {
          void closeRuntime(started.runtimeId);
          return;
        }
        setRuntimeId(started.runtimeId);
        setRepo(started.repo);
      })
      .catch(() => {
        if (!disposed) window.localStorage.removeItem("medusa.desktop.repo");
      });
    return () => {
      disposed = true;
    };
  }, []);
"""
assert old in text
text = text.replace(old, new, 1)
old = """    try {
      if (runtimeId) await closeRuntime(runtimeId);
      const started = await startRuntime(selected);
"""
new = """    try {
      const started = await startRuntime(selected);
"""
assert old in text
text = text.replace(old, new, 1)
app.write_text(text)
