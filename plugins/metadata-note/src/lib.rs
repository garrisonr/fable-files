//! Metadata Note — the Fable Files example plugin.
//!
//! A dependency-free WASM guest for the Phase 6 host ABI (see
//! crates/ufm-plugins/src/runtime.rs). On an `action.context`
//! invocation it re-reads the item's metadata through the host's
//! `fable.read_item`, logs what it found, and PROPOSES one text file
//! — `<item name>.fable-note.txt` — summarising the item. The host
//! shows the proposal to the user; only a confirmed proposal becomes
//! a journaled job that writes the file.
//!
//! Build (see README.md): cargo build --release --target
//! wasm32-unknown-unknown, then copy the .wasm to `main.wasm`.

#[link(wasm_import_module = "fable")]
extern "C" {
    fn log(ptr: u32, len: u32);
    fn read_item(path_ptr: u32, path_len: u32, out_ptr: u32, out_cap: u32) -> i32;
}

// ---------------------------------------------------------------------------
// Host ABI: allocation. The host allocates the event in our memory,
// we allocate the response in ours. Instances are per-call, so the
// simple leak-and-forget allocator never outlives an invocation.
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn alloc(len: u32) -> u32 {
    let mut v = Vec::<u8>::with_capacity(len as usize);
    let ptr = v.as_mut_ptr();
    std::mem::forget(v);
    ptr as u32
}

#[no_mangle]
pub extern "C" fn dealloc(ptr: u32, len: u32) {
    unsafe {
        let _ = Vec::from_raw_parts(ptr as *mut u8, 0, len as usize);
    }
}

fn host_log(msg: &str) {
    unsafe { log(msg.as_ptr() as u32, msg.len() as u32) }
}

fn bytes<'a>(ptr: u32, len: u32) -> &'a [u8] {
    unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) }
}

// ---------------------------------------------------------------------------
// Tiny JSON helpers — just enough to read flat fields and to write
// correctly-escaped strings. A real plugin may use serde; the example
// stays dependency-free so anyone can read it end to end.
// ---------------------------------------------------------------------------

fn json_str(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let at = json.find(&needle)?;
    let rest = json[at + needle.len()..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(other) => out.push(other),
                None => return None,
            },
            other => out.push(other),
        }
    }
    None
}

fn json_num(json: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\"");
    let at = json.find(&needle)?;
    let rest = json[at + needle.len()..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn json_bool(json: &str, key: &str) -> Option<bool> {
    let needle = format!("\"{key}\"");
    let at = json.find(&needle)?;
    let rest = json[at + needle.len()..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// The first string element of `"key": ["a", "b"]`, plus a count.
fn json_str_array(json: &str, key: &str) -> Vec<String> {
    let needle = format!("\"{key}\"");
    let Some(at) = json.find(&needle) else {
        return Vec::new();
    };
    let rest = json[at + needle.len()..].trim_start();
    let Some(rest) = rest.strip_prefix(':') else {
        return Vec::new();
    };
    let Some(rest) = rest.trim_start().strip_prefix('[') else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut rest = rest;
    loop {
        rest = rest.trim_start();
        if rest.starts_with(']') || rest.is_empty() {
            break;
        }
        if let Some(stripped) = rest.strip_prefix(',') {
            rest = stripped;
            continue;
        }
        if rest.starts_with('"') {
            // Reuse json_str on a synthetic single-field object.
            let probe = format!("\"k\":{}", rest);
            if let Some(v) = json_str(&probe, "k") {
                // Advance past the string we just consumed.
                let consumed = v.len() + 2; // quotes, approx for escapes
                out.push(v);
                rest = &rest[consumed.min(rest.len())..];
                continue;
            }
        }
        break;
    }
    out
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The event.
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn fable_event(ptr: u32, len: u32) -> u64 {
    let event = String::from_utf8_lossy(bytes(ptr, len)).into_owned();
    let response = handle(&event);
    let out = alloc(response.len() as u32);
    unsafe {
        std::ptr::copy_nonoverlapping(response.as_ptr(), out as *mut u8, response.len());
    }
    ((out as u64) << 32) | (response.len() as u64)
}

fn handle(event: &str) -> String {
    // The event envelope carries the item metadata; the path is the
    // key we re-read through the host API to prove the round trip.
    let Some(path) = json_str(event, "path") else {
        return r#"{"logs":["Metadata Note: the event carried no item path."]}"#.to_string();
    };
    // Re-read the item through the host (invocation-scoped).
    let buf_ptr = alloc(16_384);
    let n = unsafe {
        read_item(
            path.as_ptr() as u32,
            path.len() as u32,
            buf_ptr,
            16_384,
        )
    };
    let item: String = if n >= 0 {
        String::from_utf8_lossy(bytes(buf_ptr, n as u32)).into_owned()
    } else {
        // Fall back to the envelope's copy of the metadata.
        event.to_string()
    };

    let name = json_str(&item, "name").unwrap_or_else(|| "item".into());
    let kind = json_str(&item, "kind").unwrap_or_else(|| "file".into());
    host_log(&format!("Metadata Note looked at {name}."));

    let mut note = String::new();
    note.push_str("Fable Files — item note\n\n");
    note.push_str(&format!("Name: {name}\n"));
    note.push_str(&format!("Kind: {kind}\n"));
    if let Some(size) = json_num(&item, "size_bytes") {
        note.push_str(&format!("Size: {size} bytes\n"));
    }
    if let Some(modified) = json_str(&item, "modified") {
        note.push_str(&format!("Modified: {modified}\n"));
    }
    let labels = json_str_array(&item, "labels");
    if !labels.is_empty() {
        note.push_str(&format!("Labels: {}\n", labels.join(", ")));
    }
    if let Some(starred) = json_bool(&item, "starred") {
        note.push_str(&format!("Starred: {}\n", if starred { "yes" } else { "no" }));
    }
    note.push_str(&format!("Path: {path}\n"));
    note.push_str(
        "\nWritten by the Metadata Note plugin, an example Fable Files plugin.\n\
         The plugin proposed this file; a person confirmed it; Fable's job\n\
         engine wrote it. Nothing happened without all three.\n",
    );

    let proposal_name = format!("{name}.fable-note.txt");
    format!(
        "{{\"logs\":[],\"proposals\":[{{\"kind\":\"create_text_file\",\"name\":\"{}\",\"contents\":\"{}\"}}]}}",
        json_escape(&proposal_name),
        json_escape(&note),
    )
}
