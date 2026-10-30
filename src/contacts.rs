//! ## Contacts
//!
//! Contacts come from the phone's address book over the Phone Book Access Profile
//! (see [`crate::pbap`]) as vCards. This module parses them and matches the addresses
//! messages carry, which come in many spellings (`+1 555-123-4567`, `5551234567`,
//! `(555) 123-4567`), to contacts and conversations.
//!
use serde::{Deserialize, Serialize};

/// A person or business that can be texted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    /// The name to show.
    pub name: String,
    /// Phone numbers.
    pub phones: Vec<Phone>,
    /// E-mail addresses, which iMessage can text too.
    pub emails: Vec<String>,
}

impl Contact {
    /// Every address of the contact, phone numbers first.
    pub fn addresses(&self) -> impl Iterator<Item = &str> {
        self.phones
            .iter()
            .map(|phone| phone.number.as_str())
            .chain(self.emails.iter().map(String::as_str))
    }

    /// Whether the contact's name or one of its addresses contains `query`.
    pub fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        if query.is_empty() || self.name.to_lowercase().contains(&query) {
            return true;
        }

        let digits: String = query.chars().filter(char::is_ascii_digit).collect();
        self.phones.iter().any(|phone| {
            !digits.is_empty()
                && phone
                    .number
                    .chars()
                    .filter(char::is_ascii_digit)
                    .collect::<String>()
                    .contains(&digits)
        }) || self
            .emails
            .iter()
            .any(|email| email.to_lowercase().contains(&query))
    }
}

/// A phone number and what kind of number it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Phone {
    /// The number as stored in the address book.
    pub number: String,
    /// `mobile`, `home`, `work`, `iPhone`, `main` or `other`.
    pub label: String,
}

/// The key identifying the conversation with `address`.
///
/// Phone numbers are compared on their last ten digits, so that the national and
/// international spellings of a number land in the same conversation. E-mail
/// addresses and alphanumeric senders are compared case insensitively.
///
/// # Examples
/// ```
/// use ferry::contacts::conversation_key;
///
/// assert_eq!(conversation_key("+1 (555) 123-4567"), conversation_key("555.123.4567"));
/// assert_eq!(conversation_key("Ann@Example.com"), conversation_key("ann@example.com"));
/// ```
pub fn conversation_key(address: &str) -> String {
    let address = address.trim();
    if address.contains('@') {
        return address.to_lowercase();
    }

    let digits: String = address.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() || address.chars().any(char::is_alphabetic) {
        return address.to_lowercase();
    }
    match digits.len() {
        len if len > 10 => digits[len - 10..].to_string(),
        _ => digits,
    }
}

/// Formats an address for display: North American numbers as `(555) 123-4567`, other
/// addresses as they are.
pub fn format_address(address: &str) -> String {
    let trimmed = address.trim();
    if trimmed.contains('@') || trimmed.chars().any(char::is_alphabetic) {
        return trimmed.to_string();
    }

    let digits: String = trimmed.chars().filter(char::is_ascii_digit).collect();
    let international = trimmed.starts_with('+');
    match (digits.len(), international) {
        (10, false) => format!("({}) {}-{}", &digits[..3], &digits[3..6], &digits[6..]),
        (11, _) if digits.starts_with('1') => {
            format!("+1 ({}) {}-{}", &digits[1..4], &digits[4..7], &digits[7..])
        }
        _ => trimmed.to_string(),
    }
}

