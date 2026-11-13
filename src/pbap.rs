//! ## Phone Book Access
//!
//! A minimal Phone Book Access Profile client (PBAP 1.2, the PCE role), the way a
//! car kit downloads a phone's contacts. It runs on the [`obex`] crate: CONNECT to the
//! PBAP target, then PullPhoneBook (a GET of `telecom/pb.vcf`) in pages.
//!
//! The phone may ask its owner to allow contact access the first time; on an iPhone
//! this is the *Sync Contacts* switch in the Bluetooth settings of the computer.
//!
use obex::header::Header;
use obex::target::PHONE_BOOK_ACCESS;
use obex::tlv::ApplicationParameters;
use obex::tokio::Client;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::contacts::{parse_vcards, Contact};

/// The OBEX type of PullPhoneBook.
pub const PHONEBOOK_TYPE: &str = "x-bt/phonebook";

/// The main phone book of the phone's memory.
pub const MAIN_PHONEBOOK: &str = "telecom/pb.vcf";

/// Application Parameters tags (PBAP section 6.2.1).
pub mod tag {
    /// `MaxListCount`, 2 bytes.
    pub const MAX_LIST_COUNT: u8 = 0x04;
    /// `ListStartOffset`, 2 bytes.
    pub const LIST_START_OFFSET: u8 = 0x05;
    /// `PropertySelector`, 8 bytes.
    pub const PROPERTY_SELECTOR: u8 = 0x06;
    /// `Format`, 1 byte: 0 for vCard 2.1, 1 for vCard 3.0.
    pub const FORMAT: u8 = 0x07;
    /// `PhonebookSize`, 2 bytes.
    pub const PHONEBOOK_SIZE: u8 = 0x08;
}

/// The vCard properties Ferry needs: VERSION, FN, N, TEL, EMAIL and ORG. Leaving out
/// PHOTO in particular keeps the download small.
pub const PROPERTIES: u64 = 1 | 1 << 1 | 1 << 2 | 1 << 7 | 1 << 8 | 1 << 16;

/// The PHOTO property, requested separately as photos make the download much larger.
pub const PHOTO: u64 = 1 << 3;

/// How many contacts to request at a time.
const PAGE: u16 = 250;

/// A PBAP client over a connected transport.
pub struct PbapClient<T> {
    obex: Client<T>,
}

impl<T: AsyncRead + AsyncWrite + Unpin> PbapClient<T> {
    /// Creates a client. Over an L2CAP link pass its MTU, which enables Single
    /// Response Mode (GOEP 2.0).
    pub fn new(transport: T, packet_mtu: Option<u16>) -> PbapClient<T> {
        let mut obex = Client::new(transport);
        if let Some(mtu) = packet_mtu {
            obex = obex
                .with_max_packet_length(mtu)
                .with_single_response_mode(true);
        }
        PbapClient { obex }
    }

    /// Opens the PBAP session.
    pub async fn connect(&mut self) -> obex::Result<()> {
        self.obex
            .connect([Header::Target(PHONE_BOOK_ACCESS.to_vec())])
            .await?;
        Ok(())
    }

    /// Closes the session.
    pub async fn disconnect(&mut self) -> obex::Result<()> {
        self.obex.disconnect().await?;
        Ok(())
    }

    async fn pull(
        &mut self,
        parameters: ApplicationParameters,
    ) -> obex::Result<obex::client::GetResponse> {
        self.obex
            .get([
                Header::Name(MAIN_PHONEBOOK.to_string()),
                Header::Type(PHONEBOOK_TYPE.to_string()),
                Header::ApplicationParameters(parameters),
            ])
            .await
    }

    /// The number of entries in the phone book, if the phone reports it.
    pub async fn phonebook_size(&mut self) -> obex::Result<Option<u16>> {
        let response = self
            .pull(ApplicationParameters::new().with_u16(tag::MAX_LIST_COUNT, 0))
            .await?;
        Ok(response
            .headers
            .application_parameters()
            .and_then(|parameters| parameters.get_u16(tag::PHONEBOOK_SIZE)))
    }

    /// Downloads up to `count` vCards from position `offset`, with the properties in
    /// `properties` (a PBAP PropertySelector).
    pub async fn pull_phonebook(
        &mut self,
        offset: u16,
        count: u16,
        properties: u64,
    ) -> obex::Result<Vec<u8>> {
        let parameters = ApplicationParameters::new()
            .with_u8(tag::FORMAT, 1)
            .with_u64(tag::PROPERTY_SELECTOR, properties)
            .with_u16(tag::MAX_LIST_COUNT, count)
            .with_u16(tag::LIST_START_OFFSET, offset);
        Ok(self.pull(parameters).await?.body)
    }

    /// Downloads every contact that can be texted, with their photos if `photos`.
    pub async fn contacts(&mut self, photos: bool) -> obex::Result<Vec<Contact>> {
        let properties = if photos {
            PROPERTIES | PHOTO
        } else {
            PROPERTIES
        };
        let size = self.phonebook_size().await.ok().flatten();
        let mut data = Vec::new();

        match size {
            Some(size) => {
                let mut offset = 0;
                while offset < size {
                    let page = self
                        .pull_phonebook(offset, PAGE.min(size - offset), properties)
                        .await?;
                    let cards = count_vcards(&page);
                    data.extend_from_slice(&page);
                    data.extend_from_slice(b"\r\n");
                    if cards == 0 {
                        break;
                    }
                    offset = offset.saturating_add(cards);
                }
            }
            None => data = self.pull_phonebook(0, u16::MAX, properties).await?,
        }

        Ok(parse_vcards(&data))
    }
}

fn count_vcards(data: &[u8]) -> u16 {
    let count = data
        .windows(b"BEGIN:VCARD".len())
        .filter(|window| window.eq_ignore_ascii_case(b"BEGIN:VCARD"))
        .count();
    u16::try_from(count).unwrap_or(u16::MAX)
}
