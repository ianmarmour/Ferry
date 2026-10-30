//! ## Message Text
//!
//! SMS bodies are plain text, but MMS, e-mail and instant messages arrive in MIME
//! format (MAP section 3.1.3), with headers, possibly multipart and encoded. This
//! module digs the readable text out of either.
//!
use map::bmessage::BMessage;
use map::types::MessageType;

use crate::contacts::decode_quoted_printable;

/// The readable text of a message.
pub fn message_text(message: &BMessage) -> String {
    let text = message.text();
    let mime = matches!(
        message.message_type,
        MessageType::Mms | MessageType::Email | MessageType::Im
    );

    if has_content_type(&text) || (mime && has_headers(&text)) {
        if let Some(body) = mime_text(&text) {
            return body.trim().to_string();
        }
    }
    text.trim_end_matches(['\r', '\n']).to_string()
}

fn header_block(text: &str) -> impl Iterator<Item = &str> {
    text.lines().take_while(|line| !line.trim().is_empty())
}

fn has_content_type(text: &str) -> bool {
    header_block(text).any(|line| line.to_ascii_lowercase().starts_with("content-type:"))
}

/// Whether `text` starts with RFC 2822 headers followed by an empty line.
fn has_headers(text: &str) -> bool {
    let is_header = |line: &str| {
        line.split_once(':').is_some_and(|(name, _)| {
            !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    };
    let lines = header_block(text).count();
    lines > 0 && lines < text.lines().count() && header_block(text).take(1).all(is_header)
}

/// Splits a MIME entity into its unfolded headers and its body.
fn split_entity(entity: &str) -> (Vec<(String, String)>, &str) {
    let entity = entity.trim_start_matches(['\r', '\n']);
    let (head, body) = match (entity.find("\r\n\r\n"), entity.find("\n\n")) {
        (Some(crlf), Some(lf)) if lf < crlf => (&entity[..lf], &entity[lf + 2..]),
        (Some(crlf), _) => (&entity[..crlf], &entity[crlf + 4..]),
        (None, Some(lf)) => (&entity[..lf], &entity[lf + 2..]),
        (None, None) => (entity, ""),
    };

    let mut headers: Vec<(String, String)> = Vec::new();
    for line in head.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = headers.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
        } else if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    (headers, body)
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(header, _)| header == name)
        .map(|(_, value)| value.as_str())
}

/// The value of parameter `name` in a header such as `Content-Type`.
fn parameter(value: &str, name: &str) -> Option<String> {
    value.split(';').skip(1).find_map(|param| {
        let (key, value) = param.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().trim_matches('"').to_string())
    })
}

/// The text/plain content of a MIME entity, preferring it over text/html.
fn mime_text(entity: &str) -> Option<String> {
    let (headers, body) = split_entity(entity);
    let content_type = header(&headers, "content-type").unwrap_or("text/plain");
    let media = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();

    if media.starts_with("multipart/") {
        let boundary = parameter(content_type, "boundary")?;
        let delimiter = format!("--{}", boundary);
        let parts: Vec<&str> = body
            .split(delimiter.as_str())
            .skip(1)
            .take_while(|part| !part.starts_with("--"))
            .collect();

        let texts: Vec<String> = parts.iter().filter_map(|part| mime_text(part)).collect();
        return (!texts.is_empty()).then(|| texts.join("\n"));
    }

    if !media.starts_with("text/") {
        return None;
    }

    let encoding = header(&headers, "content-transfer-encoding")
        .unwrap_or("")
        .to_ascii_lowercase();
    let bytes = match encoding.as_str() {
        "quoted-printable" => decode_quoted_printable(body),
        "base64" => decode_base64(body),
        _ => body.as_bytes().to_vec(),
    };
    let text = String::from_utf8_lossy(&bytes).into_owned();

    if media == "text/html" {
        Some(strip_html(&text))
    } else {
        Some(text)
    }
}

fn decode_base64(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0;

    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    out
}

fn strip_html(html: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    text.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use map::bmessage::BMessageBody;

    fn message(message_type: MessageType, text: &str) -> BMessage {
        BMessage::new(message_type, BMessageBody::from_text(text))
    }

    #[test]
    fn sms_text_is_kept() {
        assert_eq!("Hi there", message_text(&message(MessageType::SmsGsm, "Hi there\r\n")));
    }

    #[test]
    fn mms_text_part_is_extracted() {
        let mms = "Date: Mon, 1 Jan 2024 10:00:00 +0000\r\nFrom: +15550001\r\n\
            Content-Type: multipart/mixed; boundary=\"b1\"\r\n\r\n\
            --b1\r\nContent-Type: image/jpeg\r\nContent-Transfer-Encoding: base64\r\n\r\n/9j/4AAQ\r\n\
            --b1\r\nContent-Type: text/plain; charset=utf-8\r\n\
            Content-Transfer-Encoding: base64\r\n\r\nTG9vayBhdCB0aGlzIPCfk7c=\r\n--b1--\r\n";
        assert_eq!("Look at this 📷", message_text(&message(MessageType::Mms, mms)));
    }

    #[test]
    fn quoted_printable_and_html_are_decoded() {
        let email = "Content-Type: text/plain; charset=utf-8\r\n\
            Content-Transfer-Encoding: quoted-printable\r\n\r\nCaf=C3=A9 at =\r\n8?";
        assert_eq!("Café at 8?", message_text(&message(MessageType::Email, email)));

        let html = "Content-Type: text/html\r\n\r\n<p>Tom &amp; Jerry</p>";
        assert_eq!("Tom & Jerry", message_text(&message(MessageType::Im, html)));
    }

    #[test]
    fn plain_bodies_of_mime_types_are_kept() {
        assert_eq!("just text", message_text(&message(MessageType::Mms, "just text")));
        let note = "Note: bring snacks\r\n\r\nand drinks";
        assert_eq!(note, message_text(&message(MessageType::SmsGsm, note)));
        let email = "From: ann@example.com\r\nSubject: Hi\r\n\r\nBody text";
        assert_eq!("Body text", message_text(&message(MessageType::Email, email)));
    }
}