/// Up to two initials for an avatar, or `None` when `name` is not a name but an
/// address.
pub fn initials(name: &str) -> Option<String> {
    if name.contains('@') || !name.chars().any(char::is_alphabetic) {
        return None;
    }

    let initials: String = name
        .split_whitespace()
        .filter_map(|word| word.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    (!initials.is_empty()).then_some(initials)
}

/// One logical vCard line: `NAME;PARAM=VALUE:value`.
struct Property {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl Property {
    fn parse(line: &str) -> Option<Property> {
        let colon = line.find(':')?;
        let (head, value) = (&line[..colon], &line[colon + 1..]);
        let mut parts = head.split(';');

        let name = parts.next()?;
        let name = name
            .rsplit('.')
            .next()
            .unwrap_or(name)
            .trim()
            .to_uppercase();
        let params = parts
            .map(|param| match param.split_once('=') {
                Some((key, value)) => (key.trim().to_uppercase(), value.trim().to_string()),
                // vCard 2.1 allows bare parameter values, such as `TEL;CELL:`.
                None => ("TYPE".to_string(), param.trim().to_string()),
            })
            .collect();

        Some(Property {
            name,
            params,
            value: value.to_string(),
        })
    }

    fn is_quoted_printable(&self) -> bool {
        self.params.iter().any(|(key, value)| {
            (key == "ENCODING" || key == "TYPE") && value.eq_ignore_ascii_case("QUOTED-PRINTABLE")
        })
    }

    /// The `TYPE` values, lower case.
    fn types(&self) -> Vec<String> {
        self.params
            .iter()
            .filter(|(key, _)| key == "TYPE")
            .flat_map(|(_, value)| value.split(','))
            .map(|value| value.trim().trim_matches('"').to_lowercase())
            .collect()
    }

    /// The decoded value, split into its `;` separated components.
    fn components(&self) -> Vec<String> {
        let raw = if self.is_quoted_printable() {
            String::from_utf8_lossy(&decode_quoted_printable(&self.value)).into_owned()
        } else {
            self.value.clone()
        };
        split_unescape(&raw)
    }

    fn text(&self) -> String {
        self.components().join(";").trim().to_string()
    }
}

/// Splits on unescaped `;` and removes vCard 3.0 backslash escapes.
fn split_unescape(value: &str) -> Vec<String> {
    let mut components = vec![String::new()];
    let mut chars = value.chars();

    while let Some(c) = chars.next() {
        let current = components.last_mut().expect("never empty");
        match c {
            '\\' => match chars.next() {
                Some('n' | 'N') => current.push('\n'),
                Some(other) => current.push(other),
                None => {}
            },
            ';' => components.push(String::new()),
            c => current.push(c),
        }
    }
    components
}

/// Decodes quoted-printable text, as used by vCard 2.1 and MIME.
pub(crate) fn decode_quoted_printable(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'=' {
            let hex = bytes.get(i + 1..i + 3).and_then(|pair| {
                std::str::from_utf8(pair)
                    .ok()
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok())
            });
            if let Some(byte) = hex {
                out.push(byte);
                i += 3;
                continue;
            }
            // A soft line break, `=` at the end of a line.
            match bytes.get(i + 1..) {
                Some([b'\r', b'\n', ..]) => i += 3,
                Some([b'\n', ..]) => i += 2,
                Some([]) => i += 1,
                _ => {
                    out.push(b'=');
                    i += 1;
                }
            }
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Joins folded lines (a line starting with a space or tab continues the previous one)
/// and quoted-printable soft line breaks (a line ending with `=`).
fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();

    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);

        if let Some(last) = lines.last_mut() {
            let quoted_printable = last.to_uppercase().contains("QUOTED-PRINTABLE");
            if quoted_printable && last.ends_with('=') {
                last.pop();
                last.push_str(line.trim_start());
                continue;
            }
            if line.starts_with(' ') || line.starts_with('\t') {
                last.push_str(&line[1..]);
                continue;
            }
        }
        if !line.trim().is_empty() {
            lines.push(line.to_string());
        }
    }
    lines
}

fn phone_label(types: &[String]) -> Option<String> {
    if types.iter().any(|t| t == "fax") {
        return None;
    }
    let label = ["iphone", "cell", "mobile", "home", "work", "main"]
        .into_iter()
        .find(|known| types.iter().any(|t| t == known))
        .map(|label| match label {
            "iphone" => "iPhone",
            "cell" => "mobile",
            other => other,
        })
        .unwrap_or("other");
    Some(label.to_string())
}

