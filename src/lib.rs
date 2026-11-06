//! `k-pdf`: a small, low-memory PDF library.
//!
//! Reading a document:
//!
//! ```no_run
//! use k_pdf::{Document, TextExtractor};
//!
//! let doc = Document::open("file.pdf")?;
//! println!("PDF {} with {} pages", doc.version(), doc.page_count()?);
//!
//! let mut text = TextExtractor::new(&doc);
//! for page in doc.pages()? {
//!     println!("{}", text.extract_page(&page)?);
//! }
//! # Ok::<(), k_pdf::Error>(())
//! ```

mod ccitt;
#[cfg(feature = "compress")]
pub mod compress;
mod document;
pub mod edit;
mod encoding;
mod error;
mod filters;
mod forms;
mod layout;
mod links;
mod font;
mod object;
mod parser;
#[cfg(feature = "render")]
mod render;
mod std14;
mod text;
mod writer;
mod xref;

pub use document::{Document, Metadata, Page};
pub use encoding::decode_text_string;
pub use error::{Error, Result};
pub use filters::Decoded;
pub use layout::reading_order;
pub use links::{page_links, Link, LinkTarget};
pub use object::{Dictionary, ObjRef, Object, Stream};
pub use text::{chars_to_string, TextChar, TextExtractor};
pub use forms::{fill_form, form_fields, ChoiceOption, FieldKind, FieldValue, FormField, Widget};
pub use writer::{write_document, write_document_with_changes, write_document_with_options, write_with_page_order, Changes, WriteOptions};
#[cfg(feature = "render")]
pub use render::{RenderedPage, Renderer};