/// Parses a phone book: any number of vCards (2.1 or 3.0) one after the other.
///
/// Entries that have neither a phone number nor an e-mail address are left out, as
/// they cannot be texted.
///
/// # Examples
/// ```
/// let contacts = ferry::contacts::parse_vcards(
///     b"BEGIN:VCARD\r\nVERSION:3.0\r\nN:Appleseed;John;;;\r\nFN:John Appleseed\r\n\
///       TEL;TYPE=CELL:+1 555 123 4567\r\nEND:VCARD\r\n",
/// );
///
/// assert_eq!("John Appleseed", contacts[0].name);
/// assert_eq!("+1 555 123 4567", contacts[0].phones[0].number);
/// assert_eq!("mobile", contacts[0].phones[0].label);
/// ```
pub fn parse_vcards(data: &[u8]) -> Vec<Contact> {
    let text = String::from_utf8_lossy(data);
    let mut contacts = Vec::new();

    let mut inside = false;
    let mut formatted = String::new();
    let mut structured: Vec<String> = Vec::new();
    let mut organization = String::new();
    let mut phones: Vec<Phone> = Vec::new();
    let mut emails: Vec<String> = Vec::new();

    for line in unfold(&text) {
        let Some(property) = Property::parse(&line) else {
            continue;
        };

        match property.name.as_str() {
            "BEGIN" if property.value.trim().eq_ignore_ascii_case("VCARD") => {
                inside = true;
                formatted.clear();
                structured.clear();
                organization.clear();
                phones.clear();
                emails.clear();
            }
            "END" if inside && property.value.trim().eq_ignore_ascii_case("VCARD") => {
                inside = false;
                if phones.is_empty() && emails.is_empty() {
                    continue;
                }

                let mut name = formatted.trim().to_string();
                if name.is_empty() {
                    // N is Family;Given;Additional;Prefix;Suffix.
                    let order = [3, 1, 2, 0, 4];
                    name = order
                        .iter()
                        .filter_map(|&i| structured.get(i))
                        .map(|part| part.trim())
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ");
                }
                if name.is_empty() {
                    name = organization.trim().to_string();
                }
                if name.is_empty() {
                    name = phones
                        .first()
                        .map(|phone| format_address(&phone.number))
                        .or_else(|| emails.first().cloned())
                        .unwrap_or_default();
                }

                contacts.push(Contact {
                    name,
                    phones: std::mem::take(&mut phones),
                    emails: std::mem::take(&mut emails),
                });
            }
            _ if !inside => {}
            "FN" => formatted = property.text(),
            "N" => structured = property.components(),
            "ORG" => organization = property.components().join(" "),
            "TEL" => {
                let number = property.text();
                let key = conversation_key(&number);
                let duplicate = phones
                    .iter()
                    .any(|phone| conversation_key(&phone.number) == key);
                if let (Some(label), false, false) =
                    (phone_label(&property.types()), number.is_empty(), duplicate)
                {
                    phones.push(Phone { number, label });
                }
            }
            "EMAIL" => {
                let email = property.text();
                if !email.is_empty() && !emails.contains(&email) {
                    emails.push(email);
                }
            }
            _ => {}
        }
    }

    contacts.sort_by_key(|contact| contact.name.to_lowercase());
    contacts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_match_spellings_of_a_number() {
        let key = conversation_key("+15551234567");
        assert_eq!(key, conversation_key("(555) 123-4567"));
        assert_eq!(key, conversation_key("1-555-123-4567"));
        assert_ne!(key, conversation_key("+15551234568"));
        assert_eq!("12345", conversation_key("12345"));
        assert_eq!("amazon", conversation_key("AMAZON"));
    }

    #[test]
    fn formats_north_american_numbers() {
        assert_eq!("+1 (555) 123-4567", format_address("+15551234567"));
        assert_eq!("(555) 123-4567", format_address("5551234567"));
        assert_eq!("+44 7700 900123", format_address("+44 7700 900123"));
        assert_eq!("ann@example.com", format_address("ann@example.com"));
    }

    #[test]
    fn initials_of_names_only() {
        assert_eq!(Some("JA".to_string()), initials("John Appleseed"));
        assert_eq!(Some("M".to_string()), initials("mom"));
        assert_eq!(None, initials("+1 (555) 123-4567"));
        assert_eq!(None, initials("ann@example.com"));
    }

    #[test]
    fn parses_vcard_21_with_quoted_printable() {
        let data = b"BEGIN:VCARD\r\nVERSION:2.1\r\n\
            N;CHARSET=UTF-8;ENCODING=QUOTED-PRINTABLE:M=C3=BCller;J=C3=\r\n=BCrgen\r\n\
            TEL;CELL:+49 170 1234567\r\nTEL;HOME;VOICE:+49 89 123456\r\n\
            TEL;FAX:+49 89 999\r\nEND:VCARD\r\n";
        let contacts = parse_vcards(data);

        assert_eq!(1, contacts.len());
        assert_eq!("Jürgen Müller", contacts[0].name);
        assert_eq!(2, contacts[0].phones.len());
        assert_eq!("mobile", contacts[0].phones[0].label);
        assert_eq!("home", contacts[0].phones[1].label);
    }

    #[test]
    fn parses_vcard_30_with_folding_groups_and_escapes() {
        let data = b"BEGIN:VCARD\nVERSION:3.0\nFN:Smith\\, Jane\nN:Smith;Jane;;;\n\
            item1.TEL;type=IPHONE;type=pref:+1 (555) 000-1111\nitem1.X-ABLabel:iPhone\n\
            EMAIL;type=INTERNET:jane@exa\n mple.com\nEND:VCARD\n\
            BEGIN:VCARD\nVERSION:3.0\nFN:No Numbers\nEND:VCARD\n\
            BEGIN:VCARD\nVERSION:3.0\nN:;;;;\nORG:Pizza Place;\nTEL:5550002222\nEND:VCARD\n";
        let contacts = parse_vcards(data);

        assert_eq!(2, contacts.len());
        assert_eq!("Pizza Place", contacts[0].name);
        assert_eq!("other", contacts[0].phones[0].label);
        assert_eq!("Smith, Jane", contacts[1].name);
        assert_eq!("iPhone", contacts[1].phones[0].label);
        assert_eq!(vec!["jane@example.com".to_string()], contacts[1].emails);
    }

    #[test]
    fn contacts_without_a_name_use_their_number() {
        let contacts = parse_vcards(
            b"BEGIN:VCARD\r\nVERSION:3.0\r\nN:;;;;\r\nFN:\r\nTEL:5551234567\r\nEND:VCARD\r\n",
        );
        assert_eq!("(555) 123-4567", contacts[0].name);
    }

    #[test]
    fn matches_names_and_numbers() {
        let contact = Contact {
            name: "Jane Smith".to_string(),
            phones: vec![Phone {
                number: "+1 (555) 000-1111".to_string(),
                label: "mobile".to_string(),
            }],
            emails: vec!["jane@example.com".to_string()],
        };

        assert!(contact.matches("jan"));
        assert!(contact.matches("5550001"));
        assert!(contact.matches("EXAMPLE"));
        assert!(!contact.matches("bob"));
    }
}
